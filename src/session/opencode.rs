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
//! - [`read_opencode_usage`] reads only the `session` table (model, tokens,
//!   timestamps). It is enough for the per-model token / cost view.
//! - [`read_opencode_analysis`] additionally folds the `part` table's tool
//!   calls (`read`, `edit`, `write`, `bash`, `todowrite`) into per-session
//!   file-operation metrics.
//!
//! Token columns map onto the Claude-style flat usage shape so the existing
//! `merge_usage_values` / `extract_token_counts` / LiteLLM cost path works
//! unchanged. Sessions are single-model in practice, so each session's totals
//! are attributed to `session.model.id`.

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
/// where the `CodeAnalysis` holds exactly one record whose `conversation_usage`
/// is keyed by the session's model id, and `stored_cost` is OpenCode's own
/// `session.cost` (USD) for that session. The date comes from
/// `session.time_updated` and is filtered by `time_range`, matching the
/// file-walker semantics.
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

/// Per-session accumulator used while folding tool parts.
struct SessionAccum {
    model_id: String,
    date: String,
    usage: Value,
    state: SessionParseState,
}

/// Collects the `usage` view from the `session` table only.
fn collect_usage(
    conn: &Connection,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);

    let mut stmt = conn.prepare(
        "SELECT model, tokens_input, tokens_output, tokens_reasoning, \
                tokens_cache_read, tokens_cache_write, time_updated, cost \
         FROM session WHERE model IS NOT NULL AND model != ''",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, f64>(7)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (model, input, output, reasoning, cache_read, cache_write, time_updated, cost) = row?;
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

/// Collects the `analysis` view from `session` + `part`.
fn collect_analysis(
    conn: &Connection,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);
    let cutoff_ms = cutoff_millis(time_range);

    // 1. Load session metadata and seed one parse state per session.
    let mut sessions: HashMap<String, SessionAccum> = HashMap::new();
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
                SessionAccum {
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
            apply_tool_part(&mut accum.state, &data);
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

/// Dispatches a single `part` (type `tool`) onto the session parse state.
///
/// Only the tools the analyzer tracks across providers are folded in
/// (`read`, `edit`, `write`, `bash`, `todowrite`); auxiliary tools such as
/// `task`, `grep`, `glob`, `webfetch`, and `question` are ignored to stay
/// consistent with the other providers' tool-count semantics.
fn apply_tool_part(state: &mut SessionParseState, data: &Value) {
    let tool = data.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let st = data.get("state");
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
        _ => {}
    }
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
/// Modern OpenCode stores it as a JSON object `{"id", "providerID", ...}`; we
/// key everything off `id`. Older builds may store a bare model string, which
/// is used verbatim. Returns `None` when no usable model name is present.
fn parse_model_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => map
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Ok(Value::String(s)) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        // Not valid JSON: treat the column as a plain model name.
        _ => Some(raw.to_string()),
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

/// Extracts the file body from an OpenCode `read` tool output.
///
/// The output wraps file contents as `<content>\n1: line\n2: line\n</content>`,
/// each line prefixed with `N: `. This returns the joined lines with the
/// line-number prefixes stripped. Directory listings (no `<content>` block)
/// yield an empty string.
fn extract_read_content(output: &str) -> String {
    let Some(start) = output.find("<content>") else {
        return String::new();
    };
    let after = &output[start + "<content>".len()..];
    let inner = match after.find("</content>") {
        Some(end) => &after[..end],
        None => after,
    };
    let inner = inner.strip_prefix('\n').unwrap_or(inner);

    let mut lines: Vec<&str> = inner.split('\n').map(strip_line_number_prefix).collect();
    // Drop the trailing empty element produced by the newline before </content>.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Strips a leading `"<digits>: "` line-number prefix, if present.
fn strip_line_number_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b':' && bytes[i + 1] == b' ' {
        &line[i + 2..]
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
struct TempDbCopy {
    dir: PathBuf,
    db_path: PathBuf,
}

impl TempDbCopy {
    fn new(src: &Path) -> Result<Self> {
        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid OpenCode DB path: {}", src.display()))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("vct-opencode-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create temp dir {}", dir.display()))?;

        let db_path = dir.join(file_name);
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

        Ok(Self { dir, db_path })
    }
}

impl Drop for TempDbCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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
             CREATE TABLE part (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        (dir, db_path)
    }

    #[test]
    fn test_parse_model_id() {
        assert_eq!(
            parse_model_id(r#"{"id":"deepseek-v4-pro","providerID":"deepseek"}"#).as_deref(),
            Some("deepseek-v4-pro")
        );
        assert_eq!(
            parse_model_id("gemini-3.5-flash").as_deref(),
            Some("gemini-3.5-flash")
        );
        assert_eq!(parse_model_id(r#"{"providerID":"x"}"#), None);
        assert_eq!(parse_model_id("   "), None);
    }

    #[test]
    fn test_extract_read_content() {
        let output = "<path>/a/b.py</path>\n<type>file</type>\n<content>\n1: import os\n2: \n3: print(1)\n</content>";
        assert_eq!(extract_read_content(output), "import os\n\nprint(1)");

        // Directory listing has no <content> block.
        let dir_output = "<path>/a</path>\n<type>directory</type>\n<entries>\nx.py\n</entries>";
        assert_eq!(extract_read_content(dir_output), "");
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
        drop(conn);

        let rows = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(rows.len(), 1);
        let (_date, analysis, cost) = &rows[0];
        assert_eq!(analysis.extension_name, "OpenCode");
        assert!((cost - 0.0375).abs() < 1e-9);
        let usage = &analysis.records[0].conversation_usage["deepseek-v4-pro"];
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
            "INSERT INTO session (id, model, directory, time_updated, tokens_input) VALUES ('old', '{\"id\":\"m1\"}', '/repo', 1000000000000, 10)",
            [],
        )
        .unwrap();
        drop(conn);

        let all = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(all.len(), 2);

        let daily = read_opencode_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(daily.len(), 1);
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
        let parts = [
            r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/a.py"},"output":"<path>/repo/a.py</path>\n<type>file</type>\n<content>\n1: one\n2: two\n</content>"}}"#,
            r#"{"type":"tool","tool":"edit","state":{"status":"completed","input":{"filePath":"/repo/a.py","oldString":"one","newString":"uno\ndos"}}}"#,
            r#"{"type":"tool","tool":"bash","state":{"status":"completed","input":{"command":"ls -la","description":"list"}}}"#,
            r#"{"type":"tool","tool":"todowrite","state":{"status":"completed","input":{"todos":[]}}}"#,
            r#"{"type":"tool","tool":"grep","state":{"status":"completed","input":{"pattern":"x"}}}"#,
            r#"{"type":"text","text":"ignored"}"#,
        ];
        for (i, p) in parts.iter().enumerate() {
            conn.execute(
                "INSERT INTO part (id, session_id, data) VALUES (?1, 's1', ?2)",
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
}
