//! Process-local cache for compact usage and analysis scan contributions.

use crate::cli::TimeRange;
use crate::models::{CodeAnalysis, ExtensionType};
use crate::session::diagnostics::{
    AnalysisFact, AnalysisMetrics, ParsedAnalysis, PricingGranularity, ToolFactStatus,
    UsageContribution, UsageFact, UsageFactUnit,
};
use crate::session::sqlite::{DatabaseFingerprint, append_suffix};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Builds the dedicated Rayon pool used by CLI scans.
pub fn build_scan_pool(threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|index| format!("vct-scan-{index}"))
        .build()
        .map_err(Into::into)
}

/// Stable provider order shared by cached usage and analysis diagnostics.
pub(crate) fn provider_scan_rank(provider: ExtensionType) -> u8 {
    match provider {
        ExtensionType::ClaudeCode => 0,
        ExtensionType::Codex => 1,
        ExtensionType::Copilot => 2,
        ExtensionType::Gemini => 3,
        ExtensionType::Grok => 4,
        ExtensionType::OpenCode => 5,
        ExtensionType::Cursor => 6,
        ExtensionType::Hermes => 7,
    }
}

/// Observable cache statistics used by tests and diagnostic logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummaryScanCacheStats {
    /// Compact source entries currently retained.
    pub entries: usize,
    /// Sources parsed during the most recent scan cycle.
    pub parsed_sources: usize,
    /// Sources parsed since this cache was created.
    pub total_parsed_sources: usize,
}

/// Compact, process-local cache reused by a TUI refresh worker.
///
/// It never stores raw JSON or complete [`CodeAnalysis`] values. File-backed
/// entries retain only model usage, operation counters, dates, and a compact
/// diagnostic summary. Database readers use the same container for their
/// already-aggregated rows.
#[derive(Default)]
pub struct SummaryScanCache {
    entries: HashMap<SummaryCacheKey, CachedSourceSummary>,
    parsed_sources: usize,
    total_parsed_sources: usize,
}

impl SummaryScanCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts one refresh cycle and resets its parse counter.
    pub(crate) fn begin_scan(&mut self) {
        self.parsed_sources = 0;
    }

    /// Records that a source loader ran during this refresh.
    pub(crate) fn record_parse(&mut self) {
        self.parsed_sources += 1;
        self.total_parsed_sources += 1;
    }

    /// Returns current and cumulative cache statistics.
    pub fn stats(&self) -> SummaryScanCacheStats {
        SummaryScanCacheStats {
            entries: self.entries.len(),
            parsed_sources: self.parsed_sources,
            total_parsed_sources: self.total_parsed_sources,
        }
    }

    pub(crate) fn get(
        &self,
        key: &SummaryCacheKey,
        fingerprint: &SourceFingerprint,
    ) -> Option<&CachedSourceSummary> {
        self.entries
            .get(key)
            .filter(|entry| &entry.fingerprint == fingerprint)
    }

    pub(crate) fn insert(
        &mut self,
        key: SummaryCacheKey,
        fingerprint: SourceFingerprint,
        summary: CompactSourceSummary,
        parsed: bool,
        failure: Option<String>,
    ) {
        self.entries.insert(
            key,
            CachedSourceSummary {
                fingerprint,
                summary,
                parsed,
                failure,
            },
        );
    }

    pub(crate) fn retain_kinds(&mut self, seen: &HashSet<SummaryCacheKey>, kinds: &[SummaryKind]) {
        self.entries
            .retain(|key, _| !kinds.contains(&key.kind) || seen.contains(key));
    }

    /// Keeps existing entries for a provider when source discovery was partial.
    pub(crate) fn preserve_provider_keys(
        &self,
        seen: &mut HashSet<SummaryCacheKey>,
        kind: SummaryKind,
        provider: ExtensionType,
    ) {
        let provider = provider.to_string();
        seen.extend(
            self.entries
                .keys()
                .filter(|key| key.kind == kind && key.provider == provider)
                .cloned(),
        );
    }
}

/// Which compact projection a database-backed cache entry contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SummaryKind {
    File,
    UsageDatabase,
    AnalysisDatabase,
}

/// Stable source identity. Time-range filtering is applied when compact facts
/// are folded, so one parsed source is reusable across every range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SummaryCacheKey {
    kind: SummaryKind,
    provider: String,
    path: PathBuf,
}

