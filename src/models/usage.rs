use std::collections::{BTreeMap, HashMap};

/// Date-based usage result: maps date -> model -> usage data
/// Uses BTreeMap for automatic chronological sorting by date (YYYY-MM-DD format)
/// Usage data format varies by extension type:
/// - Claude/Gemini: { input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, ... }
/// - Codex: { total_token_usage: { input_tokens, output_tokens, ... }, ... }
pub type DateUsageResult = BTreeMap<String, HashMap<String, serde_json::Value>>;
