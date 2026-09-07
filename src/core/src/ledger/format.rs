//! The on-disk shape of one provider's ledger file, and the fold helpers that
//! build its per-day entries from parsed sessions.
//!
//! Every value here is either something a provider recorded (a token bucket in
//! the provider's own key shape, a stored cost) or a plain count, so a view for
//! any time range is a filter on the day key followed by addition. Nothing
//! priced is ever written: cost is computed at display time from the current
//! pricing map.

use crate::constants::FastHashMap;
use crate::models::{AggregatedAnalysisRow, CodeAnalysis};
use crate::session::diagnostics::UsageTokenContribution;
use crate::utils::{extract_token_counts, merge_usage_values};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::{RawValue, to_raw_value};
use std::collections::BTreeMap;

/// Version of the file layout below. A file carrying a newer version is left
/// untouched and never written; there is no older version to migrate yet.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// One provider's ledger file.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LedgerFile {
    pub(crate) schema_version: u32,
    pub(crate) provider: String,
    /// Whole-database stamps for the providers whose sessions all live in one
    /// SQLite file (OpenCode, Hermes); absent for every other provider.
    #[serde(default, skip_serializing_if = "HalfStamps::is_empty")]
    pub(crate) source: HalfStamps,
    #[serde(default)]
    pub(crate) sessions: BTreeMap<String, SessionEntry>,
}

/// Just the version, so a file this build cannot decode can still say why.
#[derive(Deserialize)]
pub(crate) struct LedgerHeader {
    pub(crate) schema_version: u32,
}

/// The two features' read stamps for one source, kept apart because a
/// database source is read by two different queries at different costs, so one
/// half can be current while the other is stale.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HalfStamps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<ScanStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) analysis: Option<ScanStamp>,
}

impl HalfStamps {
    pub(crate) fn is_empty(&self) -> bool {
        self.usage.is_none() && self.analysis.is_none()
    }

    pub(crate) fn half(&self, feature: ScanFeature) -> Option<&ScanStamp> {
        match feature {
            ScanFeature::Usage => self.usage.as_ref(),
            ScanFeature::Analysis => self.analysis.as_ref(),
        }
    }

    pub(crate) fn set_half(&mut self, feature: ScanFeature, stamp: ScanStamp) {
        match feature {
            ScanFeature::Usage => self.usage = Some(stamp),
            ScanFeature::Analysis => self.analysis = Some(stamp),
        }
    }

    /// Whether `feature`'s half was read from exactly what `current` describes.
    pub(crate) fn is_current(&self, feature: ScanFeature, current: &ScanStamp) -> bool {
        self.half(feature)
            .is_some_and(|stamp| stamp.is_current(feature, current))
    }

    /// Drops the retained diagnostics; a source that is gone can no longer be
    /// re-read, so its parse verdict has nothing left to say.
    pub(crate) fn clear_failures(&mut self) {
        for stamp in [self.usage.as_mut(), self.analysis.as_mut()]
            .into_iter()
            .flatten()
        {
            stamp.failure = None;
        }
    }
}

/// Which feature is scanning, and therefore which half of an entry it reads
/// and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanFeature {
    Usage,
    Analysis,
}

/// What one read of a source saw: the build that read it, the tier snapshot it
/// classified against, every file the result depends on, and how the read went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScanStamp {
    pub(crate) parser: String,
    /// Fingerprint of the context-tier snapshot the usage half classified
    /// against; `None` when it classified nothing. The analysis half never
    /// carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tiers: Option<String>,
    /// Every file the read depended on, by name; `None` is an optional sidecar
    /// that did not exist.
    pub(crate) files: BTreeMap<String, Option<FileStamp>>,
    /// Whether the read produced usable data (a valid blank source included).
    #[serde(default = "default_true", skip_serializing_if = "Clone::clone")]
    pub(crate) parsed: bool,
    /// The retained diagnostic for a present source: a partial-parse reason
    /// beside its data, or the reason a read produced nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<String>,
}

fn default_true() -> bool {
    true
}

impl ScanStamp {
    /// A stamp for a read this build is about to do.
    pub(crate) fn new(tiers: Option<String>, files: BTreeMap<String, Option<FileStamp>>) -> Self {
        Self {
            parser: crate::VERSION.to_string(),
            tiers,
            files,
            parsed: true,
            failure: None,
        }
    }