impl SummaryCacheKey {
    pub(crate) fn new(
        kind: SummaryKind,
        provider: ExtensionType,
        path: &Path,
        _time_range: TimeRange,
    ) -> Self {
        Self {
            kind,
            provider: provider.to_string(),
            path: path.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
}

/// Fingerprint of one source and every sidecar that changes its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceFingerprint(Vec<(PathBuf, Option<FileStamp>)>);

impl SourceFingerprint {
    /// Fingerprints a file source and provider-specific dependencies.
    pub(crate) fn file(path: &Path, provider: ExtensionType) -> Result<Self> {
        let mut paths = vec![(path.to_path_buf(), Some(required_stamp(path)?))];
        if provider == ExtensionType::Grok {
            paths.push((
                path.with_file_name("summary.json"),
                optional_stamp(&path.with_file_name("summary.json"))?,
            ));
            paths.push((
                path.with_file_name("updates.jsonl"),
                optional_stamp(&path.with_file_name("updates.jsonl"))?,
            ));
            if let Some(workspace) = path.parent().and_then(Path::parent) {
                let cwd = workspace.join(".cwd");
                paths.push((cwd.clone(), optional_stamp(&cwd)?));
            }
        }
        Ok(Self(paths))
    }

    /// Fingerprints a SQLite database, its WAL, and optional extra databases.
    pub(crate) fn sqlite(path: &Path, extras: &[&Path]) -> Result<Self> {
        let mut paths = Vec::with_capacity(2 + extras.len() * 2);
        push_sqlite_stamps(&mut paths, path, true)?;
        for extra in extras {
            push_sqlite_stamps(&mut paths, extra, false)?;
        }
        Ok(Self(paths))
    }

    /// Fingerprints a SQLite source plus a previously validated dependency
    /// snapshot. This prevents a cached Cursor summary from pairing model map A
    /// with tracking fingerprint B when the tracking DB changes mid-read.
    pub(crate) fn sqlite_with_dependency(
        path: &Path,
        dependency_path: &Path,
        dependency: Option<&DatabaseFingerprint>,
    ) -> Result<Self> {
        let mut paths = Vec::with_capacity(4);
        push_sqlite_stamps(&mut paths, path, true)?;
        match dependency {
            Some(fingerprint) => {
                paths.push((
                    dependency_path.to_path_buf(),
                    Some(FileStamp {
                        modified: fingerprint.database.modified,
                        len: fingerprint.database.length,
                    }),
                ));
                let wal = append_suffix(dependency_path, "-wal");
                paths.push((
                    wal,
                    fingerprint.wal.as_ref().map(|stamp| FileStamp {
                        modified: stamp.modified,
                        len: stamp.length,
                    }),
                ));
            }
            None => {
                paths.push((dependency_path.to_path_buf(), None));
                paths.push((append_suffix(dependency_path, "-wal"), None));
            }
        }
        Ok(Self(paths))
    }
}

fn required_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

fn optional_stamp(path: &Path) -> Result<Option<FileStamp>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(FileStamp {
        modified: metadata.modified()?,
        len: metadata.len(),
    }))
}

fn push_sqlite_stamps(
    paths: &mut Vec<(PathBuf, Option<FileStamp>)>,
    path: &Path,
    required: bool,
) -> Result<()> {
    let stamp = if required {
        Some(required_stamp(path)?)
    } else {
        optional_stamp(path)?
    };
    paths.push((path.to_path_buf(), stamp));
    let wal = append_suffix(path, "-wal");
    paths.push((wal.clone(), optional_stamp(&wal)?));
    Ok(())
}

/// Cached result and its source-level diagnostic state.
pub(crate) struct CachedSourceSummary {
    fingerprint: SourceFingerprint,
    pub(crate) summary: CompactSourceSummary,
    pub(crate) parsed: bool,
    pub(crate) failure: Option<String>,
}

/// Compact contribution retained between refreshes.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompactSourceSummary {
    pub(crate) usage_facts: Vec<UsageFact>,
    pub(crate) analysis_facts: Vec<AnalysisFact>,
}

