//! Shared ledger-scan machinery for the usage and analysis roll-ups.
//!
//! Both features discover the same session files, look each one up in the same
//! [`SessionLedger`], parse misses into the same [`SessionEntry`], fold the
//! same days, and record the same failures. The only per-feature parts are
//! which half of an entry a scan checks and writes ([`ScanFeature`]) and where
//! a folded day lands ([`CompactSink`]). Usage also threads a per-request tier
//! snapshot; analysis passes `None`.

use super::{ScanDiagnostics, ScanFeature};
use crate::constants::FastHashSet;
use crate::ledger::key::session_key;
use crate::ledger::{
    DayEntry, ProviderLedger, ScanStamp, SessionEntry, SessionLedger, TinyMap, stamp,
};
use crate::models::ExtensionType;
use crate::models::TimeRange;
use crate::pricing::TierThresholds;
use crate::session::ParseMode;
use crate::session::diagnostics::partial_failure_reason;
use crate::session::parser::parse_session_file_typed_as_with_diagnostics;
use crate::utils::directory::{DiscoveredFile, collect_provider_candidates_diagnostics};
use anyhow::Result;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// The per-feature fold target: usage accumulates token maps, analysis
/// accumulates file-operation rows. The shared scanners only need this one hook.
pub(crate) trait CompactSink {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry);

    fn folds_days(&self) -> bool {
        true
    }
}

/// Sink for ledger-only refreshes (`vct ledger export`) that need no in-memory
/// roll-up.
pub(crate) struct NoopSink;

impl CompactSink for NoopSink {
    fn fold(&mut self, _provider: ExtensionType, _date: &str, _day: &DayEntry) {}

    fn folds_days(&self) -> bool {
        false
    }
}

/// Which half (or both) a file scan requires to be current in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanTarget {
    Single(ScanFeature),
    BothHalves,
}

/// The inclusive `YYYY-MM-DD` lower bound a scan folds from; `None` folds
/// every day.
pub(crate) fn cutoff_string(time_range: TimeRange) -> Option<String> {
    time_range
        .cutoff_date()
        .map(|date| date.format("%Y-%m-%d").to_string())
}

/// Folds every day of `days` on or after `cutoff`.
pub(crate) fn fold_days(
    provider: ExtensionType,
    days: &TinyMap<String, DayEntry>,
    cutoff: Option<&str>,
    sink: &mut impl CompactSink,
) {
    if !sink.folds_days() {
        return;
    }
    for (date, day) in days {
        if cutoff.is_none_or(|cutoff| date.as_str() >= cutoff) {
            sink.fold(provider, date, day);
        }
    }
}

/// Folds a session that is present on disk and reports the verdict its
/// `feature` half retained from the read that produced it.
pub(crate) fn fold_present(
    provider: ExtensionType,
    source: &Path,
    entry: &SessionEntry,
    feature: ScanFeature,
    cutoff: Option<&str>,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) {
    let stamp = entry.scanned.half(feature);
    if stamp.is_none_or(|stamp| stamp.parsed) {
        diagnostics.parsed += 1;
        fold_days(provider, &entry.days, cutoff, sink);
    }
    if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
        diagnostics.record_failure(provider, source, failure.clone());
    }
}

/// Folds every session of `book` the scan did not find on disk, and returns
/// how many of them held anything.
pub(crate) fn fold_unseen(
    provider: ExtensionType,
    book: &ProviderLedger,
    seen: &FastHashSet<String>,
    cutoff: Option<&str>,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) -> usize {
    let mut retained = 0;
    for (key, entry) in &book.sessions {
        if seen.contains(key) {
            continue;
        }
        fold_days(provider, &entry.days, cutoff, sink);
        if !entry.days.is_empty() {
            retained += 1;
        }
    }
    diagnostics.retained += retained;
    retained
}

/// Folds the sessions of other machines that `ledger` holds snapshots of, for
/// every provider `enabled` lets through.
pub(crate) fn fold_remotes(
    ledger: &SessionLedger,
    enabled: impl Fn(ExtensionType) -> bool,
    time_range: TimeRange,
    sink: &mut impl CompactSink,
) {
    let cutoff = cutoff_string(time_range);
    for provider in crate::ledger::ALL_PROVIDERS {
        if !enabled(provider) {
            continue;
        }
        for entry in ledger.remote_sessions(provider) {
            fold_days(provider, &entry.days, cutoff.as_deref(), sink);
        }
    }
}