    /// The same stamp with its read verdict filled in.
    pub(crate) fn with_verdict(mut self, parsed: bool, failure: Option<String>) -> Self {
        self.parsed = parsed;
        self.failure = failure;
        self
    }

    /// Whether a read described by `current` would see what this stamp saw.
    ///
    /// The analysis half ignores the tier snapshot: file-operation counts do
    /// not depend on it, so an entry a usage scan wrote still serves an
    /// analysis scan and vice versa, while a usage scan with a different
    /// snapshot has to re-read.
    pub(crate) fn is_current(&self, feature: ScanFeature, current: &ScanStamp) -> bool {
        self.files == current.files
            && self.parser == current.parser
            && (feature == ScanFeature::Analysis || self.tiers == current.tiers)
    }
}

/// The modification time and length of one file the read depended on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    /// RFC 3339 UTC with nanoseconds, the stamp format vct writes everywhere.
    pub(crate) modified: String,
    pub(crate) len: u64,
}

/// One session: its read stamps, where it ran, and what it did per day.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct SessionEntry {
    /// Read stamps for a session that is its own source (a file, a Cursor
    /// store). A session inside a shared database has none; its provider's
    /// `source` stamps cover it.
    #[serde(default, skip_serializing_if = "HalfStamps::is_empty")]
    pub(crate) scanned: HalfStamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) git_remote: Option<String>,
    /// Keyed by local `YYYY-MM-DD`. A file session has exactly one day, the
    /// date of its last modification; a database session has one per day it
    /// was active.
    #[serde(default)]
    pub(crate) days: BTreeMap<String, DayEntry>,
    /// Local date on which the source was first found to be gone. Cleared if
    /// it comes back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) missing_since: Option<String>,
}

impl SessionEntry {
    /// Builds a file session's entry from one parse.
    ///
    /// One parse yields both halves, so both are stamped. The usage half keeps
    /// the snapshot the parse classified against (`None` for an analysis
    /// scan, which classifies nothing); the analysis half never carries one.
    pub(crate) fn from_file_parse(
        analysis: Option<CodeAnalysis>,
        emit: bool,
        date: String,
        stamp: ScanStamp,
    ) -> Self {
        let mut entry = Self::default();
        if let Some(analysis) = analysis {
            if let Some(record) = analysis.records.first() {
                entry.cwd = non_empty(&record.folder_path);
                entry.git_remote = non_empty(&record.git_remote_url);
            }
            let mut day = DayEntry::default();
            day.add_file_records(analysis, emit);
            if !day.is_empty() {
                entry.days.insert(date, day);
            }
        }
        entry.scanned.analysis = Some(ScanStamp {
            tiers: None,
            ..stamp.clone()
        });
        entry.scanned.usage = Some(stamp);
        entry
    }

    /// Replaces `feature`'s half of this whole session with `days`, leaving
    /// the other half untouched.
    ///
    /// The half is replaced across every day, not merged day by day: a
    /// database row can be cumulative and re-dated (a Hermes per-model row
    /// carries the session's running total under its latest `last_seen`), so
    /// keeping a day the re-read no longer produced would count that total
    /// twice. Retention is per session; a session the read still returns is
    /// exactly what the read says it is.
    pub(crate) fn replace_half(&mut self, feature: ScanFeature, days: BTreeMap<String, DayEntry>) {
        for day in self.days.values_mut() {
            day.clear_half(feature);
        }
        for (date, day) in days {
            self.days
                .entry(date)
                .or_default()
                .replace_half(feature, day);
        }
        self.days.retain(|_, day| !day.is_empty());
    }
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// What one session did on one local day.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct DayEntry {
    /// Per-model token buckets in the provider's own key shape, merged across
    /// the day's records exactly as the usage roll-up merges them.
    ///
    /// Held as the JSON text they serialize to rather than as parsed trees:
    /// a whole ledger sits in memory for every refresh, a parsed object costs
    /// several times its text, and a fold re-parses a few hundred bytes per
    /// model in microseconds. The text is compact even inside the pretty
    /// ledger file, which also keeps one model's buckets on one line there.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) usage: BTreeMap<String, Box<RawValue>>,
    /// Per-model file-operation and tool-call counters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) analysis: BTreeMap<String, AggregatedAnalysisRow>,
    /// The cost the provider itself recorded, per model (OpenCode, Hermes).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) stored_cost: BTreeMap<String, f64>,
    #[serde(default)]
    pub(crate) active: ActiveFlags,
}

