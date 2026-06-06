//! OpenCode session reader (SQLite, not JSONL).
//!
//! Unlike the four file-based providers, OpenCode stores every session in a
//! single SQLite database at `~/.local/share/opencode/opencode.db` (WAL mode).
//! This module owns the "SQLite rows -> typed [`CodeAnalysis`]" boundary, so
//! both the `usage` and `analysis` aggregators consume the same shape the
//! file-based providers produce.
//!
//! Two entry points keep the work proportional to what each command needs:
//!
//! - [`read_opencode_usage`] reads assistant messages for per-model tokens and
//!   cost, with an older `session`-table fallback.
//! - [`read_opencode_analysis`] additionally folds the `part` table's tool
//!   calls (`read`, `edit`, `write`, `bash`, `todowrite`) into
//!   per-message file-operation metrics.
//!
//! Token columns map onto the Claude-style flat usage shape so the existing
//! `merge_usage_values` / `extract_token_counts` / LiteLLM cost path works
//! unchanged. Assistant messages carry their own `providerID` + `modelID`, so
//! sessions that switch model mid-stream are split before aggregation.

use crate::VERSION;
use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, ExtensionType};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Reads per-session token usage from the OpenCode database.
///
/// Each returned tuple is `(local YYYY-MM-DD date, CodeAnalysis, stored_cost)`
/// where the `CodeAnalysis` holds one assistant message's
/// `conversation_usage`, keyed by that message's provider-qualified model id,
/// and `stored_cost` is OpenCode's own cost for that message. The date comes
/// from the assistant message timestamp and is filtered by `time_range`,
/// matching the file-walker semantics.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
pub fn read_opencode_usage(
    db_path: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    with_connection(db_path, |conn| collect_usage(conn, time_range))
}

/// Reads per-session file-operation metrics from the OpenCode database.
///
/// Like [`read_opencode_usage`], but also folds each session's tool calls from
/// the `part` table into `tool_call_counts` and the `total_*` line/character
/// counts. `mode` controls whether the heavy per-operation detail bodies are
/// retained ([`ParseMode::Full`]) or skipped ([`ParseMode::UsageOnly`]); the
/// aggregated `analysis` view uses `UsageOnly`.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
pub fn read_opencode_analysis(
    db_path: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    with_connection(db_path, |conn| collect_analysis(conn, time_range, mode))
}

/// Parsed usage for one OpenCode assistant message.
struct MessageUsage {
    model_id: String,
    usage: Value,
    cost: f64,
    timestamp: Option<i64>,
}

/// Per-record accumulator used while folding tool parts.
struct AnalysisAccum {
    model_id: String,
    date: String,
    usage: Value,
    state: SessionParseState,
}

/// Collects the `usage` view from assistant messages when available.
fn collect_usage(
    conn: &Connection,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    if table_exists(conn, "message")? {
        return collect_message_usage(conn, time_range);
    }

    collect_session_usage(conn, time_range)
}

/// Collects the `usage` view from the legacy `session` columns.
fn collect_session_usage(
    conn: &Connection,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);
    let cutoff_ms = cutoff_millis(time_range);

    let sql = match cutoff_ms {
        Some(_) => {
            "SELECT model, tokens_input, tokens_output, tokens_reasoning, \
                    tokens_cache_read, tokens_cache_write, time_updated, cost \
             FROM session WHERE model IS NOT NULL AND model != '' AND time_updated >= ?1"
        }
        None => {
            "SELECT model, tokens_input, tokens_output, tokens_reasoning, \
                    tokens_cache_read, tokens_cache_write, time_updated, cost \
             FROM session WHERE model IS NOT NULL AND model != ''"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = match cutoff_ms {
        Some(cutoff_ms) => stmt.query([cutoff_ms])?,
        None => stmt.query([])?,
    };

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let model = row.get::<_, String>(0)?;
        let input = row.get::<_, i64>(1)?;
        let output = row.get::<_, i64>(2)?;
        let reasoning = row.get::<_, i64>(3)?;
        let cache_read = row.get::<_, i64>(4)?;
        let cache_write = row.get::<_, i64>(5)?;
        let time_updated = row.get::<_, i64>(6)?;
        let cost = row.get::<_, f64>(7)?;
        let Some(model_id) = parse_model_id(&model) else {
            continue;
        };
        let Some(date) = ms_to_local_date(time_updated) else {
            continue;
        };
        if is_before_cutoff(&date, &cutoff) {
            continue;
        }

        let usage = session_usage_value(input, output, reasoning, cache_read, cache_write);
        let mut map = FastHashMap::default();
        map.insert(model_id, usage);

        let mut state = SessionParseState::with_mode(ParseMode::UsageOnly);
        state.last_ts = time_updated;
        out.push((
            date,
            wrap_record(state.into_record(map), &user, &machine),
            cost,
        ));
    }

    Ok(out)
}

