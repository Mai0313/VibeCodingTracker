//! Hermes session reader (SQLite, not JSONL).
//!
//! Like OpenCode, Hermes stores usage in a single SQLite database at
//! `~/.hermes/state.db` rather than per-session JSONL files. The
//! `session_model_usage` table already holds one pre-aggregated row per
//! `(session, model, billing_provider, ...)`, so this module reads those rows
//! directly and maps them onto the same flat [`CodeAnalysis`] shape the
//! file-based providers produce, letting the `usage` aggregator fold Hermes in
//! alongside everyone else.
//!
//! Only the `usage` view is supported: the table has no file-operation detail,
//! so there is no `analysis` reader. Rows are keyed by the bare `model` column,
//! so a model billed through two providers merges into one model row.

use crate::VERSION;
use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, ExtensionType};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Reads per-model token usage from the Hermes database.
///
/// Each returned tuple is `(local YYYY-MM-DD date, CodeAnalysis, stored_cost)`,
/// where the `CodeAnalysis` holds one `session_model_usage` row's
/// `conversation_usage` keyed by that row's `model`, and `stored_cost` is
/// Hermes's own cost for the row (the actual billed cost when known, otherwise
/// its estimate). The date comes from the row's `last_seen` timestamp and is
/// filtered by `time_range`, matching the file-walker semantics.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
pub fn read_hermes_usage(
    db_path: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    open_readonly(db_path, |conn| collect_usage(conn, time_range))
}

/// Collects the `usage` view from `session_model_usage` rows.
fn collect_usage(
    conn: &Connection,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cutoff = cutoff_string(time_range);

    let mut stmt = conn.prepare(
        "SELECT model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, last_seen, first_seen \
         FROM session_model_usage",
    )?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let model: String = row.get(0)?;
        let model = model.trim();
        if model.is_empty() {
            continue;
        }

        // `last_seen` (last activity) drives the date, falling back to
        // `first_seen`; a row with neither has no place on the calendar.
        let Some(seconds) = row
            .get::<_, Option<f64>>(8)?
            .or(row.get::<_, Option<f64>>(9)?)
        else {
            continue;
        };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            continue;
        };
        if is_before_cutoff(&date, &cutoff) {
            continue;
        }

        let input = row.get::<_, i64>(1)?;
        let output = row.get::<_, i64>(2)?;
        let cache_read = row.get::<_, i64>(3)?;
        let cache_write = row.get::<_, i64>(4)?;
        let reasoning = row.get::<_, i64>(5)?;
        let estimated_cost = row.get::<_, f64>(6)?;
        let actual_cost = row.get::<_, f64>(7)?;

        // Hermes bills through an OpenAI-compatible layer where `output_tokens`
        // already includes reasoning, so subtract it back out to keep each token
        // billed once (the flat shape below treats the buckets as disjoint).
        let output = (output - reasoning).max(0);
        // Prefer the real billed cost; fall back to Hermes's own estimate.
        let cost = if actual_cost > 0.0 {
            actual_cost
        } else {
            estimated_cost
        };

        let mut map = FastHashMap::default();
        map.insert(
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
        );

        let mut state = SessionParseState::with_mode(ParseMode::UsageOnly);
        state.last_ts = (seconds * 1000.0) as i64;
        out.push((
            date,
            wrap_record(state.into_record(map), &user, &machine),
            cost,
        ));
    }

    Ok(out)
}

/// Builds the Claude-style flat usage value from a row's token columns.
///
/// Hermes records non-cached input separately from cache reads (matching the
/// Claude convention), so the columns map straight onto the field names
/// `extract_token_counts` understands.
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