/// Whether the day counts as an active day for each feature.
///
/// Usage needs tokens or a stored cost; analysis needs only that the session
/// was emitted, which is why it cannot be derived from the counters (a session
/// that ran no tool is still a day the assistant was used).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveFlags {
    #[serde(default)]
    pub(crate) usage: bool,
    #[serde(default)]
    pub(crate) analysis: bool,
}

impl DayEntry {
    /// One model's buckets as a value the roll-up can merge.
    ///
    /// Text that no longer parses (a hand-edited file) folds as nothing
    /// rather than failing the scan.
    pub(crate) fn usage_value(raw: &RawValue) -> Value {
        serde_json::from_str(raw.get()).unwrap_or(Value::Null)
    }

    /// Folds one parsed file session, both halves at once.
    ///
    /// Every model in a record's `conversation_usage` is credited with the
    /// record's whole counters, except a `<synthetic>` placeholder, and
    /// advisor usage joins the token map under the advisor's own model
    /// without any counters, since an advisor never runs a tool.
    pub(crate) fn add_file_records(&mut self, analysis: CodeAnalysis, emit: bool) {
        for record in analysis.records {
            for (model, usage) in record.conversation_usage {
                if meaningful_usage(&usage) {
                    self.active.usage = true;
                }
                if emit && !model.contains("<synthetic>") {
                    let row = self
                        .analysis
                        .entry(model.clone())
                        .or_insert_with(|| empty_row(&model));
                    row.edit_lines += record.total_edit_lines;
                    row.read_lines += record.total_read_lines;
                    row.write_lines += record.total_write_lines;
                    row.bash_count += record.tool_call_counts.bash;
                    row.edit_count += record.tool_call_counts.edit;
                    row.read_count += record.tool_call_counts.read;
                    row.todo_write_count += record.tool_call_counts.todo_write;
                    row.write_count += record.tool_call_counts.write;
                }
                merge_model_usage(&mut self.usage, model, usage);
            }
            for (model, usage) in record.advisor_usage {
                if meaningful_usage(&usage) {
                    self.active.usage = true;
                }
                merge_model_usage(&mut self.usage, model, usage);
            }
        }
        if emit {
            self.active.analysis = true;
        }
    }

    /// Folds one database usage row into the usage half.
    ///
    /// `stored_cost` is `None` for a provider that prices none of its rows
    /// (Cursor). One that does is recorded even when it is zero, matching the
    /// uncached roll-up, whose per-model cost map carries a `0.0` entry for a
    /// model the provider priced at nothing.
    pub(crate) fn add_usage_row(
        &mut self,
        model: String,
        tokens: UsageTokenContribution,
        tier_level: usize,
        stored_cost: Option<f64>,
    ) {
        if stored_cost.is_some_and(|cost| cost != 0.0) || tokens.has_activity() {
            self.active.usage = true;
        }
        if let Some(cost) = stored_cost {
            *self.stored_cost.entry(model.clone()).or_insert(0.0) += cost;
        }
        merge_model_usage(&mut self.usage, model, tokens.into_value(tier_level));
    }

    /// Folds one database analysis row into the analysis half.
    pub(crate) fn add_analysis_records(&mut self, analysis: &CodeAnalysis) {
        for record in &analysis.records {
            for model in record.conversation_usage.keys() {
                if model.contains("<synthetic>") {
                    continue;
                }
                let row = self
                    .analysis
                    .entry(model.clone())
                    .or_insert_with(|| empty_row(model));
                row.edit_lines += record.total_edit_lines;
                row.read_lines += record.total_read_lines;
                row.write_lines += record.total_write_lines;
                row.bash_count += record.tool_call_counts.bash;
                row.edit_count += record.tool_call_counts.edit;
                row.read_count += record.tool_call_counts.read;
                row.todo_write_count += record.tool_call_counts.todo_write;
                row.write_count += record.tool_call_counts.write;
            }
        }
        self.active.analysis = true;
    }