/// Collects the `usage` view from assistant messages.
fn collect_message_usage(
    conn: &Connection,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);
    let cutoff_ms = cutoff_millis(time_range);

    let sql = match cutoff_ms {
        Some(_) => {
            "SELECT session.time_updated, message.data \
             FROM message \
             JOIN session ON session.id = message.session_id \
             WHERE json_extract(message.data, '$.role') = 'assistant' \
               AND COALESCE( \
                   json_extract(message.data, '$.time.completed'), \
                   json_extract(message.data, '$.time.created'), \
                   session.time_updated \
               ) >= ?1"
        }
        None => {
            "SELECT session.time_updated, message.data \
             FROM message \
             JOIN session ON session.id = message.session_id \
             WHERE json_extract(message.data, '$.role') = 'assistant'"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = match cutoff_ms {
        Some(cutoff_ms) => stmt.query([cutoff_ms])?,
        None => stmt.query([])?,
    };

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let session_ts = row.get::<_, i64>(0)?;
        let data_text = row.get::<_, String>(1)?;
        let Some(message) = parse_message_usage(&data_text) else {
            continue;
        };
        let message_ts = message.timestamp.unwrap_or(session_ts);
        let Some(date) = ms_to_local_date(message_ts) else {
            continue;
        };
        if is_before_cutoff(&date, &cutoff) {
            continue;
        }

        let mut map = FastHashMap::default();
        map.insert(message.model_id, message.usage);

        let mut state = SessionParseState::with_mode(ParseMode::UsageOnly);
        state.last_ts = message_ts;
        out.push((
            date,
            wrap_record(state.into_record(map), &user, &machine),
            message.cost,
        ));
    }

    Ok(out)
}

/// Collects the `analysis` view from assistant messages + parts when available.
fn collect_analysis(
    conn: &Connection,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    if table_exists(conn, "message")? {
        return collect_message_analysis(conn, time_range, mode);
    }

    collect_session_analysis(conn, time_range, mode)
}

/// Collects the legacy `analysis` view from `session` + `part`.
fn collect_session_analysis(
    conn: &Connection,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);
    let cutoff_ms = cutoff_millis(time_range);

    // 1. Load session metadata and seed one parse state per session.
    let mut sessions: HashMap<String, AnalysisAccum> = HashMap::new();
    {
        let sql = match cutoff_ms {
            Some(_) => {
                "SELECT id, model, directory, time_updated, tokens_input, tokens_output, \
                        tokens_reasoning, tokens_cache_read, tokens_cache_write \
                 FROM session \
                 WHERE model IS NOT NULL AND model != '' AND time_updated >= ?1"
            }
            None => {
                "SELECT id, model, directory, time_updated, tokens_input, tokens_output, \
                        tokens_reasoning, tokens_cache_read, tokens_cache_write \
                 FROM session WHERE model IS NOT NULL AND model != ''"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let id = row.get::<_, String>(0)?;
            let model = row.get::<_, String>(1)?;
            let directory = row.get::<_, String>(2)?;
            let ts = row.get::<_, i64>(3)?;
            let input = row.get::<_, i64>(4)?;
            let output = row.get::<_, i64>(5)?;
            let reasoning = row.get::<_, i64>(6)?;
            let cache_read = row.get::<_, i64>(7)?;
            let cache_write = row.get::<_, i64>(8)?;
            let Some(model_id) = parse_model_id(&model) else {
                continue;
            };
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };

            let usage = session_usage_value(input, output, reasoning, cache_read, cache_write);
            let mut state = SessionParseState::with_mode(mode);
            state.folder_path = directory;
            state.task_id = id.clone();
            state.last_ts = ts;

            sessions.insert(
                id,
                AnalysisAccum {
                    model_id,
                    date,
                    usage,
                    state,
                },
            );
        }
    }

    // 2. Fold tool parts into their owning session's parse state.
    {
        let sql = match cutoff_ms {
            Some(_) => {
                "SELECT part.session_id, part.data \
                 FROM part \
                 JOIN session ON session.id = part.session_id \
                 WHERE json_extract(part.data, '$.type') = 'tool' \
                   AND session.model IS NOT NULL \
                   AND session.model != '' \
                   AND session.time_updated >= ?1"
            }
            None => {
                "SELECT session_id, data \
                 FROM part \
                 WHERE json_extract(data, '$.type') = 'tool'"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let session_id = row.get::<_, String>(0)?;
            let data_text = row.get::<_, String>(1)?;
            let Some(accum) = sessions.get_mut(&session_id) else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&data_text) else {
                continue;
            };
            if let Some("tool") = data.get("type").and_then(|v| v.as_str()) {
                apply_tool_part(&mut accum.state, &data)
            }
        }
    }

    // 3. Convert each session into a CodeAnalysis, honouring the time filter.
    let mut out = Vec::with_capacity(sessions.len());
    for (_id, accum) in sessions {
        if is_before_cutoff(&accum.date, &cutoff) {
            continue;
        }
        let mut usage_map = FastHashMap::default();
        usage_map.insert(accum.model_id, accum.usage);
        let record = accum.state.into_record(usage_map);
        out.push((accum.date, wrap_record(record, &user, &machine)));
    }

    Ok(out)
}

