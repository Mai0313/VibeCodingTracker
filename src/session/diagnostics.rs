use crate::VERSION;
use crate::analysis::AggregatedAnalysisRow;
use crate::constants::FastHashMap;
use crate::models::{
    CodeAnalysis, CodeAnalysisApplyDiffDetail, CodeAnalysisReadDetail, CodeAnalysisRecord,
    CodeAnalysisRunCommandDetail, CodeAnalysisWriteDetail, ExtensionType,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{TokenCounts, extract_token_counts};
use serde_json::{Value, json};
use std::sync::Arc;

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

/// A normalized analysis value plus parser-only diagnostics.
#[derive(Debug)]
pub(crate) struct ParsedAnalysis {
    pub analysis: CodeAnalysis,
    pub diagnostics: ParseDiagnostics,
    pub usage_facts: Vec<UsageFact>,
    pub analysis_facts: Vec<AnalysisFact>,
}

/// Whether a token contribution represents one provider request or an
/// already-aggregated provider total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PricingGranularity {
    Request,
    Aggregate,
}

/// One model-specific pricing unit inside a provider usage event.
#[derive(Debug, Clone)]
pub(crate) struct UsageFactUnit {
    pub(crate) model: String,
    pub(crate) usage: Arc<Value>,
    pub(crate) counts: TokenCounts,
    pub(crate) stored_cost: Option<f64>,
    pub(crate) granularity: PricingGranularity,
    pub(crate) provider_pricing_modifiers: Vec<String>,
    pub(crate) analysis_presence: bool,
}

impl UsageFactUnit {
    pub(crate) fn from_value(
        model: String,
        usage: &Value,
        granularity: PricingGranularity,
    ) -> Self {
        Self {
            model,
            usage: Arc::new(usage.clone()),
            counts: extract_token_counts(usage),
            stored_cost: None,
            granularity,
            provider_pricing_modifiers: extract_provider_pricing_modifiers(usage),
            analysis_presence: true,
        }
    }

    pub(crate) fn from_counts(
        model: String,
        counts: TokenCounts,
        granularity: PricingGranularity,
    ) -> Self {
        let usage = json!({
            "input_tokens": counts.input_tokens,
            "output_tokens": counts.output_tokens,
            "reasoning_output_tokens": counts.reasoning_tokens,
            "cache_read_input_tokens": counts.cache_read,
            "cache_creation_input_tokens": counts.cache_creation,
            "cache_creation": {
                "ephemeral_5m_input_tokens": counts.cache_creation_5m,
                "ephemeral_1h_input_tokens": counts.cache_creation_1h,
            },
            "server_tool_use": {
                "web_search_requests": counts.web_search_requests,
            },
            "total_tokens": counts.total,
        });
        Self {
            model,
            usage: Arc::new(usage),
            counts,
            stored_cost: None,
            granularity,
            provider_pricing_modifiers: Vec::new(),
            analysis_presence: true,
        }
    }

    pub(crate) fn inherit_provider_pricing_modifiers(&mut self, usage: &Value) {
        let has_authoritative_metadata =
            usage.get("speed").is_some() || usage.get("inference_geo").is_some();
        if has_authoritative_metadata {
            self.provider_pricing_modifiers = extract_provider_pricing_modifiers(usage);
        }
    }
}

fn extract_provider_pricing_modifiers(usage: &Value) -> Vec<String> {
    let mut modifiers = Vec::with_capacity(2);
    if let Some(speed) = usage.get("speed").and_then(Value::as_str) {
        let speed = speed.trim().to_ascii_lowercase();
        if !speed.is_empty() && speed != "standard" {
            modifiers.push(speed);
        }
    }
    if let Some(inference_geo) = usage.get("inference_geo").and_then(Value::as_str) {
        let inference_geo = inference_geo.trim().to_ascii_lowercase();
        if !inference_geo.is_empty()
            && inference_geo != "global"
            && inference_geo != "not_available"
            && !modifiers.contains(&inference_geo)
        {
            modifiers.push(inference_geo);
        }
    }
    modifiers
}

