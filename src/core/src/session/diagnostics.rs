use crate::VERSION;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, ExtensionType};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::TierSlice;
use crate::utils::token_merge::write_tier_slice;
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
/// The `partially parsed:` prefix is a contract, not phrasing:
/// [`is_partial_failure_reason`] matches it to pick the per-source log wording
/// that keeps a partial skip from reading as a dropped source. The tail is
/// shown verbatim in the CLI's stderr scan summary, so it has to stand on its
/// own there; rewording the tail is free, changing the prefix silently breaks
/// that match.
pub(crate) fn partial_failure_reason(count: usize) -> String {
    format!("partially parsed: skipped {count} malformed or unsupported analyzer records")
}

/// Whether a stored failure reason describes a partial (data-retained) parse.
///
/// Recognizes only the prefix [`partial_failure_reason`] writes; any other
/// reason is a source that produced nothing.
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
/// SQLite rows stay in this scalar form through the incremental cache, so a
/// cached source costs no per-row JSON object. Callers materialize the
/// historical JSON shape with [`Self::into_value`] only where they fold into a
/// map that is already `Value`-keyed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UsageTokenContribution {
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) reasoning_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_creation_tokens: i64,
}

impl UsageTokenContribution {
    /// Materializes the historical JSON shape, billed at price `tier_level`.
    ///
    /// Above level 0 the same buckets are additionally written into an
    /// `above_tier` object, exactly as the file parsers write theirs, so
    /// `calculate_cost` bills them at that level's rates and the value merges
    /// through `merge_usage_values` like any other. A whole row sits at one
    /// level because it is one request; level 0 is the model's base level and
    /// leaves nothing behind.
    pub(crate) fn into_value(self, tier_level: usize) -> Value {
        let mut value = json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_input_tokens": self.cache_read_tokens,
            "cache_creation_input_tokens": self.cache_creation_tokens,
            "reasoning_output_tokens": self.reasoning_tokens,
        });
        if tier_level == 0 {
            return value;
        }

        let mut above = serde_json::Map::with_capacity(4);
        write_tier_slice(
            &mut above,
            tier_level,
            &TierSlice {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                reasoning_tokens: self.reasoning_tokens,
                cache_read: self.cache_read_tokens,
                // These readers see one cache-write total with no TTL split,
                // which is the 5-minute default everywhere it is published.
                cache_creation_5m: self.cache_creation_tokens,
                cache_creation_1h: 0,
            },
        );
        if let (Some(object), false) = (value.as_object_mut(), above.is_empty()) {
            object.insert("above_tier".to_string(), Value::Object(above));
        }
        value
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
    /// Price level this row's own request context classified into, `0` being
    /// the model's base level. Only a reader whose rows are single requests
    /// can classify: a row that is already aggregated over several of them
    /// (Hermes's per-model table, Cursor's per-conversation gauge, OpenCode's
    /// legacy `session` fallback) carries `0` and bills at base rates, which
    /// is a lower bound rather than an over-report.
    pub(crate) tier_level: usize,
}

/// Database usage rows plus schema-normalization diagnostics.
#[derive(Debug)]
pub(crate) struct DatabaseUsageRead {
    pub(crate) rows: Vec<UsageContribution>,
    pub(crate) expected_records: usize,
    pub(crate) parsed_records: usize,
}

impl DatabaseUsageRead {
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
        tier_level: usize,
    ) -> Self {
        Self {
            date,
            timestamp_ms,
            model,
            tokens,
            stored_cost,
            tier_level,
        }
    }

    // This is the one route from a compact row into a public `CodeAnalysis`,
    // so `tier_level` reaching it decides whether `above_tier` appears in that
    // public JSON. It stays absent only because every caller of this route
    // reads with no thresholds; threading them into one would change the
    // public shape without touching this line.
    pub(crate) fn into_public_row(
        self,
        provider: ExtensionType,
        user: &str,
        machine: &str,
    ) -> (String, CodeAnalysis, f64) {
        let mut state = SessionParseState::with_mode(ParseMode::UsageOnly);
        state.last_ts = self.timestamp_ms;
        let mut usage = FastHashMap::default();
        usage.insert(self.model, self.tokens.into_value(self.tier_level));
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