/// Collects the `analysis` view from assistant messages and their parts.
fn collect_message_analysis(
    conn: &Connection,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);
    let cutoff_ms = cutoff_millis(time_range);

    let mut messages: HashMap<String, AnalysisAccum> = HashMap::new();
    {
        let sql = match cutoff_ms {
            Some(_) => {
                "SELECT message.id, message.session_id, message.data, session.directory, session.time_updated \
                 FROM message \
                 JOIN session ON session.id = message.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant' \
                   AND COALESCE( \
                       json_extract(message.data, '$.time.completed'), \
                       json_extract(message.data, '$.time.created'), \
                       session.time_updated \
                   ) >= ?1"
            }
            None => {
                "SELECT message.id, message.session_id, message.data, session.directory, session.time_updated \
                 FROM message \
                 JOIN session ON session.id = message.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant'"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let message_id = row.get::<_, String>(0)?;
            let session_id = row.get::<_, String>(1)?;
            let data_text = row.get::<_, String>(2)?;
            let directory = row.get::<_, String>(3)?;
            let session_ts = row.get::<_, i64>(4)?;
            let Some(message) = parse_message_usage(&data_text) else {
                continue;
            };
            let message_ts = message.timestamp.unwrap_or(session_ts);
            let Some(date) = ms_to_local_date(message_ts) else {
                continue;
            };
            if is_before_cutoff(&date, &cutoff) {
                continue;
            }

            let mut state = SessionParseState::with_mode(mode);
            state.folder_path = directory;
            state.task_id = session_id;
            state.last_ts = message_ts;

            messages.insert(
                message_id,
                AnalysisAccum {
                    model_id: message.model_id,
                    date,
                    usage: message.usage,
                    state,
                },
            );
        }
    }

    {
        let sql = match cutoff_ms {
            Some(_) => {
                "SELECT part.message_id, part.data \
                 FROM part \
                 JOIN message ON message.id = part.message_id \
                 JOIN session ON session.id = part.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant' \
                   AND json_extract(part.data, '$.type') = 'tool' \
                   AND COALESCE( \
                       json_extract(message.data, '$.time.completed'), \
                       json_extract(message.data, '$.time.created'), \
                       session.time_updated \
                   ) >= ?1"
            }
            None => {
                "SELECT part.message_id, part.data \
                 FROM part \
                 JOIN message ON message.id = part.message_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant' \
                   AND json_extract(part.data, '$.type') = 'tool'"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let message_id = row.get::<_, String>(0)?;
            let data_text = row.get::<_, String>(1)?;
            let Some(accum) = messages.get_mut(&message_id) else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&data_text) else {
                continue;
            };
            if let Some("tool") = data.get("type").and_then(|v| v.as_str()) {
                apply_tool_part(&mut accum.state, &data)
            }
        }
    }

    let mut out = Vec::with_capacity(messages.len());
    for (_id, accum) in messages {
        if is_before_cutoff(&accum.date, &cutoff) {
            continue;
        }
        let mut usage_map = FastHashMap::default();
        usage_map.insert(accum.model_id, accum.usage);
        let record = accum.state.into_record(usage_map);
        out.push((accum.date, wrap_record(record, &user, &machine)));
    }

    Ok(out)
}

/// Dispatches a single `part` (type `tool`) onto the session parse state.
///
/// Only the tools the analyzer tracks across providers are folded in
/// (`read`, `edit`, `write`, `bash`, `todowrite`, `apply_patch`); auxiliary
/// tools such as `task`, `grep`, `glob`, `webfetch`, and `question` are ignored
/// to stay consistent with the other providers' tool-count semantics.
fn apply_tool_part(state: &mut SessionParseState, data: &Value) {
    let tool = data.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let st = data.get("state");
    if st.and_then(|s| s.get("status")).and_then(|v| v.as_str()) != Some("completed") {
        return;
    }
    let input = st.and_then(|s| s.get("input"));
    let ts = st
        .and_then(|s| s.get("time"))
        .and_then(|t| t.get("start"))
        .and_then(|v| v.as_i64())
        .unwrap_or(state.last_ts);

    let str_in = |key: &str| -> &str {
        input
            .and_then(|i| i.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };

    match tool {
        "read" => {
            let path = str_in("filePath");
            let output = st
                .and_then(|s| s.get("output"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = extract_read_content(output);
            if !path.is_empty() && !content.is_empty() {
                state.add_read_detail(path, &content, ts);
            } else {
                // Directory listings (and empty reads) still count as a read
                // invocation even though they contribute no file lines.
                state.tool_counts.read += 1;
            }
        }
        "edit" => {
            let path = str_in("filePath");
            state.add_edit_detail(path, str_in("oldString"), str_in("newString"), ts);
        }
        "write" => {
            let path = str_in("filePath");
            state.add_write_detail(path, str_in("content"), ts);
        }
        "bash" => {
            state.add_run_command(str_in("command"), str_in("description"), ts);
        }
        "todowrite" => {
            state.tool_counts.todo_write += 1;
        }
        "apply_patch" => {
            apply_patch_text(state, str_in("patchText"), ts);
        }
        _ => {}
    }
}

/// Folds an OpenCode `apply_patch` tool input into file-operation counts.
fn apply_patch_text(state: &mut SessionParseState, patch_text: &str, ts: i64) {
    for patch in parse_apply_patch_text(patch_text) {
        let (old_str, new_str) = extract_patch_strings(&patch.lines);

        match patch.action.as_str() {
            "add" => state.add_write_detail(&patch.file_path, &new_str, ts),
            "delete" => state.add_edit_detail(&patch.file_path, &old_str, "", ts),
            _ => state.add_edit_detail(&patch.file_path, &old_str, &new_str, ts),
        }
    }
}

/// One file hunk extracted from an OpenCode `apply_patch` tool call.
struct OpenCodePatch {
    action: String,
    file_path: String,
    lines: Vec<String>,
}

/// Parses the `*** Begin Patch` format carried by `state.input.patchText`.
fn parse_apply_patch_text(patch_text: &str) -> Vec<OpenCodePatch> {
    let start = match patch_text.find("*** Begin Patch") {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    let mut patches = Vec::with_capacity(3);
    let mut current: Option<OpenCodePatch> = None;
    for line in patch_text[start..].lines() {
        let line = line.trim_end_matches('\r');

        if line.starts_with("*** End Patch") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            break;
        } else if line.starts_with("*** Begin Patch") {
            continue;
        } else if line.starts_with("*** Update File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            current = Some(OpenCodePatch {
                action: "update".to_string(),
                file_path: line
                    .trim_start_matches("*** Update File:")
                    .trim()
                    .to_string(),
                lines: Vec::with_capacity(20),
            });
        } else if line.starts_with("*** Add File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            current = Some(OpenCodePatch {
                action: "add".to_string(),
                file_path: line.trim_start_matches("*** Add File:").trim().to_string(),
                lines: Vec::with_capacity(20),
            });
        } else if line.starts_with("*** Delete File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            current = Some(OpenCodePatch {
                action: "delete".to_string(),
                file_path: line
                    .trim_start_matches("*** Delete File:")
                    .trim()
                    .to_string(),
                lines: Vec::with_capacity(20),
            });
        } else if line.starts_with("*** Move to:") {
            // Rename/move marker: attribute the hunk to the destination path.
            if let Some(ref mut patch) = current {
                patch.file_path = line.trim_start_matches("*** Move to:").trim().to_string();
            }
        } else if let Some(ref mut patch) = current {
            patch.lines.push(line.to_string());
        }
    }

    if let Some(patch) = current {
        patches.push(patch);
    }
    patches
}

