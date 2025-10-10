use std::collections::{BTreeMap, HashMap};

/// Chronologically sorted token usage data by date and model
///
/// Structure: Date (YYYY-MM-DD) -> Model Name -> Usage Metrics
/// - Uses BTreeMap for automatic chronological sorting
/// - Usage format varies by provider:
///   * Claude/Gemini: `{ input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens }`
///   * Codex: `{ total_token_usage: { input_tokens, output_tokens } }`
pub type DateUsageResult = BTreeMap<String, HashMap<String, serde_json::Value>>;
