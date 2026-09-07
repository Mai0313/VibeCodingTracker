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
    DayEntry, ProviderLedger, ScanStamp, SessionEntry, SessionLedger, stamp, today,
};
use crate::models::ExtensionType;
use crate::models::TimeRange;
use crate::pricing::TierThresholds;
use crate::session::ParseMode;
use crate::session::diagnostics::partial_failure_reason;
use crate::session::parser::parse_session_file_typed_as_with_diagnostics;
use crate::utils::directory::{FileInfo, collect_provider_files_diagnostics};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

/// The per-feature fold target: usage accumulates token maps, analysis
/// accumulates file-operation rows. The shared scanners only need this one hook.
pub(crate) trait CompactSink {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry);
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
    days: &BTreeMap<String, DayEntry>,
    cutoff: Option<&str>,
    sink: &mut impl CompactSink,
) {
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
/// how many there were.
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
        retained += 1;
    }
    diagnostics.retained += retained;
    retained
}

/// Parses one session log in `UsageOnly` mode into its ledger entry, stamped
/// with `stamp` and the read's verdict.
///
/// The only feature-specific input is the optional per-request tier snapshot
/// (usage passes it, analysis passes `None`).
pub(crate) fn load_file_session(
    file: &FileInfo,
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
    let date = file.modified_date.clone();
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

/// Scans one file-backed provider's session roots through the ledger.
///
/// Shared verbatim by usage and analysis: discovery of every log under the
/// roots (the time range is applied to the folded days, never to discovery,
/// so an older session is never mistaken for a deleted one), stamp check,
/// parallel miss-parse, ledger update, fold, and finally the fold of every
/// session the roots no longer hold.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_cached_files<F>(
    dirs: &[&Path],
    provider: ExtensionType,
    filter: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    ledger: &mut SessionLedger,
    feature: ScanFeature,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    tiers: Option<&TierThresholds>,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let discovery = collect_provider_files_diagnostics(dirs, filter, TimeRange::All, max_depth);
    let partial = !discovery.failures.is_empty();
    diagnostics.candidates += discovery.failures.len();
    for failure in discovery.failures {
        diagnostics.record_hard_failure(provider, &failure.path, failure.error);
    }

    let mut files = discovery.files;
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    diagnostics.candidates += files.len();

    let cutoff = cutoff_string(time_range);
    let tiers_fingerprint = stamp::tiers_fingerprint(tiers);
    let mut book = ledger.take_provider(provider);
    let mut seen = FastHashSet::default();
    let mut misses = Vec::new();
    for file in files {
        let key = session_key(provider, dirs, &file.path);
        seen.insert(key.clone());
        let current = match stamp::file_source(&file, provider) {
            Ok(files) => ScanStamp::new(tiers_fingerprint.clone(), files),
            Err(error) => {
                diagnostics.record_hard_failure(provider, &file.path, error.to_string());
                continue;
            }
        };
        match book.sessions.get(&key) {
            Some(entry) if entry.scanned.is_current(feature, &current) => {
                fold_present(
                    provider,
                    &file.path,
                    entry,
                    feature,
                    cutoff.as_deref(),
                    sink,
                    diagnostics,
                );
                book.mark_present(&key);
            }
            _ => misses.push((file, key, current)),
        }
    }

    let loaded: Vec<_> = misses
        .into_par_iter()
        .map(|(file, key, current)| {
            let result = load_file_session(&file, provider, tiers, current);
            (file.path, key, result)
        })
        .collect();
    ledger.record_parses(loaded.len());
    for (source, key, result) in loaded {
        match result {
            Ok(entry) => {
                fold_present(
                    provider,
                    &source,
                    &entry,
                    feature,
                    cutoff.as_deref(),
                    sink,
                    diagnostics,
                );
                book.sessions.insert(key, entry);
                book.dirty = true;
            }
            Err(error) => diagnostics.record_hard_failure(provider, &source, error.to_string()),
        }
    }

    book.settle_unseen(&seen, !partial, &today());
    let retained = fold_unseen(provider, &book, &seen, cutoff.as_deref(), sink, diagnostics);
    ledger.record_retained(retained);
    ledger.put_provider(provider, book);
    Ok(())
}