/// Splits diff lines into old and new text, skipping hunk headers.
fn extract_patch_strings(lines: &[String]) -> (String, String) {
    let estimated_size = lines.iter().map(|l| l.len()).sum::<usize>();
    let mut old_str = String::with_capacity(estimated_size / 2);
    let mut new_str = String::with_capacity(estimated_size / 2);

    for line in lines {
        if line.is_empty() || line.starts_with("@@") {
            continue;
        }

        match line.as_bytes()[0] {
            b'+' => {
                new_str.push_str(&line[1..]);
                new_str.push('\n');
            }
            b'-' => {
                old_str.push_str(&line[1..]);
                old_str.push('\n');
            }
            b'\\' => continue,
            _ => {}
        }
    }

    old_str.truncate(old_str.trim_end_matches('\n').len());
    new_str.truncate(new_str.trim_end_matches('\n').len());
    (old_str, new_str)
}

/// Builds the Claude-style flat usage value from a session's token columns.
///
/// OpenCode records non-cached input separately from cache reads (input is
/// disjoint from cache, matching the Claude convention), so the columns map
/// straight onto the field names `extract_token_counts` understands.
fn session_usage_value(
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_input_tokens": cache_read,
        "cache_creation_input_tokens": cache_write,
        "reasoning_output_tokens": reasoning,
    })
}

/// Resolves the model name from the `session.model` column.
///
/// Modern OpenCode stores it as a JSON object `{"id", "providerID", ...}`. When
/// `providerID` is present, the returned key is `providerID/id` so same-named
/// models from different backends do not merge. Older builds may store a bare
/// model string, which is used verbatim. Returns `None` when no usable model
/// name is present.
fn parse_model_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => {
            let model = map.get("id").and_then(|v| v.as_str())?.trim();
            if model.is_empty() {
                return None;
            }
            Some(provider_qualified_model_id(
                map.get("providerID")
                    .or_else(|| map.get("provider_id"))
                    .and_then(|v| v.as_str()),
                model,
            ))
        }
        Ok(Value::String(s)) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        // Not valid JSON: treat the column as a plain model name.
        _ => Some(raw.to_string()),
    }
}

