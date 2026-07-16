use crate::VERSION;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, ExtensionType};
use crate::session::state::{ParseMode, SessionParseState};
use serde_json::{Value, json};

/// Parser-only counters used to distinguish valid empty sessions from schema drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ParseDiagnostics {
    pub source_records: usize,
    pub recognized_records: usize,
    pub unrecognized_records: usize,
    pub relevant_records: usize,
    pub normalized_records: usize,
    pub failed_relevant_records: usize,
    pub malformed_records: usize,
}

impl ParseDiagnostics {
    pub fn merge(&mut self, other: Self) {
        self.source_records += other.source_records;
        self.recognized_records += other.recognized_records;
        self.unrecognized_records += other.unrecognized_records;
        self.relevant_records += other.relevant_records;
        self.normalized_records += other.normalized_records;
        self.failed_relevant_records += other.failed_relevant_records;
        self.malformed_records += other.malformed_records;
    }

    pub fn record_recognized_source(&mut self) {
        self.source_records += 1;
        self.recognized_records += 1;
    }

    pub fn record_relevant(&mut self, normalized: bool) {
        self.relevant_records += 1;
        if normalized {
            self.normalized_records += 1;
        } else {
            self.failed_relevant_records += 1;
        }
    }

    pub fn record_unrecognized(&mut self) {
        self.source_records += 1;
        self.unrecognized_records += 1;
    }

    pub fn record_malformed(&mut self) {
        self.source_records += 1;
        self.malformed_records += 1;
    }

    pub fn is_complete_failure(&self) -> bool {
        (self.source_records > 0 && self.recognized_records == 0)
            || (self.relevant_records > 0 && self.normalized_records == 0)
    }

    pub fn partial_failure_count(&self) -> usize {
        if self.is_complete_failure() {
            0
        } else {
            self.failed_relevant_records + self.malformed_records + self.unrecognized_records
        }
    }

    pub fn should_emit_session(&self) -> bool {
        self.source_records > 0 && self.recognized_records > 0 && !self.is_complete_failure()
    }
}

/// Reason string for a source that parsed but skipped some records.
///
/// Worded so every surface that shows it (per-source warnings, stderr
/// summaries) states that the recognized records were still counted — a
/// partial skip is not a dropped source.
pub(crate) fn partial_failure_reason(count: usize) -> String {
    format!("partially parsed: skipped {count} malformed or unsupported analyzer records")
}

/// Whether a stored failure reason describes a partial (data-retained) parse.
pub(crate) fn is_partial_failure_reason(reason: &str) -> bool {
    reason.starts_with("partially parsed:")
}

/// A normalized analysis value plus parser-only diagnostics.
#[derive(Debug)]
pub(crate) struct ParsedAnalysis {
    pub analysis: CodeAnalysis,
    pub diagnostics: ParseDiagnostics,
}

/// Database-backed analysis row with a stable source identity for ordering.
pub(crate) struct DatabaseAnalysisRow {
    pub source_id: String,
    pub date: String,
    pub analysis: CodeAnalysis,
}

/// Typed token buckets produced by database-backed usage readers.
///
/// SQLite rows stay in this scalar form through the incremental cache. The
/// compatibility wrappers materialize the historical JSON object only at the
/// public API boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UsageTokenContribution {
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) reasoning_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_creation_tokens: i64,
}

impl UsageTokenContribution {
    pub(crate) fn into_value(self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_input_tokens": self.cache_read_tokens,
            "cache_creation_input_tokens": self.cache_creation_tokens,
            "reasoning_output_tokens": self.reasoning_tokens,
        })
    }

    pub(crate) fn has_activity(self) -> bool {
        self.input_tokens != 0
            || self.output_tokens != 0
            || self.reasoning_tokens != 0
            || self.cache_read_tokens != 0
            || self.cache_creation_tokens != 0
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
    }
}

/// Compact token contribution produced by a database-backed usage reader.
#[derive(Debug)]
pub(crate) struct UsageContribution {
    pub(crate) date: String,
    pub(crate) timestamp_ms: i64,
    pub(crate) model: String,
    pub(crate) tokens: UsageTokenContribution,
    pub(crate) stored_cost: f64,
}

/// Database usage rows plus schema-normalization diagnostics.
#[derive(Debug)]
pub(crate) struct DatabaseUsageRead {
    pub(crate) rows: Vec<UsageContribution>,
    pub(crate) expected_records: usize,
    pub(crate) parsed_records: usize,
}

impl DatabaseUsageRead {
    pub(crate) fn complete(rows: Vec<UsageContribution>) -> Self {
        let parsed_records = rows.len();
        Self {
            rows,
            expected_records: parsed_records,
            parsed_records,
        }
    }

    pub(crate) fn failed_records(&self) -> usize {
        self.expected_records.saturating_sub(self.parsed_records)
    }
}

impl UsageContribution {
    pub(crate) fn single_model(
        date: String,
        timestamp_ms: i64,
        model: String,
        tokens: UsageTokenContribution,
        stored_cost: f64,
    ) -> Self {
        Self {
            date,
            timestamp_ms,
            model,
            tokens,
            stored_cost,
        }
    }

    pub(crate) fn into_public_row(
        self,
        provider: ExtensionType,
        user: &str,
        machine: &str,
    ) -> (String, CodeAnalysis, f64) {
        let mut state = SessionParseState::with_mode(ParseMode::UsageOnly);
        state.last_ts = self.timestamp_ms;
        let mut usage = FastHashMap::default();
        usage.insert(self.model, self.tokens.into_value());
        let analysis = CodeAnalysis {
            user: user.to_string(),
            extension_name: provider.to_string(),
            insights_version: VERSION.to_string(),
            machine_id: machine.to_string(),
            records: vec![state.into_record(usage)],
        };
        (self.date, analysis, self.stored_cost)
    }
}

impl ParsedAnalysis {
    pub fn new(analysis: CodeAnalysis, diagnostics: ParseDiagnostics) -> Self {
        Self {
            analysis,
            diagnostics,
        }
    }
}