impl CompactSourceSummary {
    /// Consumes a parser result while retaining only its private compact facts.
    pub(crate) fn from_parsed(mut parsed: ParsedAnalysis, emit_analysis: bool) -> Self {
        if emit_analysis && parsed.analysis_facts.is_empty() {
            parsed.analysis_facts = crate::session::diagnostics::fallback_facts(&parsed.analysis).1;
        }
        let ParsedAnalysis {
            usage_facts,
            analysis_facts,
            ..
        } = parsed;
        Self {
            usage_facts,
            analysis_facts: if emit_analysis {
                analysis_facts
            } else {
                Vec::new()
            },
        }
    }

    /// Folds one compact database usage row without materializing CodeAnalysis.
    pub(crate) fn add_usage_contribution(
        &mut self,
        contribution: UsageContribution,
        provider: ExtensionType,
    ) {
        let UsageContribution {
            date: _,
            timestamp_ms,
            model,
            tokens,
            stored_cost,
        } = contribution;
        let counts = crate::utils::TokenCounts {
            input_tokens: tokens.input_tokens,
            output_tokens: tokens.output_tokens,
            reasoning_tokens: tokens.reasoning_tokens,
            cache_read: tokens.cache_read_tokens,
            cache_creation: tokens.cache_creation_tokens,
            cache_creation_5m: tokens.cache_creation_tokens,
            cache_creation_1h: 0,
            web_search_requests: 0,
            total: tokens.input_tokens
                + tokens.output_tokens
                + tokens.reasoning_tokens
                + tokens.cache_read_tokens
                + tokens.cache_creation_tokens,
        };
        self.usage_facts.push(UsageFact::anonymous(
            timestamp_ms,
            self.usage_facts.len(),
            vec![UsageFactUnit {
                model,
                usage: std::sync::Arc::new(tokens.into_value()),
                counts,
                stored_cost,
                granularity: if provider == ExtensionType::Hermes {
                    PricingGranularity::Aggregate
                } else {
                    PricingGranularity::Request
                },
                provider_pricing_modifiers: Vec::new(),
                analysis_presence: true,
            }],
        ));
    }

    /// Folds one database-backed analysis row whose date is authoritative.
    pub(crate) fn add_analysis(
        &mut self,
        analysis: CodeAnalysis,
        emit_analysis: bool,
        analysis_facts: Vec<AnalysisFact>,
    ) {
        if !emit_analysis {
            return;
        }
        let has_explicit_facts = !analysis_facts.is_empty();
        self.analysis_facts.extend(analysis_facts);
        for record in analysis.records {
            let timestamp_ms = (record.timestamp > 0).then_some(record.timestamp);
            let metrics = AnalysisMetrics::from_record(&record);
            if !has_explicit_facts
                && (metrics.has_activity() || !record.conversation_usage.is_empty())
            {
                let mut models: Vec<_> = record
                    .conversation_usage
                    .keys()
                    .filter(|model| !model.contains("<synthetic>"))
                    .cloned()
                    .collect();
                if models.is_empty() {
                    models.push(String::new());
                }
                for model in models {
                    self.analysis_facts.push(AnalysisFact {
                        stable_id: None,
                        timestamp_ms,
                        observed_at_ms: timestamp_ms,
                        source_order: self.analysis_facts.len(),
                        model,
                        status: ToolFactStatus::Succeeded,
                        metrics,
                        effect: None,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_usage_fact_unit_shares_its_json_payload() {
        let unit = UsageFactUnit::from_counts(
            "model".to_string(),
            crate::utils::TokenCounts {
                input_tokens: 1,
                total: 1,
                ..Default::default()
            },
            PricingGranularity::Request,
        );
        let cloned = unit.clone();

        assert!(std::sync::Arc::ptr_eq(&unit.usage, &cloned.usage));
    }

    #[test]
    fn dependency_fingerprint_stays_paired_with_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store.db");
        let tracking = dir.path().join("tracking.db");
        std::fs::write(&store, b"store").unwrap();
        std::fs::write(&tracking, b"model-a").unwrap();
        let snapshot = crate::session::sqlite::database_fingerprint(&tracking).unwrap();

        let paired =
            SourceFingerprint::sqlite_with_dependency(&store, &tracking, Some(&snapshot)).unwrap();
        std::fs::write(&tracking, b"model-b-longer").unwrap();
        let current = crate::session::sqlite::database_fingerprint(&tracking).unwrap();
        let changed =
            SourceFingerprint::sqlite_with_dependency(&store, &tracking, Some(&current)).unwrap();

        assert_ne!(paired, changed);
    }
}