/// Parses one assistant `message.data` payload into usage and cost.
fn parse_message_usage(raw: &str) -> Option<MessageUsage> {
    let data = serde_json::from_str::<Value>(raw).ok()?;
    if data.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }

    let model_id = message_model_id(&data)?;
    let tokens = data.get("tokens")?;
    let cache = tokens.get("cache");
    let usage = session_usage_value(
        tokens.get("input").and_then(|v| v.as_i64()).unwrap_or(0),
        tokens.get("output").and_then(|v| v.as_i64()).unwrap_or(0),
        tokens
            .get("reasoning")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        cache
            .and_then(|v| v.get("read"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        cache
            .and_then(|v| v.get("write"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
    );
    let cost = data.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let timestamp = data
        .get("time")
        .and_then(|v| v.get("completed").or_else(|| v.get("created")))
        .and_then(|v| v.as_i64());

    Some(MessageUsage {
        model_id,
        usage,
        cost,
        timestamp,
    })
}

/// Resolves the model from an assistant message payload.
fn message_model_id(data: &Value) -> Option<String> {
    let model = data
        .get("modelID")
        .or_else(|| data.get("model_id"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            data.get("model")
                .and_then(|v| v.get("modelID").or_else(|| v.get("id")))
                .and_then(|v| v.as_str())
        })?
        .trim();
    if model.is_empty() {
        return None;
    }

    Some(provider_qualified_model_id(
        data.get("providerID")
            .or_else(|| data.get("provider_id"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("model")
                    .and_then(|v| v.get("providerID").or_else(|| v.get("provider_id")))
                    .and_then(|v| v.as_str())
            }),
        model,
    ))
}

/// Keeps OpenCode model keys provider-qualified when the payload has provider metadata.
fn provider_qualified_model_id(provider: Option<&str>, model: &str) -> String {
    let Some(provider) = provider.map(str::trim).filter(|s| !s.is_empty()) else {
        return model.to_string();
    };
    if model.starts_with(&format!("{provider}/")) {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

/// Converts a millisecond epoch timestamp into a local `YYYY-MM-DD` date.
fn ms_to_local_date(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    })
}

/// Pre-computes the `YYYY-MM-DD` cutoff for `time_range`, if any.
fn cutoff_string(time_range: TimeRange) -> Option<String> {
    time_range
        .cutoff_date()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

/// Converts the inclusive local-date cutoff into an epoch-millis lower bound.
fn cutoff_millis(time_range: TimeRange) -> Option<i64> {
    use chrono::{Datelike, TimeZone};

    time_range.cutoff_date().and_then(|date| {
        chrono::Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .earliest()
            .map(|dt| dt.timestamp_millis())
    })
}

/// Returns `true` when `date` is strictly before the cutoff (should be skipped).
fn is_before_cutoff(date: &str, cutoff: &Option<String>) -> bool {
    matches!(cutoff, Some(c) if date < c.as_str())
}

/// Returns whether a table exists in the OpenCode database.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Extracts the file body from an OpenCode `read` tool output.
fn extract_read_content(output: &str) -> String {
    let Some(start) = output.find("<content>") else {
        return extract_plain_read_content(output);
    };
    let after = &output[start + "<content>".len()..];
    let inner = match after.find("</content>") {
        Some(end) => &after[..end],
        None => after,
    };
    let inner = inner.strip_prefix('\n').unwrap_or(inner);

    strip_numbered_content_lines(inner)
}

/// Extracts current OpenCode plain read output: `path\nfile\n\n1: line`.
fn extract_plain_read_content(output: &str) -> String {
    let mut lines = output.split('\n');
    let Some(_path) = lines.next() else {
        return String::new();
    };
    let Some(kind) = lines.next() else {
        return String::new();
    };
    if kind.trim_end_matches('\r') != "file" {
        return String::new();
    }

    let mut content = Vec::new();
    let mut saw_separator = false;
    let mut saw_content = false;
    for line in lines {
        if !saw_separator {
            if line.trim_end_matches('\r').is_empty() {
                saw_separator = true;
            }
            continue;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if has_line_number_prefix(line) {
            content.push(line);
            saw_content = true;
        } else if saw_content {
            break;
        }
    }

    if saw_separator {
        strip_numbered_content_lines_from_iter(content)
    } else {
        String::new()
    }
}

fn strip_numbered_content_lines(inner: &str) -> String {
    strip_numbered_content_lines_from_iter(inner.split('\n').collect())
}

fn strip_numbered_content_lines_from_iter(lines: Vec<&str>) -> String {
    let mut lines: Vec<&str> = lines
        .into_iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .map(strip_line_number_prefix)
        .collect();

    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Returns whether a line starts with OpenCode's `N: ` read-output prefix.
fn has_line_number_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > 0 && i + 1 < bytes.len() && bytes[i] == b':' && bytes[i + 1] == b' '
}

/// Strips a leading `"<digits>: "` line-number prefix, if present.
fn strip_line_number_prefix(line: &str) -> &str {
    let Some((prefix, content)) = line.split_once(": ") else {
        return line;
    };
    if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
        content
    } else {
        line
    }
}

/// Wraps a single record into a fully-populated [`CodeAnalysis`].
fn wrap_record(record: CodeAnalysisRecord, user: &str, machine: &str) -> CodeAnalysis {
    CodeAnalysis {
        user: user.to_string(),
        extension_name: ExtensionType::OpenCode.to_string(),
        insights_version: VERSION.to_string(),
        machine_id: machine.to_string(),
        records: vec![record],
    }
}

/// Opens the OpenCode DB read-only and runs `f` against it.
///
/// The primary path opens the database read-only so the user's file is never
/// mutated. A read-only connection reads a WAL database fine on modern SQLite,
/// but if that ever fails (e.g. the WAL needs recovery and cannot be locked
/// read-only) we fall back to copying the database plus its `-wal`/`-shm`
/// sidecars into a private temp directory and reading the copy. The temp copy
/// is removed when `f` returns.
fn with_connection<T>(db_path: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    if let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        && conn
            .query_row("SELECT count(*) FROM session", [], |_| Ok(()))
            .is_ok()
    {
        return f(&conn);
    }

    // Fallback: read from a throwaway copy that includes the WAL sidecars.
    let copy = TempDbCopy::new(db_path)?;
    let conn = Connection::open_with_flags(&copy.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
            format!(
                "Failed to open OpenCode DB copy at {}",
                copy.db_path.display()
            )
        })?;
    f(&conn)
}

/// A private temp-directory copy of the OpenCode database (plus WAL sidecars),
/// removed on drop.
///
/// The directory comes from [`tempfile::TempDir`], so it has an unguessable
/// name and owner-only permissions: the copied chat database is never exposed
/// to other local users.
struct TempDbCopy {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
}

impl TempDbCopy {
    fn new(src: &Path) -> Result<Self> {
        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid OpenCode DB path: {}", src.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("vct-opencode-")
            .tempdir()
            .context("Failed to create temp dir for OpenCode DB copy")?;

        let db_path = dir.path().join(file_name);
        std::fs::copy(src, &db_path)
            .with_context(|| format!("Failed to copy OpenCode DB from {}", src.display()))?;

        // Best-effort copy of the WAL sidecars so recently committed rows are
        // visible; absence is fine for a checkpointed database.
        for suffix in ["-wal", "-shm"] {
            let sidecar = append_suffix(src, suffix);
            if sidecar.exists() {
                let _ = std::fs::copy(&sidecar, append_suffix(&db_path, suffix));
            }
        }

        Ok(Self { _dir: dir, db_path })
    }
}

/// Appends a raw suffix to a path's final component (e.g. `db` -> `db-wal`).
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os: OsString = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_MESSAGE_TS: i64 = 1780757089000;

    fn make_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 model TEXT,
                 directory TEXT,
                 time_updated INTEGER NOT NULL,
                 cost REAL NOT NULL DEFAULT 0,
                 tokens_input INTEGER NOT NULL DEFAULT 0,
                 tokens_output INTEGER NOT NULL DEFAULT 0,
	                 tokens_reasoning INTEGER NOT NULL DEFAULT 0,
	                 tokens_cache_read INTEGER NOT NULL DEFAULT 0,
	                 tokens_cache_write INTEGER NOT NULL DEFAULT 0
	             );
	             CREATE TABLE message (
	                 id TEXT PRIMARY KEY,
	                 session_id TEXT NOT NULL,
	                 time_created INTEGER NOT NULL DEFAULT 0,
	                 time_updated INTEGER NOT NULL DEFAULT 0,
	                 data TEXT NOT NULL
	             );
		             CREATE TABLE part (
		                 id TEXT PRIMARY KEY,
		                 message_id TEXT NOT NULL DEFAULT '',
		                 session_id TEXT NOT NULL,
		                 time_updated INTEGER NOT NULL DEFAULT 0,
		                 data TEXT NOT NULL
		             );",
        )
        .unwrap();
        (dir, db_path)
    }

    fn assistant_message(
        model: &str,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
    ) -> String {
        assistant_message_with_provider(
            model,
            None,
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            cost,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assistant_message_at(
        model: &str,
        timestamp: i64,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
    ) -> String {
        assistant_message_with_provider_at(
            model,
            None,
            timestamp,
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            cost,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assistant_message_with_provider(
        model: &str,
        provider: Option<&str>,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
    ) -> String {
        assistant_message_with_provider_at(
            model,
            provider,
            DEFAULT_MESSAGE_TS,
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            cost,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assistant_message_with_provider_at(
        model: &str,
        provider: Option<&str>,
        timestamp: i64,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
    ) -> String {
        let mut message = serde_json::json!({
            "role": "assistant",
            "modelID": model,
            "cost": cost,
            "tokens": {
                "input": input,
                "output": output,
                "reasoning": reasoning,
                "cache": {
                    "read": cache_read,
                    "write": cache_write,
                },
            },
            "time": {
                "created": timestamp.saturating_sub(1000),
                "completed": timestamp,
            },
        });
        if let Some(provider) = provider {
            message["providerID"] = serde_json::Value::String(provider.to_string());
        }
        message.to_string()
    }

    #[test]
    fn test_parse_model_id() {
        assert_eq!(
            parse_model_id(r#"{"id":"deepseek-v4-pro","providerID":"deepseek"}"#).as_deref(),
            Some("deepseek/deepseek-v4-pro")
        );
        assert_eq!(
            parse_model_id("gemini-3.5-flash").as_deref(),
            Some("gemini-3.5-flash")
        );
        assert_eq!(parse_model_id(r#"{"providerID":"x"}"#), None);
        assert_eq!(parse_model_id("   "), None);
    }

    #[test]
    fn test_message_model_id_preserves_provider() {
        let message = serde_json::json!({
            "role": "assistant",
            "modelID": "gpt-4.1",
            "providerID": "azure",
        });
        assert_eq!(message_model_id(&message).as_deref(), Some("azure/gpt-4.1"));
    }

    #[test]
    fn test_extract_read_content() {
        let output = "<path>/a/b.py</path>\n<type>file</type>\n<content>\n1: import os\n2: \n3: print(1)\n</content>";
        assert_eq!(extract_read_content(output), "import os\n\nprint(1)");

        let plain_output = "/a/b.py\nfile\n\n1: import os\n2: \n3: print(1)";
        assert_eq!(extract_read_content(plain_output), "import os\n\nprint(1)");

        let plain_output_with_footer =
            "/a/b.py\nfile\n\n1: import os\n2: \n3: print(1)\n\n(End of file - total 3 lines)";
        assert_eq!(
            extract_read_content(plain_output_with_footer),
            "import os\n\nprint(1)"
        );

        // Directory listing has no <content> block.
        let dir_output = "<path>/a</path>\n<type>directory</type>\n<entries>\nx.py\n</entries>";
        assert_eq!(extract_read_content(dir_output), "");

        let plain_dir_output = "/a\ndirectory\n\nx.py";
        assert_eq!(extract_read_content(plain_dir_output), "");
    }

    #[test]
    fn test_strip_line_number_prefix() {
        assert_eq!(strip_line_number_prefix("12: hello"), "hello");
        assert_eq!(strip_line_number_prefix("1: 2: nested"), "2: nested");
        assert_eq!(strip_line_number_prefix("no prefix"), "no prefix");
    }

    #[test]
    fn test_read_usage_maps_tokens() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write)
	             VALUES ('s1', '{\"id\":\"deepseek-v4-pro\"}', '/repo', 1780757088080, 0.0375, 100, 50, 7, 200, 25)",
	            [],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message_with_provider(
                "deepseek-v4-pro",
                Some("deepseek"),
                100,
                50,
                7,
                200,
                25,
                0.0375,
            )],
        )
        .unwrap();
        drop(conn);

        let rows = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(rows.len(), 1);
        let (_date, analysis, cost) = &rows[0];
        assert_eq!(analysis.extension_name, "OpenCode");
        assert!((cost - 0.0375).abs() < 1e-9);
        let usage = &analysis.records[0].conversation_usage["deepseek/deepseek-v4-pro"];
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 50);
        assert_eq!(usage["reasoning_output_tokens"], 7);
        assert_eq!(usage["cache_read_input_tokens"], 200);
        assert_eq!(usage["cache_creation_input_tokens"], 25);
    }

    #[test]
    fn test_time_range_filters_old_sessions() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        let now_ms = chrono::Local::now().timestamp_millis();
        // One session today, one well in the past.
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated, tokens_input) VALUES ('recent', '{\"id\":\"m1\"}', '/repo', ?1, 10)",
	            rusqlite::params![now_ms],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('recent-msg', 'recent', ?1)",
            [assistant_message_at("m1", now_ms, 10, 0, 0, 0, 0, 0.01)],
        )
        .unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated, tokens_input) VALUES ('old', '{\"id\":\"m1\"}', '/repo', 1000000000000, 10)",
	            [],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('old-msg', 'old', ?1)",
            [assistant_message_at(
                "m1",
                1000000000000,
                10,
                0,
                0,
                0,
                0,
                0.01,
            )],
        )
        .unwrap();
        drop(conn);

        let all = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(all.len(), 2);

        let daily = read_opencode_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(daily.len(), 1);
    }

    #[test]
    fn test_message_time_range_filters_resumed_sessions() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        let now_ms = chrono::Local::now().timestamp_millis();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated, tokens_input) VALUES ('resumed', '{\"id\":\"m1\"}', '/repo', ?1, 10)",
            rusqlite::params![now_ms],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('old-msg', 'resumed', ?1)",
            [assistant_message_at(
                "old-model",
                1000000000000,
                10,
                0,
                0,
                0,
                0,
                0.01,
            )],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('recent-msg', 'resumed', ?1)",
            [assistant_message_at(
                "recent-model",
                now_ms,
                20,
                0,
                0,
                0,
                0,
                0.02,
            )],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('old-part', 'old-msg', 'resumed', ?1)",
            [r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/old.py"},"output":"<content>\n1: old\n</content>"}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('recent-part', 'recent-msg', 'resumed', ?1)",
            [r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/recent.py"},"output":"<content>\n1: recent\n</content>"}}"#],
        )
        .unwrap();
        drop(conn);

        let usage_rows = read_opencode_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(usage_rows.len(), 1);
        assert!(
            usage_rows[0].1.records[0]
                .conversation_usage
                .contains_key("recent-model")
        );

        let analysis_rows =
            read_opencode_analysis(&db_path, TimeRange::Daily, ParseMode::UsageOnly).unwrap();
        assert_eq!(analysis_rows.len(), 1);
        let record = &analysis_rows[0].1.records[0];
        assert!(record.conversation_usage.contains_key("recent-model"));
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 1);
    }

    #[test]
    fn test_legacy_session_usage_filters_old_sessions() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE message", []).unwrap();
        conn.execute("DROP TABLE part", []).unwrap();

        let now_ms = chrono::Local::now().timestamp_millis();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated, cost, tokens_input) VALUES ('recent', '{\"id\":\"m1\"}', '/repo', ?1, 0.01, 10)",
            rusqlite::params![now_ms],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated, cost, tokens_input) VALUES ('old', '{\"id\":\"m2\"}', '/repo', 1000000000000, 0.02, 20)",
            [],
        )
        .unwrap();
        drop(conn);

        let daily = read_opencode_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(daily.len(), 1);
        assert!(daily[0].1.records[0].conversation_usage.contains_key("m1"));
    }

    #[test]
    fn test_messages_split_usage_and_analysis_by_model() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m2\"}', '/repo', 1780757088080)",
	            [],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("m1", 10, 2, 0, 3, 4, 0.01)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m2', 's1', ?1)",
            [assistant_message("m2", 20, 4, 1, 6, 8, 0.02)],
        )
        .unwrap();
        conn.execute(
	            "INSERT INTO part (id, message_id, session_id, data) VALUES ('p1', 'm1', 's1', ?1)",
	            [r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/a.py"},"output":"<content>\n1: one\n</content>"}}"#],
	        )
	        .unwrap();
        conn.execute(
	            "INSERT INTO part (id, message_id, session_id, data) VALUES ('p2', 'm2', 's1', ?1)",
	            [r#"{"type":"tool","tool":"edit","state":{"status":"completed","input":{"filePath":"/repo/b.py","oldString":"old","newString":"new\nline"}}}"#],
	        )
	        .unwrap();
        drop(conn);

        let usage_rows = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(usage_rows.len(), 2);
        let mut usage_by_model: HashMap<String, (i64, f64)> = HashMap::new();
        for (_date, analysis, cost) in usage_rows {
            let (model, usage) = analysis.records[0]
                .conversation_usage
                .iter()
                .next()
                .unwrap();
            usage_by_model.insert(
                model.clone(),
                (usage["input_tokens"].as_i64().unwrap(), cost),
            );
        }
        assert_eq!(usage_by_model["m1"], (10, 0.01));
        assert_eq!(usage_by_model["m2"], (20, 0.02));

        let analysis_rows =
            read_opencode_analysis(&db_path, TimeRange::All, ParseMode::UsageOnly).unwrap();
        assert_eq!(analysis_rows.len(), 2);
        let mut counts_by_model: HashMap<String, (usize, usize)> = HashMap::new();
        for (_date, analysis) in analysis_rows {
            let record = &analysis.records[0];
            let model = record.conversation_usage.keys().next().unwrap().clone();
            counts_by_model.insert(
                model,
                (record.tool_call_counts.read, record.tool_call_counts.edit),
            );
        }
        assert_eq!(counts_by_model["m1"], (1, 0));
        assert_eq!(counts_by_model["m2"], (0, 1));
    }

    #[test]
    fn test_read_analysis_counts_tools() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m1\"}', '/repo', 1780757088080)",
	            [],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("m1", 1, 1, 0, 0, 0, 0.01)],
        )
        .unwrap();
        let parts = [
            r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/a.py"},"output":"<path>/repo/a.py</path>\n<type>file</type>\n<content>\n1: one\n2: two\n</content>"}}"#,
            r#"{"type":"tool","tool":"edit","state":{"status":"completed","input":{"filePath":"/repo/a.py","oldString":"one","newString":"uno\ndos"}}}"#,
            r#"{"type":"tool","tool":"bash","state":{"status":"completed","input":{"command":"ls -la","description":"list"}}}"#,
            r#"{"type":"tool","tool":"todowrite","state":{"status":"completed","input":{"todos":[]}}}"#,
            r#"{"type":"tool","tool":"edit","state":{"status":"error","input":{"filePath":"/repo/a.py","oldString":"one","newString":"failed\nchange"}}}"#,
            r#"{"type":"tool","tool":"grep","state":{"status":"completed","input":{"pattern":"x"}}}"#,
            r#"{"type":"text","text":"ignored"}"#,
        ];
        for (i, p) in parts.iter().enumerate() {
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, data) VALUES (?1, 'm1', 's1', ?2)",
                rusqlite::params![format!("p{i}"), p],
            )
            .unwrap();
        }
        drop(conn);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::UsageOnly).unwrap();
        assert_eq!(rows.len(), 1);
        let record = &rows[0].1.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.todo_write, 1);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(record.total_edit_lines, 2);
        // grep / text parts are not tracked.
        assert_eq!(record.tool_call_counts.write, 0);
    }

    #[test]
    fn test_read_analysis_counts_apply_patch_tool() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m1\"}', '/repo', 1780757088080)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("m1", 1, 1, 0, 0, 0, 0.01)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('p1', 'm1', 's1', ?1)",
            [r#"{"type":"tool","tool":"apply_patch","state":{"status":"completed","input":{"patchText":"*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n+line\n*** Add File: src/new.rs\n+created\n*** End Patch"}}}"#],
        )
        .unwrap();
        drop(conn);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::UsageOnly).unwrap();
        assert_eq!(rows.len(), 1);
        let record = &rows[0].1.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_edit_lines, 2);
        assert_eq!(record.total_write_lines, 1);
    }

    #[test]
    fn test_parse_apply_patch_text_handles_move_marker() {
        let patches = parse_apply_patch_text(
            "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-a\n+b\n*** End Patch",
        );
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].action, "update");
        assert_eq!(patches[0].file_path, "src/new.rs");
        let (old_str, new_str) = extract_patch_strings(&patches[0].lines);
        assert_eq!(old_str, "a");
        assert_eq!(new_str, "b");
    }

    #[test]
    fn test_read_analysis_ignores_patch_snapshots() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m1\"}', '/repo', 1780757088080)",
	            [],
	        )
	        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("m1", 1, 1, 0, 0, 0, 0.01)],
        )
        .unwrap();
        conn.execute(
	            "INSERT INTO part (id, message_id, session_id, time_updated, data) VALUES ('p1', 'm1', 's1', 1780757089000, ?1)",
	            [r#"{"type":"patch","files":["/repo/a.py","/repo/b.py"]}"#],
	        )
        .unwrap();
        drop(conn);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::UsageOnly).unwrap();
        assert_eq!(rows.len(), 1);
        let record = &rows[0].1.records[0];
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.total_edit_lines, 0);
    }
}
