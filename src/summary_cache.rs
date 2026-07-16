//! Process-local cache for compact usage and analysis scan contributions.

use crate::analysis::AggregatedAnalysisRow;
use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, ExtensionType, UsageResult};
use crate::session::diagnostics::{UsageContribution, UsageTokenContribution};
use crate::session::sqlite::{DatabaseFingerprint, append_suffix};
use crate::usage::merge_usage_values;
use crate::utils::extract_token_counts;
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
    tier_fingerprint: u64,
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

    /// Drops every entry when the context-tier snapshot changed.
    ///
    /// Cached summaries embed the per-request tier classification, so a new
    /// thresholds snapshot (daily pricing reload, or pricing becoming
    /// available after an offline start) must invalidate them; unchanged
    /// snapshots keep the incremental behavior.
    pub(crate) fn ensure_tier_fingerprint(&mut self, fingerprint: u64) {
        if self.tier_fingerprint != fingerprint {
            self.entries.clear();
            self.tier_fingerprint = fingerprint;
        }
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

/// Stable source identity, including the effective date cutoff.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SummaryCacheKey {
    kind: SummaryKind,
    provider: String,
    path: PathBuf,
    cutoff: Option<String>,
}

impl SummaryCacheKey {
    pub(crate) fn new(
        kind: SummaryKind,
        provider: ExtensionType,
        path: &Path,
        time_range: TimeRange,
    ) -> Self {
        Self {
            kind,
            provider: provider.to_string(),
            path: path.to_path_buf(),
            cutoff: time_range
                .cutoff_date()
                .map(|date| date.format("%Y-%m-%d").to_string()),
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
    /// File-parser usage values, retained in their provider-specific shape so
    /// the diagnostics-aware collector stays identical to the public legacy
    /// aggregation API.
    pub(crate) usage: UsageResult,
    /// Typed database usage. SQLite rows never need a per-row JSON object or
    /// model map in the incremental path.
    pub(crate) database_usage: FastHashMap<String, UsageTokenContribution>,
    pub(crate) stored_costs: FastHashMap<String, f64>,
    pub(crate) usage_dates: HashSet<String>,
    pub(crate) analysis: FastHashMap<String, AggregatedAnalysisRow>,
    pub(crate) analysis_dates: HashSet<String>,
}

impl CompactSourceSummary {
    /// Consumes a UsageOnly parse without retaining the full analysis.
    pub(crate) fn from_file(analysis: CodeAnalysis, date: String, emit_analysis: bool) -> Self {
        let mut summary = Self::default();
        summary.add_analysis(analysis, date, 0.0, emit_analysis);
        summary
    }

    /// Folds one compact database usage row without materializing CodeAnalysis.
    pub(crate) fn add_usage_contribution(&mut self, contribution: UsageContribution) {
        let UsageContribution {
            date,
            timestamp_ms: _,
            model,
            tokens,
            stored_cost,
        } = contribution;
        let date_has_usage = stored_cost != 0.0 || tokens.has_activity();
        *self.stored_costs.entry(model.clone()).or_insert(0.0) += stored_cost;
        self.database_usage
            .entry(model)
            .and_modify(|existing| existing.merge(tokens))
            .or_insert(tokens);
        if date_has_usage {
            self.usage_dates.insert(date);
        }
    }

    /// Folds one owned analysis row and optional provider-stored cost.
    pub(crate) fn add_analysis(
        &mut self,
        analysis: CodeAnalysis,
        date: String,
        stored_cost: f64,
        emit_analysis: bool,
    ) {
        let mut date_has_usage = stored_cost != 0.0;
        for record in analysis.records {
            let counters = (
                record.total_edit_lines,
                record.total_read_lines,
                record.total_write_lines,
                record.tool_call_counts.bash,
                record.tool_call_counts.edit,
                record.tool_call_counts.read,
                record.tool_call_counts.todo_write,
                record.tool_call_counts.write,
            );

            for (model, usage) in record.conversation_usage {
                if meaningful_usage(&usage) {
                    date_has_usage = true;
                }
                if stored_cost != 0.0 {
                    *self.stored_costs.entry(model.clone()).or_insert(0.0) += stored_cost;
                }
                merge_model_usage(&mut self.usage, model.clone(), usage);

                if emit_analysis && !model.contains("<synthetic>") {
                    let row = self.analysis.entry(model.clone()).or_insert_with(|| {
                        AggregatedAnalysisRow {
                            model,
                            edit_lines: 0,
                            read_lines: 0,
                            write_lines: 0,
                            bash_count: 0,
                            edit_count: 0,
                            read_count: 0,
                            todo_write_count: 0,
                            write_count: 0,
                        }
                    });
                    row.edit_lines += counters.0;
                    row.read_lines += counters.1;
                    row.write_lines += counters.2;
                    row.bash_count += counters.3;
                    row.edit_count += counters.4;
                    row.read_count += counters.5;
                    row.todo_write_count += counters.6;
                    row.write_count += counters.7;
                }
            }

            for (model, usage) in record.advisor_usage {
                if meaningful_usage(&usage) {
                    date_has_usage = true;
                }
                merge_model_usage(&mut self.usage, model, usage);
            }
        }

        if date_has_usage {
            self.usage_dates.insert(date.clone());
        }
        if emit_analysis {
            self.analysis_dates.insert(date);
        }
    }
}

fn merge_model_usage(result: &mut UsageResult, model: String, usage: serde_json::Value) {
    result
        .entry(model)
        .and_modify(|existing| merge_usage_values(existing, &usage))
        .or_insert(usage);
}

fn meaningful_usage(value: &serde_json::Value) -> bool {
    let counts = extract_token_counts(value);
    counts.total != 0
        || counts.input_tokens != 0
        || counts.output_tokens != 0
        || counts.reasoning_tokens != 0
        || counts.cache_read != 0
        || counts.cache_creation != 0
        || counts.cache_creation_5m != 0
        || counts.cache_creation_1h != 0
        || counts.web_search_requests != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CodeAnalysisRecord, CodeAnalysisToolCalls};
    use serde_json::json;

    fn analysis_with_usage(tokens: i64) -> CodeAnalysis {
        analysis_with_usage_value(json!({ "input_tokens": tokens }))
    }

    fn analysis_with_usage_value(value: serde_json::Value) -> CodeAnalysis {
        let mut usage = FastHashMap::default();
        usage.insert("model".to_string(), value);
        CodeAnalysis {
            user: String::new(),
            extension_name: "Codex".to_string(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![CodeAnalysisRecord {
                total_unique_files: 0,
                total_write_lines: 0,
                total_read_lines: 0,
                total_edit_lines: 0,
                total_write_characters: 0,
                total_read_characters: 0,
                total_edit_characters: 0,
                write_file_details: Vec::new(),
                read_file_details: Vec::new(),
                edit_file_details: Vec::new(),
                run_command_details: Vec::new(),
                tool_call_counts: CodeAnalysisToolCalls::default(),
                conversation_usage: usage,
                advisor_usage: FastHashMap::default(),
                task_id: String::new(),
                timestamp: 0,
                folder_path: String::new(),
                git_remote_url: String::new(),
            }],
        }
    }

    #[test]
    fn zero_usage_does_not_mark_an_active_day() {
        let summary =
            CompactSourceSummary::from_file(analysis_with_usage(0), "2026-07-14".to_string(), true);
        assert!(summary.usage_dates.is_empty());
    }

    #[test]
    fn nonzero_usage_marks_an_active_day() {
        let summary =
            CompactSourceSummary::from_file(analysis_with_usage(1), "2026-07-14".to_string(), true);
        assert!(summary.usage_dates.contains("2026-07-14"));
    }

    #[test]
    fn published_total_alone_marks_an_active_day() {
        let summary = CompactSourceSummary::from_file(
            analysis_with_usage_value(json!({
                "total_token_usage": { "total_tokens": 7 }
            })),
            "2026-07-14".to_string(),
            false,
        );
        assert!(summary.usage_dates.contains("2026-07-14"));
    }

    #[test]
    fn tool_only_usage_marks_an_active_day() {
        let summary = CompactSourceSummary::from_file(
            analysis_with_usage_value(json!({
                "input_tokens": 0,
                "output_tokens": 0,
                "tool_tokens": 5,
                "total_tokens": 5
            })),
            "2026-07-14".to_string(),
            false,
        );
        assert!(summary.usage_dates.contains("2026-07-14"));
    }

    #[test]
    fn emitted_synthetic_session_keeps_analysis_active_day() {
        let mut analysis = analysis_with_usage(0);
        let usage = analysis.records[0]
            .conversation_usage
            .remove("model")
            .unwrap();
        analysis.records[0]
            .conversation_usage
            .insert("<synthetic>".to_string(), usage);

        let summary = CompactSourceSummary::from_file(analysis, "2026-07-14".to_string(), true);
        assert!(summary.analysis.is_empty());
        assert!(summary.analysis_dates.contains("2026-07-14"));
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