/// One timestamped provider response. Stable ids stay private so batch
/// reducers can remove replayed or forked history without changing the public
/// `CodeAnalysis` JSON contract.
#[derive(Debug, Clone)]
pub(crate) struct UsageFact {
    pub(crate) stable_id: Option<String>,
    pub(crate) timestamp_ms: Option<i64>,
    pub(crate) observed_at_ms: Option<i64>,
    pub(crate) source_order: usize,
    pub(crate) units: Vec<UsageFactUnit>,
}

impl UsageFact {
    pub(crate) fn anonymous(
        timestamp_ms: i64,
        source_order: usize,
        units: Vec<UsageFactUnit>,
    ) -> Self {
        let timestamp_ms = (timestamp_ms > 0).then_some(timestamp_ms);
        Self {
            stable_id: None,
            timestamp_ms,
            observed_at_ms: timestamp_ms,
            source_order,
            units,
        }
    }
}

/// Compact tool counters and line totals retained by the summary cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AnalysisMetrics {
    pub(crate) edit_lines: usize,
    pub(crate) read_lines: usize,
    pub(crate) write_lines: usize,
    pub(crate) bash_count: usize,
    pub(crate) edit_count: usize,
    pub(crate) read_count: usize,
    pub(crate) todo_write_count: usize,
    pub(crate) write_count: usize,
}

impl AnalysisMetrics {
    pub(crate) fn from_record(record: &CodeAnalysisRecord) -> Self {
        Self {
            edit_lines: record.total_edit_lines,
            read_lines: record.total_read_lines,
            write_lines: record.total_write_lines,
            bash_count: record.tool_call_counts.bash,
            edit_count: record.tool_call_counts.edit,
            read_count: record.tool_call_counts.read,
            todo_write_count: record.tool_call_counts.todo_write,
            write_count: record.tool_call_counts.write,
        }
    }

    pub(crate) fn from_state(state: &SessionParseState) -> Self {
        Self {
            edit_lines: state.total_edit_lines,
            read_lines: state.total_read_lines,
            write_lines: state.total_write_lines,
            bash_count: state.tool_counts.bash,
            edit_count: state.tool_counts.edit,
            read_count: state.tool_counts.read,
            todo_write_count: state.tool_counts.todo_write,
            write_count: state.tool_counts.write,
        }
    }

    pub(crate) fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            edit_lines: self.edit_lines.saturating_sub(earlier.edit_lines),
            read_lines: self.read_lines.saturating_sub(earlier.read_lines),
            write_lines: self.write_lines.saturating_sub(earlier.write_lines),
            bash_count: self.bash_count.saturating_sub(earlier.bash_count),
            edit_count: self.edit_count.saturating_sub(earlier.edit_count),
            read_count: self.read_count.saturating_sub(earlier.read_count),
            todo_write_count: self
                .todo_write_count
                .saturating_sub(earlier.todo_write_count),
            write_count: self.write_count.saturating_sub(earlier.write_count),
        }
    }

    pub(crate) fn add_assign(&mut self, other: Self) {
        self.edit_lines += other.edit_lines;
        self.read_lines += other.read_lines;
        self.write_lines += other.write_lines;
        self.bash_count += other.bash_count;
        self.edit_count += other.edit_count;
        self.read_count += other.read_count;
        self.todo_write_count += other.todo_write_count;
        self.write_count += other.write_count;
    }

    pub(crate) fn into_row(self, model: String) -> AggregatedAnalysisRow {
        AggregatedAnalysisRow {
            model,
            edit_lines: self.edit_lines,
            read_lines: self.read_lines,
            write_lines: self.write_lines,
            bash_count: self.bash_count,
            edit_count: self.edit_count,
            read_count: self.read_count,
            todo_write_count: self.todo_write_count,
            write_count: self.write_count,
        }
    }

    pub(crate) fn has_activity(self) -> bool {
        self != Self::default()
    }
}

/// Lifecycle state used when the same provider tool id appears in more than
/// one session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ToolFactStatus {
    Pending,
    Failed,
    Succeeded,
}

/// One timestamped tool contribution attributed to the model active at
/// invocation time.
#[derive(Debug, Clone)]
pub(crate) struct AnalysisFact {
    pub(crate) stable_id: Option<String>,
    pub(crate) timestamp_ms: Option<i64>,
    pub(crate) observed_at_ms: Option<i64>,
    pub(crate) source_order: usize,
    pub(crate) model: String,
    pub(crate) status: ToolFactStatus,
    pub(crate) metrics: AnalysisMetrics,
    pub(crate) effect: Option<AnalysisFactEffect>,
}

