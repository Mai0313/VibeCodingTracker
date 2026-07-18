//! Shared cached-scan machinery for the usage and analysis roll-ups.
//!
//! Both features discover the same session files, look each one up in the same
//! incremental [`SummaryScanCache`], parse misses into the same
//! [`CompactSourceSummary`], and record the same failures. The only per-feature
//! part is where a parsed summary is folded, expressed by [`CompactSink`]. Usage
//! also threads a per-request tier snapshot; analysis passes `None`.

use super::ScanDiagnostics;
use crate::cli::TimeRange;
use crate::constants::FastHashSet;
use crate::models::ExtensionType;
use crate::pricing::TierThresholds;
use crate::session::ParseMode;
use crate::session::diagnostics::partial_failure_reason;
use crate::session::parser::parse_session_file_as_with_diagnostics;
use crate::summary_cache::{
    CachedSourceSummary, CompactSourceSummary, SourceFingerprint, SummaryCacheKey, SummaryKind,
    SummaryScanCache,
};
use crate::utils::directory::{FileInfo, collect_files_with_max_depth_diagnostics};
use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

/// A freshly loaded compact source summary plus its parse verdict.
pub(crate) struct LoadedCompactSummary {
    pub(crate) summary: CompactSourceSummary,
    pub(crate) parsed: bool,
    pub(crate) failure: Option<String>,
}

/// The per-feature fold target: usage accumulates token maps, analysis
/// accumulates file-operation rows. The shared scanners only need this one hook.
pub(crate) trait CompactSink {
    fn fold(&mut self, provider: ExtensionType, summary: &CompactSourceSummary);
}

/// Parses one session file into a compact summary in `UsageOnly` mode.
///
/// The only feature-specific input is the optional per-request tier snapshot
/// (usage passes it, analysis passes `None`).
pub(crate) fn load_compact_file_summary(
    file: &FileInfo,
    provider: ExtensionType,
    tiers: Option<&TierThresholds>,
) -> Result<LoadedCompactSummary> {
    let parsed =
        parse_session_file_as_with_diagnostics(&file.path, provider, ParseMode::UsageOnly, tiers)?;
    if parsed.diagnostics.is_complete_failure() {
        let failure = if parsed.diagnostics.recognized_records == 0 {
            "source contained no recognized provider records".to_string()
        } else {
            format!(
                "none of {} analyzer-relevant provider records used a supported schema",
                parsed.diagnostics.relevant_records
            )
        };
        return Ok(LoadedCompactSummary {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some(failure),
        });
    }

    let emit = parsed.diagnostics.should_emit_session();
    if emit && parsed.analysis.records.is_empty() {
        return Ok(LoadedCompactSummary {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some("normalized source produced no analysis records".to_string()),
        });
    }
    let partial = parsed.diagnostics.partial_failure_count();
    Ok(LoadedCompactSummary {
        summary: CompactSourceSummary::from_file(parsed.analysis, file.modified_date.clone(), emit),
        parsed: true,
        failure: (partial > 0).then(|| partial_failure_reason(partial)),
    })
}

/// Folds a cached source summary into `sink`, recording any retained failure.
pub(crate) fn fold_cached(
    provider: ExtensionType,
    source: &Path,
    cached: &CachedSourceSummary,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) {
    if cached.parsed {
        diagnostics.parsed += 1;
        sink.fold(provider, &cached.summary);
    }
    if let Some(error) = &cached.failure {
        diagnostics.record_failure(provider, source, error.clone());
    }
}

/// Folds a freshly loaded source summary into `sink`, recording any failure.
pub(crate) fn fold_loaded(
    provider: ExtensionType,
    source: &Path,
    loaded: &LoadedCompactSummary,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) {
    if loaded.parsed {
        diagnostics.parsed += 1;
        sink.fold(provider, &loaded.summary);
    }
    if let Some(error) = &loaded.failure {
        diagnostics.record_failure(provider, source, error.clone());
    }
}

/// Scans one file-backed provider directory through the incremental cache.
///
/// Shared verbatim by usage and analysis: discovery, cache lookup, parallel
/// miss-load, cache insert, and fold. Each feature supplies only its `sink`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_cached_files<F>(
    dir: &Path,
    provider: ExtensionType,
    filter: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    cache: &mut SummaryScanCache,
    seen: &mut FastHashSet<SummaryCacheKey>,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    tiers: Option<&TierThresholds>,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let discovery = collect_files_with_max_depth_diagnostics(dir, filter, time_range, max_depth);
    if !discovery.failures.is_empty() {
        cache.preserve_provider_keys(seen, SummaryKind::File, provider);
    }
    diagnostics.candidates += discovery.failures.len();
    for failure in discovery.failures {
        diagnostics.record_failure(provider, &failure.path, failure.error);
    }

    let mut files = discovery.files;
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    diagnostics.candidates += files.len();

    let mut misses = Vec::new();
    for file in files {
        let key = SummaryCacheKey::new(SummaryKind::File, provider, &file.path, time_range);
        seen.insert(key.clone());
        match SourceFingerprint::file(&file.path, provider) {
            Ok(fingerprint) => {
                if let Some(cached) = cache.get(&key, &fingerprint) {
                    fold_cached(provider, &file.path, cached, sink, diagnostics);
                } else {
                    misses.push((file, key, fingerprint));
                }
            }
            Err(error) => diagnostics.record_failure(provider, &file.path, error.to_string()),
        }
    }

    let loaded: Vec<_> = misses
        .into_par_iter()
        .map(|(file, key, fingerprint)| {
            let result = load_compact_file_summary(&file, provider, tiers);
            (file.path, key, fingerprint, result)
        })
        .collect();

    for (source, key, fingerprint, result) in loaded {
        cache.record_parse();
        match result {
            Ok(loaded) => {
                fold_loaded(provider, &source, &loaded, sink, diagnostics);
                cache.insert(
                    key,
                    fingerprint,
                    loaded.summary,
                    loaded.parsed,
                    loaded.failure,
                );
            }
            Err(error) => diagnostics.record_failure(provider, &source, error.to_string()),
        }
    }
    Ok(())
}