/// Parses one session log in `UsageOnly` mode into its ledger entry, stamped
/// with `stamp` and the read's verdict.
///
/// The only feature-specific input is the optional per-request tier snapshot
/// (usage passes it, analysis passes `None`).
pub(crate) fn load_file_session(
    file: &DiscoveredFile,
    provider: ExtensionType,
    tiers: Option<&TierThresholds>,
    stamp: ScanStamp,
) -> Result<SessionEntry> {
    let parsed = parse_session_file_typed_as_with_diagnostics(
        &file.path,
        provider,
        ParseMode::UsageOnly,
        tiers,
    )?;
    let date = file.modified_date();
    if parsed.diagnostics.is_complete_failure() {
        let failure = if parsed.diagnostics.recognized_records == 0 {
            "source contained no recognized provider records".to_string()
        } else {
            format!(
                "none of {} analyzer-relevant provider records used a supported schema",
                parsed.diagnostics.relevant_records
            )
        };
        return Ok(SessionEntry::from_file_parse(
            None,
            false,
            date,
            stamp.with_verdict(false, Some(failure)),
        ));
    }

    let emit = parsed.diagnostics.should_emit_session();
    if emit && parsed.analysis.records.is_empty() {
        return Ok(SessionEntry::from_file_parse(
            None,
            false,
            date,
            stamp.with_verdict(
                false,
                Some("normalized source produced no analysis records".to_string()),
            ),
        ));
    }
    let partial = parsed.diagnostics.partial_failure_count();
    let stamp = stamp.with_verdict(true, (partial > 0).then(|| partial_failure_reason(partial)));
    Ok(SessionEntry::from_file_parse(
        Some(parsed.analysis),
        emit,
        date,
        stamp,
    ))
}

pub(crate) struct ProviderFileScanOutcome {
    pub(crate) provider: ExtensionType,
    pub(crate) book: ProviderLedger,
    pub(crate) seen: FastHashSet<String>,
    pub(crate) failed_keys: FastHashSet<String>,
    pub(crate) candidates: usize,
    pub(crate) parsed: usize,
    pub(crate) parsed_sources: usize,
    pub(crate) hard_failures: Vec<(PathBuf, String)>,
    pub(crate) retained_failures: Vec<(PathBuf, String)>,
}

fn entry_matches_target(
    entry: &SessionEntry,
    target: ScanTarget,
    file: &DiscoveredFile,
    tiers_fingerprint: Option<&str>,
) -> bool {
    match target {
        ScanTarget::Single(feature) => {
            entry
                .scanned
                .is_current_single_file(feature, file, tiers_fingerprint)
        }
        ScanTarget::BothHalves => {
            entry
                .scanned
                .is_current_single_file(ScanFeature::Usage, file, tiers_fingerprint)
                && entry
                    .scanned
                    .is_current_single_file(ScanFeature::Analysis, file, None)
        }
    }
}