/// Full successful-operation payload retained only for batch JSON
/// materialization. Compact summary paths consume [`AnalysisMetrics`] and do
/// not inspect this value.
#[derive(Debug, Clone, Default)]
pub(crate) struct AnalysisFactEffect {
    pub(crate) aggregate: bool,
    pub(crate) unique_files: Vec<String>,
    pub(crate) unknown_unique_files: usize,
    pub(crate) write_characters: usize,
    pub(crate) read_characters: usize,
    pub(crate) edit_characters: usize,
    pub(crate) write_file_details: Vec<CodeAnalysisWriteDetail>,
    pub(crate) read_file_details: Vec<CodeAnalysisReadDetail>,
    pub(crate) edit_file_details: Vec<CodeAnalysisApplyDiffDetail>,
    pub(crate) run_command_details: Vec<CodeAnalysisRunCommandDetail>,
}

impl AnalysisFactEffect {
    pub(crate) fn from_record(record: &CodeAnalysisRecord) -> Self {
        let mut unique_files = Vec::new();
        for path in record
            .write_file_details
            .iter()
            .map(|detail| &detail.base.file_path)
            .chain(
                record
                    .read_file_details
                    .iter()
                    .map(|detail| &detail.base.file_path),
            )
            .chain(
                record
                    .edit_file_details
                    .iter()
                    .map(|detail| &detail.base.file_path),
            )
        {
            if !path.is_empty() && !unique_files.contains(path) {
                unique_files.push(path.clone());
            }
        }
        Self {
            aggregate: true,
            unknown_unique_files: record.total_unique_files.saturating_sub(unique_files.len()),
            unique_files,
            write_characters: record.total_write_characters,
            read_characters: record.total_read_characters,
            edit_characters: record.total_edit_characters,
            write_file_details: record.write_file_details.clone(),
            read_file_details: record.read_file_details.clone(),
            edit_file_details: record.edit_file_details.clone(),
            run_command_details: record.run_command_details.clone(),
        }
    }

    pub(crate) fn add_assign(&mut self, mut other: Self) {
        for path in other.unique_files.drain(..) {
            if !path.is_empty() && !self.unique_files.contains(&path) {
                self.unique_files.push(path);
            }
        }
        self.unknown_unique_files += other.unknown_unique_files;
        self.write_characters += other.write_characters;
        self.read_characters += other.read_characters;
        self.edit_characters += other.edit_characters;
        self.write_file_details
            .append(&mut other.write_file_details);
        self.read_file_details.append(&mut other.read_file_details);
        self.edit_file_details.append(&mut other.edit_file_details);
        self.run_command_details
            .append(&mut other.run_command_details);
    }
}

/// Scalar and vector offsets used to capture one operation without cloning
/// the complete parser state before every tool result.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisStateSnapshot {
    write_characters: usize,
    read_characters: usize,
    edit_characters: usize,
    write_details: usize,
    read_details: usize,
    edit_details: usize,
    run_details: usize,
    unique_files: usize,
}

impl AnalysisStateSnapshot {
    pub(crate) fn capture(state: &SessionParseState) -> Self {
        Self {
            write_characters: state.total_write_characters,
            read_characters: state.total_read_characters,
            edit_characters: state.total_edit_characters,
            write_details: state.write_details.len(),
            read_details: state.read_details.len(),
            edit_details: state.edit_details.len(),
            run_details: state.run_details.len(),
            unique_files: state.unique_file_order.len(),
        }
    }