/// Wraps a single record into a fully-populated [`CodeAnalysis`].
fn wrap_record(record: CodeAnalysisRecord, user: &str, machine: &str) -> CodeAnalysis {
    CodeAnalysis {
        user: user.to_string(),
        extension_name: ExtensionType::Hermes.to_string(),
        insights_version: VERSION.to_string(),
        machine_id: machine.to_string(),
        records: vec![record],
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

/// Returns `true` when `date` is strictly before the cutoff (should be skipped).
fn is_before_cutoff(date: &str, cutoff: &Option<String>) -> bool {
    matches!(cutoff, Some(c) if date < c.as_str())
}

/// Opens the Hermes DB read-only and runs `f` against it.
///
/// The primary path opens the database read-only so the user's file is never
/// mutated. If that fails (e.g. a WAL needing recovery that cannot be locked
/// read-only) we fall back to copying the database plus its `-wal`/`-shm`
/// sidecars into a private temp directory and reading the copy. The temp copy
/// is removed when `f` returns.
fn open_readonly<T>(db_path: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    if let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        && conn
            .query_row("SELECT count(*) FROM session_model_usage", [], |_| Ok(()))
            .is_ok()
    {
        return f(&conn);
    }

    let copy = TempDbCopy::new(db_path)?;
    let conn = Connection::open_with_flags(&copy.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
            format!(
                "Failed to open Hermes DB copy at {}",
                copy.db_path.display()
            )
        })?;
    f(&conn)
}

/// A private temp-directory copy of the Hermes database (plus WAL sidecars),
/// removed on drop. The temp dir has owner-only permissions so the data is
/// never exposed to other local users.
struct TempDbCopy {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
}

impl TempDbCopy {
    fn new(src: &Path) -> Result<Self> {
        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid Hermes DB path: {}", src.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("vct-hermes-")
            .tempdir()
            .context("Failed to create temp dir for Hermes DB copy")?;
        let db_path = dir.path().join(file_name);
        std::fs::copy(src, &db_path)
            .with_context(|| format!("Failed to copy Hermes DB from {}", src.display()))?;
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

    /// Builds a temp Hermes database with the `session_model_usage` table.
    fn make_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_model_usage (
                 session_id TEXT NOT NULL,
                 model TEXT NOT NULL,
                 billing_provider TEXT NOT NULL DEFAULT '',
                 input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                 reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                 estimated_cost_usd REAL NOT NULL DEFAULT 0,
                 actual_cost_usd REAL NOT NULL DEFAULT 0,
                 first_seen REAL,
                 last_seen REAL
             );",
        )
        .unwrap();
        (dir, db_path)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_row(
        conn: &Connection,
        session_id: &str,
        model: &str,
        billing_provider: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
        estimated: f64,
        actual: f64,
        last_seen: f64,
    ) {
        conn.execute(
            "INSERT INTO session_model_usage (session_id, model, billing_provider, \
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
                 reasoning_tokens, estimated_cost_usd, actual_cost_usd, first_seen, last_seen) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            rusqlite::params![
                session_id,
                model,
                billing_provider,
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
                estimated,
                actual,
                last_seen
            ],
        )
        .unwrap();
    }

    /// A recent `last_seen` so the row survives every time-range filter.
    fn recent_epoch_secs() -> f64 {
        chrono::Local::now().timestamp() as f64 - 60.0
    }

    fn usage_of<'a>(analysis: &'a CodeAnalysis, model: &str) -> &'a Value {
        &analysis.records[0].conversation_usage[model]
    }

    #[test]
    fn maps_tokens_and_subtracts_reasoning_from_output() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            // output=1745 includes reasoning=1399 → billed output should be 346.
            insert_row(
                &conn,
                "s1",
                "gemini-pro-latest",
                "openai-api",
                49246,
                1745,
                40465,
                0,
                1399,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        let (_date, analysis, _cost) = &sessions[0];
        assert_eq!(analysis.extension_name, "Hermes");
        let usage = usage_of(analysis, "gemini-pro-latest");
        assert_eq!(usage["input_tokens"], 49246);
        assert_eq!(usage["output_tokens"], 346);
        assert_eq!(usage["reasoning_output_tokens"], 1399);
        assert_eq!(usage["cache_read_input_tokens"], 40465);
        assert_eq!(usage["cache_creation_input_tokens"], 0);
        drop(dir);
    }

    #[test]
    fn cost_prefers_actual_then_estimated() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            // estimated set, actual 0 → use estimated.
            insert_row(
                &conn,
                "s1",
                "gpt-5.6-sol",
                "openai-api",
                18993,
                32,
                0,
                0,
                0,
                0.095925,
                0.0,
                recent_epoch_secs(),
            );
            // actual set → use actual over estimated.
            insert_row(
                &conn,
                "s2",
                "claude-x",
                "anthropic",
                100,
                10,
                0,
                0,
                0,
                0.5,
                0.25,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        let cost_of = |model: &str| {
            sessions
                .iter()
                .find(|(_, a, _)| a.records[0].conversation_usage.contains_key(model))
                .map(|(_, _, c)| *c)
                .unwrap()
        };
        assert!((cost_of("gpt-5.6-sol") - 0.095925).abs() < 1e-9);
        assert!((cost_of("claude-x") - 0.25).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn same_model_across_billing_providers_keeps_one_key_per_row() {
        // Two rows share a model but differ in billing_provider; the reader keys
        // both by the bare model, so the aggregator merges them into one row.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "gpt-5.6-sol",
                "custom",
                17487,
                103,
                0,
                0,
                25,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            insert_row(
                &conn,
                "s1",
                "gpt-5.6-sol",
                "openai-api",
                18993,
                32,
                0,
                0,
                0,
                0.095925,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 2);
        for (_, analysis, _) in &sessions {
            assert!(
                analysis.records[0]
                    .conversation_usage
                    .contains_key("gpt-5.6-sol")
            );
        }
        drop(dir);
    }

    #[test]
    fn time_range_filters_old_rows() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            // ~10 days ago, dropped by the daily filter but kept by All.
            let old = chrono::Local::now().timestamp() as f64 - 10.0 * 86_400.0;
            insert_row(&conn, "old", "m", "p", 1, 1, 0, 0, 0, 0.0, 0.0, old);
            insert_row(
                &conn,
                "new",
                "m",
                "p",
                2,
                2,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
        }
        let all = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(all.len(), 2);
        let daily = read_hermes_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(daily.len(), 1);
        drop(dir);
    }
}