fn entry_matches_stamp(entry: &SessionEntry, target: ScanTarget, current: &ScanStamp) -> bool {
    match target {
        ScanTarget::Single(feature) => entry.scanned.is_current(feature, current),
        ScanTarget::BothHalves => {
            entry.scanned.is_current(ScanFeature::Usage, current)
                && entry.scanned.is_current(ScanFeature::Analysis, current)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_provider_files<F>(
    dirs: &[&Path],
    provider: ExtensionType,
    filter: F,
    max_depth: Option<usize>,
    mut book: ProviderLedger,
    target: ScanTarget,
    tiers: Option<&TierThresholds>,
    tiers_fingerprint: Option<&str>,
    today_str: &str,
) -> ProviderFileScanOutcome
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let discovery = collect_provider_candidates_diagnostics(dirs, provider, filter, max_depth);
    let partial = !discovery.failures.is_empty();
    let mut candidates = discovery.failures.len();
    let mut hard_failures: Vec<(PathBuf, String)> = discovery
        .failures
        .into_iter()
        .map(|failure| (failure.path, failure.error))
        .collect();

    let mut files = discovery.files;
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    candidates += files.len();

    let verdict_feature = match target {
        ScanTarget::Single(feature) => feature,
        ScanTarget::BothHalves => ScanFeature::Usage,
    };
    let single_file = !stamp::has_sidecars(provider);
    let mut seen = FastHashSet::with_capacity_and_hasher(files.len(), Default::default());
    let mut failed_keys = FastHashSet::default();
    let mut retained_failures = Vec::new();
    let mut parsed = 0usize;
    let mut misses = Vec::new();

    for file in files {
        let key = session_key(provider, dirs, &file.path);
        if single_file {
            if let Some(entry) = book.sessions.get(&key)
                && entry_matches_target(entry, target, &file, tiers_fingerprint)
            {
                let stamp = entry.scanned.half(verdict_feature);
                if stamp.is_none_or(|stamp| stamp.parsed) {
                    parsed += 1;
                }
                if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
                    retained_failures.push((file.path.clone(), failure.clone()));
                }
                book.mark_present(&key);
                seen.insert(key);
                continue;
            }
            match stamp::file_source(&file, provider) {
                Ok(source_files) => {
                    let current =
                        ScanStamp::new(tiers_fingerprint.map(str::to_string), source_files);
                    seen.insert(key.clone());
                    misses.push((file, key, current));
                }
                Err(error) => {
                    hard_failures.push((file.path, error.to_string()));
                    failed_keys.insert(key.clone());
                    seen.insert(key);
                }
            }
        } else {
            let current = match stamp::file_source(&file, provider) {
                Ok(source_files) => {
                    ScanStamp::new(tiers_fingerprint.map(str::to_string), source_files)
                }
                Err(error) => {
                    hard_failures.push((file.path, error.to_string()));
                    failed_keys.insert(key.clone());
                    seen.insert(key);
                    continue;
                }
            };
            match book.sessions.get(&key) {
                Some(entry) if entry_matches_stamp(entry, target, &current) => {
                    let stamp = entry.scanned.half(verdict_feature);
                    if stamp.is_none_or(|stamp| stamp.parsed) {
                        parsed += 1;
                    }
                    if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
                        retained_failures.push((file.path, failure.clone()));
                    }
                    book.mark_present(&key);
                    seen.insert(key);
                }
                _ => {
                    seen.insert(key.clone());
                    misses.push((file, key, current));
                }
            }
        }
    }

    let loaded: Vec<_> = misses
        .into_par_iter()
        .map(|(file, key, current)| {
            let result = load_file_session(&file, provider, tiers, current);
            (file.path, key, result)
        })
        .collect();
    let parsed_sources = loaded.len();
    for (source, key, result) in loaded {
        match result {
            Ok(entry) => {
                let stamp = entry.scanned.half(verdict_feature);
                if stamp.is_none_or(|stamp| stamp.parsed) {
                    parsed += 1;
                }
                if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
                    retained_failures.push((source, failure.clone()));
                }
                book.sessions.insert(key, entry);
                book.dirty = true;
            }
            Err(error) => {
                hard_failures.push((source, error.to_string()));
                failed_keys.insert(key);
            }
        }
    }

    book.settle_unseen(&seen, !partial, today_str);
    ProviderFileScanOutcome {
        provider,
        book,
        seen,
        failed_keys,
        candidates,
        parsed,
        parsed_sources,
        hard_failures,
        retained_failures,
    }
}

pub(crate) fn finish_provider_scan(
    outcome: ProviderFileScanOutcome,
    target: ScanTarget,
    cutoff: Option<&str>,
    ledger: &mut SessionLedger,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) {
    let provider = outcome.provider;
    diagnostics.candidates += outcome.candidates;
    diagnostics.parsed += outcome.parsed;
    for (source, error) in outcome.hard_failures {
        diagnostics.record_hard_failure(provider, &source, error);
    }
    for (source, error) in outcome.retained_failures {
        diagnostics.record_failure(provider, &source, error);
    }
    ledger.record_parses(outcome.parsed_sources);

    let verdict_feature = match target {
        ScanTarget::Single(feature) => feature,
        ScanTarget::BothHalves => ScanFeature::Usage,
    };
    let fold = sink.folds_days();
    let mut retained = 0;
    for (key, entry) in &outcome.book.sessions {
        if outcome.seen.contains(key) {
            if fold
                && !outcome.failed_keys.contains(key)
                && entry
                    .scanned
                    .half(verdict_feature)
                    .is_none_or(|stamp| stamp.parsed)
            {
                fold_days(provider, &entry.days, cutoff, sink);
            }
        } else {
            if fold {
                fold_days(provider, &entry.days, cutoff, sink);
            }
            if !entry.days.is_empty() {
                retained += 1;
            }
        }
    }
    diagnostics.retained += retained;
    ledger.record_retained(retained);
    ledger.put_provider(provider, outcome.book);
}