    pub(crate) fn effect_since(
        self,
        state: &SessionParseState,
        paths: impl IntoIterator<Item = String>,
    ) -> AnalysisFactEffect {
        let mut unique_files = Vec::new();
        for path in paths
            .into_iter()
            .chain(state.unique_file_order[self.unique_files..].iter().cloned())
            .chain(
                state.write_details[self.write_details..]
                    .iter()
                    .map(|detail| detail.base.file_path.clone()),
            )
            .chain(
                state.read_details[self.read_details..]
                    .iter()
                    .map(|detail| detail.base.file_path.clone()),
            )
            .chain(
                state.edit_details[self.edit_details..]
                    .iter()
                    .map(|detail| detail.base.file_path.clone()),
            )
        {
            if !path.is_empty() && !unique_files.contains(&path) {
                unique_files.push(path);
            }
        }
        AnalysisFactEffect {
            aggregate: false,
            unique_files,
            unknown_unique_files: 0,
            write_characters: state
                .total_write_characters
                .saturating_sub(self.write_characters),
            read_characters: state
                .total_read_characters
                .saturating_sub(self.read_characters),
            edit_characters: state
                .total_edit_characters
                .saturating_sub(self.edit_characters),
            write_file_details: state.write_details[self.write_details..].to_vec(),
            read_file_details: state.read_details[self.read_details..].to_vec(),
            edit_file_details: state.edit_details[self.edit_details..].to_vec(),
            run_command_details: state.run_details[self.run_details..].to_vec(),
        }
    }
}

/// Database-backed analysis row with a stable source identity for ordering.
pub(crate) struct DatabaseAnalysisRow {
    pub source_id: String,
    pub date: String,
    pub analysis: CodeAnalysis,
    pub analysis_facts: Vec<AnalysisFact>,
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
}

/// Compact token contribution produced by a database-backed usage reader.
#[derive(Debug)]
pub(crate) struct UsageContribution {
    pub(crate) date: String,
    pub(crate) timestamp_ms: i64,
    pub(crate) model: String,
    pub(crate) tokens: UsageTokenContribution,
    pub(crate) stored_cost: Option<f64>,
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
        stored_cost: Option<f64>,
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
        (self.date, analysis, self.stored_cost.unwrap_or(0.0))
    }
}

impl ParsedAnalysis {
    pub fn new(analysis: CodeAnalysis, diagnostics: ParseDiagnostics) -> Self {
        let (usage_facts, analysis_facts) = fallback_facts(&analysis);
        Self {
            analysis,
            diagnostics,
            usage_facts,
            analysis_facts,
        }
    }

    pub(crate) fn with_facts(
        mut self,
        usage_facts: Vec<UsageFact>,
        analysis_facts: Vec<AnalysisFact>,
    ) -> Self {
        self.usage_facts = usage_facts;
        self.analysis_facts = analysis_facts;
        self
    }
}

pub(crate) fn fallback_facts(analysis: &CodeAnalysis) -> (Vec<UsageFact>, Vec<AnalysisFact>) {
    let mut usage_facts = Vec::new();
    let mut analysis_facts = Vec::new();
    for (source_order, record) in analysis.records.iter().enumerate() {
        let timestamp_ms = (record.timestamp > 0).then_some(record.timestamp);
        let mut units =
            Vec::with_capacity(record.conversation_usage.len() + record.advisor_usage.len());
        for (model, usage) in &record.conversation_usage {
            units.push(UsageFactUnit::from_value(
                model.clone(),
                usage,
                PricingGranularity::Aggregate,
            ));
        }
        for (model, usage) in &record.advisor_usage {
            let mut unit =
                UsageFactUnit::from_value(model.clone(), usage, PricingGranularity::Aggregate);
            unit.analysis_presence = false;
            units.push(unit);
        }
        if !units.is_empty() {
            usage_facts.push(UsageFact {
                stable_id: None,
                timestamp_ms,
                observed_at_ms: timestamp_ms,
                source_order,
                units,
            });
        }

        let metrics = AnalysisMetrics::from_record(record);
        let mut models: Vec<_> = record
            .conversation_usage
            .keys()
            .filter(|model| !model.contains("<synthetic>"))
            .cloned()
            .collect();
        if metrics.has_activity() {
            if models.is_empty() || (models.len() > 1 && metrics.has_activity()) {
                models.clear();
                models.push(String::new());
            }
            for model in models {
                analysis_facts.push(AnalysisFact {
                    stable_id: None,
                    timestamp_ms,
                    observed_at_ms: timestamp_ms,
                    source_order,
                    model,
                    status: ToolFactStatus::Succeeded,
                    metrics,
                    effect: Some(AnalysisFactEffect::from_record(record)),
                });
            }
        }
    }
    (usage_facts, analysis_facts)
}