    /// Takes `feature`'s half from `other`, keeping the other half as it is.
    pub(crate) fn replace_half(&mut self, feature: ScanFeature, other: DayEntry) {
        match feature {
            ScanFeature::Usage => {
                self.usage = other.usage;
                self.stored_cost = other.stored_cost;
                self.active.usage = other.active.usage;
            }
            ScanFeature::Analysis => {
                self.analysis = other.analysis;
                self.active.analysis = other.active.analysis;
            }
        }
    }

    /// Empties `feature`'s half, keeping the other half as it is.
    pub(crate) fn clear_half(&mut self, feature: ScanFeature) {
        self.replace_half(feature, DayEntry::default());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.usage.is_empty()
            && self.analysis.is_empty()
            && self.stored_cost.is_empty()
            && !self.active.usage
            && !self.active.analysis
    }
}

fn empty_row(model: &str) -> AggregatedAnalysisRow {
    AggregatedAnalysisRow {
        model: model.to_string(),
        edit_lines: 0,
        read_lines: 0,
        write_lines: 0,
        bash_count: 0,
        edit_count: 0,
        read_count: 0,
        todo_write_count: 0,
        write_count: 0,
    }
}

fn merge_model_usage(target: &mut BTreeMap<String, Box<RawValue>>, model: String, usage: Value) {
    let merged = match target.get(&model) {
        Some(existing) => {
            let mut value = DayEntry::usage_value(existing);
            merge_usage_values(&mut value, &usage);
            value
        }
        None => usage,
    };
    target.insert(
        model,
        to_raw_value(&merged).expect("a JSON value always serializes"),
    );
}

fn meaningful_usage(value: &Value) -> bool {
    extract_token_counts(value).has_activity()
}

