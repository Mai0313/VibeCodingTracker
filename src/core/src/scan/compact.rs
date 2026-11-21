//! Shared ledger-scan machinery for file-backed providers.
//!
//! Discovers session files, checks and updates [`SessionLedger`] entries per [`ScanFeature`], parses misses into [`SessionEntry`], and folds matching days into a [`CompactSink`].

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

/// Fold target receiving per-day ledger entries during a scan.
pub(crate) trait CompactSink {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry);

    fn folds_days(&self) -> bool {
        true
    }
}

/// No-op [`CompactSink`] for ledger-only refreshes that skip in-memory folding.
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

/// Returns the inclusive `YYYY-MM-DD` lower bound for `time_range`, or `None` when unbounded.
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

/// Folds an on-disk session from `entry` and records its cached `feature` verdict in `diagnostics`.
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

/// Folds every session in `book` absent from `seen` and returns how many non-empty entries were retained.
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

/// Folds remote-machine sessions stored in `ledger` for every provider passing `enabled`.
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

/// Parses one session file in [`ParseMode::UsageOnly`] into a [`SessionEntry`] stamped with `stamp` and its parse verdict.
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

/// Result of scanning one file-backed provider's roots against its ledger book.
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

/// Discovers, stamps, and parses stale files for one provider, returning an updated ledger book and diagnostics.
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
            if let Some(entry) = book.sessions.get_mut(&key)
                && entry_matches_target(entry, target, &file, tiers_fingerprint)
            {
                let stamp = entry.scanned.half(verdict_feature);
                if stamp.is_none_or(|stamp| stamp.parsed) {
                    parsed += 1;
                }
                if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
                    retained_failures.push((file.path.clone(), failure.clone()));
                }
                if entry.missing_since.take().is_some() {
                    book.dirty = true;
                }
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
            match book.sessions.get_mut(&key) {
                Some(entry) if entry_matches_stamp(entry, target, &current) => {
                    let stamp = entry.scanned.half(verdict_feature);
                    if stamp.is_none_or(|stamp| stamp.parsed) {
                        parsed += 1;
                    }
                    if let Some(failure) = stamp.and_then(|stamp| stamp.failure.as_ref()) {
                        retained_failures.push((file.path, failure.clone()));
                    }
                    if entry.missing_since.take().is_some() {
                        book.dirty = true;
                    }
                    seen.insert(key);
                }
                _ => {
                    seen.insert(key.clone());
                    misses.push((file, key, current));
                }
            }
        }
    }

    // LPT scheduling: dispatch largest files first so huge logs do not stall the parallel tail.
    let reordered_misses = misses.len() > 1;
    if reordered_misses {
        misses.sort_unstable_by_key(|left| std::cmp::Reverse(left.0.len));
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
    if reordered_misses {
        if hard_failures.len() > 1 {
            hard_failures.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        }
        if retained_failures.len() > 1 {
            retained_failures.sort_unstable_by(|left, right| left.0.cmp(&right.0));
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

/// Merges a [`ProviderFileScanOutcome`] into `ledger`, `sink`, and `diagnostics`.
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