/// Merges per-model counters into a model-keyed accumulator.
pub(crate) fn merge_analysis_rows(
    target: &mut FastHashMap<String, AggregatedAnalysisRow>,
    source: &BTreeMap<String, AggregatedAnalysisRow>,
) {
    for (model, row) in source {
        let entry = target
            .entry(model.clone())
            .or_insert_with(|| empty_row(model));
        entry.edit_lines += row.edit_lines;
        entry.read_lines += row.read_lines;
        entry.write_lines += row.write_lines;
        entry.bash_count += row.bash_count;
        entry.edit_count += row.edit_count;
        entry.read_count += row.read_count;
        entry.todo_write_count += row.todo_write_count;
        entry.write_count += row.write_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CodeAnalysisRecord, CodeAnalysisToolCalls};
    use crate::session::diagnostics::UsageTokenContribution;
    use serde_json::json;

    fn analysis_with_usage_value(value: Value) -> CodeAnalysis {
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

    fn analysis_with_usage(tokens: i64) -> CodeAnalysis {
        analysis_with_usage_value(json!({ "input_tokens": tokens }))
    }

    #[test]
    fn zero_usage_does_not_mark_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(analysis_with_usage(0), true);
        assert!(!day.active.usage);
        assert!(day.active.analysis);
    }

    #[test]
    fn nonzero_usage_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(analysis_with_usage(1), true);
        assert!(day.active.usage);
    }

    #[test]
    fn published_total_alone_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(
            analysis_with_usage_value(json!({ "total_token_usage": { "total_tokens": 7 } })),
            false,
        );
        assert!(day.active.usage);
        assert!(!day.active.analysis);
    }

    #[test]
    fn tool_only_usage_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(
            analysis_with_usage_value(json!({
                "input_tokens": 0,
                "output_tokens": 0,
                "tool_tokens": 5,
                "total_tokens": 5
            })),
            false,
        );
        assert!(day.active.usage);
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

        let mut day = DayEntry::default();
        day.add_file_records(analysis, true);
        assert!(day.analysis.is_empty());
        assert!(day.active.analysis);
        assert!(!day.is_empty());
    }

    #[test]
    fn a_blank_source_leaves_an_empty_day() {
        let mut analysis = analysis_with_usage(0);
        analysis.records.clear();
        let mut day = DayEntry::default();
        day.add_file_records(analysis, false);
        assert!(day.is_empty());
    }

    #[test]
    fn one_model_at_two_levels_keeps_its_slices_apart() {
        let tokens = |input: i64| UsageTokenContribution {
            input_tokens: input,
            ..Default::default()
        };
        let mut day = DayEntry::default();
        day.add_usage_row("model".to_string(), tokens(300_000), 1, Some(0.0));
        day.add_usage_row("model".to_string(), tokens(400_000), 2, Some(0.0));
        day.add_usage_row("model".to_string(), tokens(1_000), 0, Some(0.0));

        let counts = extract_token_counts(&DayEntry::usage_value(&day.usage["model"]));
        assert_eq!(counts.input_tokens, 701_000);
        assert_eq!(counts.above_tiers[0].input_tokens, 300_000);
        assert_eq!(counts.above_tiers[1].input_tokens, 400_000);
        assert_eq!(day.stored_cost["model"], 0.0);
    }

    #[test]
    fn replacing_one_half_leaves_the_other() {
        let mut day = DayEntry::default();
        day.add_usage_row(
            "model".to_string(),
            UsageTokenContribution {
                input_tokens: 5,
                ..Default::default()
            },
            0,
            Some(0.25),
        );
        let mut fresh = DayEntry::default();
        fresh.add_analysis_records(&analysis_with_usage(0));
        day.replace_half(ScanFeature::Analysis, fresh);

        assert!(day.active.usage);
        assert!(day.active.analysis);
        assert_eq!(day.stored_cost["model"], 0.25);
        assert!(day.analysis.contains_key("model"));
    }

    #[test]
    fn replacing_a_session_half_drops_the_days_the_read_no_longer_produced() {
        let tokens = UsageTokenContribution {
            input_tokens: 1_000,
            ..Default::default()
        };
        let mut first = DayEntry::default();
        first.add_usage_row("model".to_string(), tokens, 0, Some(0.0));
        let mut entry = SessionEntry::default();
        entry.days.insert("2026-09-01".to_string(), first.clone());
        entry
            .days
            .entry("2026-09-01".to_string())
            .or_default()
            .add_analysis_records(&analysis_with_usage(0));

        // The cumulative row moved to a later day carrying the whole total.
        let mut second = DayEntry::default();
        second.add_usage_row(
            "model".to_string(),
            UsageTokenContribution {
                input_tokens: 3_000,
                ..Default::default()
            },
            0,
            Some(0.0),
        );
        entry.replace_half(
            ScanFeature::Usage,
            BTreeMap::from([("2026-09-02".to_string(), second)]),
        );

        let old = &entry.days["2026-09-01"];
        assert!(
            old.usage.is_empty(),
            "the moved day must not keep the old total"
        );
        assert!(old.active.analysis, "the other half stays");
        assert_eq!(
            extract_token_counts(&DayEntry::usage_value(
                &entry.days["2026-09-02"].usage["model"]
            ))
            .input_tokens,
            3_000
        );
    }

    #[test]
    fn analysis_half_ignores_the_tier_snapshot() {
        let files = BTreeMap::from([(
            "session.jsonl".to_string(),
            Some(FileStamp {
                modified: "2026-09-08T00:00:00.000000000Z".to_string(),
                len: 1,
            }),
        )]);
        let stored = ScanStamp::new(None, files.clone());
        let current = ScanStamp::new(Some("abc".to_string()), files);
        assert!(stored.is_current(ScanFeature::Analysis, &current));
        assert!(!stored.is_current(ScanFeature::Usage, &current));
    }

    #[test]
    fn file_parse_stamps_both_halves_but_tiers_only_on_usage() {
        let stamp = ScanStamp::new(Some("abc".to_string()), BTreeMap::new());
        let entry = SessionEntry::from_file_parse(
            Some(analysis_with_usage(1)),
            true,
            "2026-09-08".into(),
            stamp,
        );
        assert_eq!(
            entry.scanned.usage.as_ref().unwrap().tiers.as_deref(),
            Some("abc")
        );
        assert!(entry.scanned.analysis.as_ref().unwrap().tiers.is_none());
        assert!(entry.days.contains_key("2026-09-08"));
    }
}
