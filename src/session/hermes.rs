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
//! so a model billed through two providers merges into one model row. Each
//! session is also reconciled against the `sessions` aggregate so partial or
//! missing per-model rows are not under-counted (see [`collect_usage`]).

use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, ExtensionType};
use crate::session::diagnostics::{DatabaseUsageRead, UsageContribution, UsageTokenContribution};
use crate::session::sqlite::with_readonly_connection;
use crate::utils::{get_current_user, get_machine_id};
use anyhow::Result;
use rusqlite::Connection;
#[cfg(test)]
use serde_json::Value;
use std::path::Path;

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
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    Ok(read_hermes_usage_contributions(db_path, time_range)?
        .rows
        .into_iter()
        .map(|row| row.into_public_row(ExtensionType::Hermes, &user, &machine))
        .collect())
}

/// Reads compact Hermes usage rows for the summary aggregation path.
pub(crate) fn read_hermes_usage_contributions(
    db_path: &Path,
    time_range: TimeRange,
) -> Result<DatabaseUsageRead> {
    with_readonly_connection(db_path, "sessions", "vct-hermes-", "Hermes", |conn| {
        collect_usage(conn, time_range).map(DatabaseUsageRead::complete)
    })
}

/// Per-session raw column sums, used to reconcile against the `sessions`
/// aggregate. Kept in raw (reasoning-inclusive) terms so the residual math
/// matches Hermes, which subtracts raw columns.
#[derive(Default)]
struct RowSums {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    estimated: f64,
    actual: f64,
}

/// Collects the `usage` view from `session_model_usage`, then reconciles each
/// session against its `sessions` aggregate.
///
/// Hermes attributes tokens and cost per model from `session_model_usage`, but
/// a session can carry partial or missing per-model rows (legacy data,
/// interrupted migrations, gateway cumulative updates). Hermes's own insights
/// view (`agent/insights.py::_compute_model_breakdown`) covers that by
/// attributing the positive residual (`sessions.<col>` minus the sum of the
/// session's per-model rows) to the session's recorded model. We mirror that so
/// vct's totals agree with Hermes instead of under-reporting.
fn collect_usage(conn: &Connection, time_range: TimeRange) -> Result<Vec<UsageContribution>> {
    let cutoff = cutoff_string(time_range);

    let mut out = Vec::new();
    let mut summed: FastHashMap<String, RowSums> = FastHashMap::default();

    // Older Hermes releases predate `session_model_usage`; treat a missing table
    // as an empty per-model set and let the `sessions` reconciliation below
    // produce usage from the aggregate (matching Hermes's own fallback).
    if table_exists(conn, "session_model_usage")? {
        collect_per_model_rows(conn, &cutoff, &mut summed, &mut out)?;
    }

    reconcile_session_residuals(conn, &cutoff, &summed, &mut out)?;
    Ok(out)
}

/// Returns whether `table` exists in the database.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Emits one contribution per `session_model_usage` row and accumulates the raw
/// per-session sums used by the residual reconciliation.
fn collect_per_model_rows(
    conn: &Connection,
    cutoff: &Option<String>,
    summed: &mut FastHashMap<String, RowSums>,
    out: &mut Vec<UsageContribution>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, last_seen, first_seen \
         FROM session_model_usage",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let model: String = row.get(1)?;
        let input = row.get::<_, i64>(2)?;
        let raw_output = row.get::<_, i64>(3)?;
        let cache_read = row.get::<_, i64>(4)?;
        let cache_write = row.get::<_, i64>(5)?;
        let reasoning = row.get::<_, i64>(6)?;
        let estimated = row.get::<_, f64>(7)?;
        let actual = row.get::<_, f64>(8)?;
        // `last_seen` (last activity) drives the date, falling back to `first_seen`.
        let seconds = row
            .get::<_, Option<f64>>(9)?
            .or(row.get::<_, Option<f64>>(10)?);

        // Accumulate raw sums for the residual, even for rows outside the time
        // window — the residual is a session-level quantity.
        let acc = summed.entry(session_id).or_default();
        acc.input += input;
        acc.output += raw_output;
        acc.cache_read += cache_read;
        acc.cache_write += cache_write;
        acc.reasoning += reasoning;
        acc.estimated += estimated;
        acc.actual += actual;

        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        let Some(seconds) = seconds else { continue };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            continue;
        };
        if is_before_cutoff(&date, cutoff) {
            continue;
        }

        // Hermes bills through an OpenAI-compatible layer where `output_tokens`
        // already includes reasoning, so subtract it back out to keep each token
        // billed once (the flat shape treats the buckets as disjoint).
        let output = (raw_output - reasoning).max(0);
        // Prefer the real billed cost; fall back to Hermes's own estimate.
        let cost = if actual > 0.0 { actual } else { estimated };
        out.push(UsageContribution::single_model(
            date,
            (seconds * 1000.0) as i64,
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost,
        ));
    }
    Ok(())
}

/// Attributes each session's positive residual (aggregate minus the sum of its
/// per-model rows) to the session's recorded model, mirroring Hermes's insights
/// view so partial or missing per-model rows are not dropped.
fn reconcile_session_residuals(
    conn: &Connection,
    cutoff: &Option<String>,
    summed: &FastHashMap<String, RowSums>,
    out: &mut Vec<UsageContribution>,
) -> Result<()> {
    // The `sessions` table is core to Hermes, but stay defensive: if it is
    // somehow absent, the per-model rows already collected are still returned.
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, ended_at, started_at \
         FROM sessions",
    ) else {
        return Ok(());
    };
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let model: Option<String> = row.get(1)?;
        let acc = summed.get(&id);
        let input =
            (row.get::<_, Option<i64>>(2)?.unwrap_or(0) - acc.map_or(0, |a| a.input)).max(0);
        // `sessions.output_tokens` follows the OpenAI convention (includes
        // reasoning), so split the residual the same way the per-model rows are
        // split: reasoning goes to its own bucket and is subtracted from output,
        // keeping the breakdown right and reasoning-rate pricing correct.
        let raw_output =
            (row.get::<_, Option<i64>>(3)?.unwrap_or(0) - acc.map_or(0, |a| a.output)).max(0);
        let cache_read =
            (row.get::<_, Option<i64>>(4)?.unwrap_or(0) - acc.map_or(0, |a| a.cache_read)).max(0);
        let cache_write =
            (row.get::<_, Option<i64>>(5)?.unwrap_or(0) - acc.map_or(0, |a| a.cache_write)).max(0);
        // Reasoning is a subset of output, so cap its residual at the output
        // residual. An inconsistent aggregate could otherwise leave reasoning >
        // output, billing reasoning tokens against zero output.
        let reasoning = (row.get::<_, Option<i64>>(6)?.unwrap_or(0)
            - acc.map_or(0, |a| a.reasoning))
        .max(0)
        .min(raw_output);
        let output = raw_output - reasoning;
        let estimated = (row.get::<_, Option<f64>>(7)?.unwrap_or(0.0)
            - acc.map_or(0.0, |a| a.estimated))
        .max(0.0);
        let actual =
            (row.get::<_, Option<f64>>(8)?.unwrap_or(0.0) - acc.map_or(0.0, |a| a.actual)).max(0.0);
        if input == 0
            && raw_output == 0
            && cache_read == 0
            && cache_write == 0
            && estimated <= 0.0
            && actual <= 0.0
        {
            continue;
        }

        let model = model.as_deref().unwrap_or("").trim();
        if model.is_empty() {
            continue;
        }
        let Some(seconds) = row
            .get::<_, Option<f64>>(9)?
            .or(row.get::<_, Option<f64>>(10)?)
        else {
            continue;
        };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            continue;
        };
        if is_before_cutoff(&date, cutoff) {
            continue;
        }

        let cost = if actual > 0.0 { actual } else { estimated };
        out.push(UsageContribution::single_model(
            date,
            (seconds * 1000.0) as i64,
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost,
        ));
    }
    Ok(())
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
) -> UsageTokenContribution {
    UsageTokenContribution {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Builds a temp Hermes database with the `session_model_usage` and
    /// `sessions` tables.
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
             );
             CREATE TABLE sessions (
                 id TEXT PRIMARY KEY,
                 model TEXT,
                 billing_provider TEXT,
                 input_tokens INTEGER DEFAULT 0,
                 output_tokens INTEGER DEFAULT 0,
                 cache_read_tokens INTEGER DEFAULT 0,
                 cache_write_tokens INTEGER DEFAULT 0,
                 reasoning_tokens INTEGER DEFAULT 0,
                 estimated_cost_usd REAL DEFAULT 0,
                 actual_cost_usd REAL DEFAULT 0,
                 started_at REAL NOT NULL,
                 ended_at REAL
             );",
        )
        .unwrap();
        (dir, db_path)
    }

    /// Inserts a `sessions` aggregate row (the per-session token/cost totals
    /// Hermes reconciles per-model rows against).
    #[allow(clippy::too_many_arguments)]
    fn insert_session(
        conn: &Connection,
        id: &str,
        model: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        estimated: f64,
        actual: f64,
        last_seen: f64,
    ) {
        // `reasoning_tokens` is left to its column default (0); tests that need a
        // non-zero reasoning residual insert the row directly.
        conn.execute(
            "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens, estimated_cost_usd, \
                 actual_cost_usd, started_at, ended_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            rusqlite::params![
                id,
                model,
                input,
                output,
                cache_read,
                cache_write,
                estimated,
                actual,
                last_seen
            ],
        )
        .unwrap();
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

    /// Sums one token bucket across every contribution keyed by `model`.
    fn total_bucket(sessions: &[(String, CodeAnalysis, f64)], model: &str, key: &str) -> i64 {
        sessions
            .iter()
            .filter_map(|(_, a, _)| a.records[0].conversation_usage.get(model))
            .filter_map(|u| u[key].as_i64())
            .sum()
    }

    fn total_cost(sessions: &[(String, CodeAnalysis, f64)], model: &str) -> f64 {
        sessions
            .iter()
            .filter(|(_, a, _)| a.records[0].conversation_usage.contains_key(model))
            .map(|(_, _, c)| *c)
            .sum()
    }

    #[test]
    fn residual_covers_session_with_no_per_model_rows() {
        // A session whose per-model rows were never written (legacy / interrupted
        // migration) is still counted from its aggregate.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(
                &conn,
                "s1",
                "gpt-x",
                100,
                10,
                5,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        let usage = usage_of(&sessions[0].1, "gpt-x");
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 10);
        assert_eq!(usage["cache_read_input_tokens"], 5);
        assert!((sessions[0].2 - 0.5).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn residual_covers_partial_per_model_rows_without_double_counting() {
        // Per-model rows account for part of the session; the remainder is
        // attributed to the session's model. The two together equal the
        // aggregate exactly (no double counting).
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "gpt-x",
                "openai-api",
                60,
                6,
                0,
                0,
                0,
                0.3,
                0.0,
                recent_epoch_secs(),
            );
            insert_session(
                &conn,
                "s1",
                "gpt-x",
                100,
                10,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        assert_eq!(total_bucket(&sessions, "gpt-x", "output_tokens"), 10);
        assert!((total_cost(&sessions, "gpt-x") - 0.5).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn no_residual_when_per_model_rows_cover_session() {
        // Aggregate equals the per-model sum, so no residual contribution.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "gpt-x",
                "openai-api",
                100,
                10,
                0,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
            insert_session(
                &conn,
                "s1",
                "gpt-x",
                100,
                10,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        drop(dir);
    }

    #[test]
    fn reads_sessions_when_per_model_table_is_missing() {
        // A pre-migration Hermes DB has `sessions` but no `session_model_usage`;
        // usage must still come from the aggregate rather than erroring out.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY,
                     model TEXT,
                     billing_provider TEXT,
                     input_tokens INTEGER DEFAULT 0,
                     output_tokens INTEGER DEFAULT 0,
                     cache_read_tokens INTEGER DEFAULT 0,
                     cache_write_tokens INTEGER DEFAULT 0,
                     reasoning_tokens INTEGER DEFAULT 0,
                     estimated_cost_usd REAL DEFAULT 0,
                     actual_cost_usd REAL DEFAULT 0,
                     started_at REAL NOT NULL,
                     ended_at REAL
                 );",
            )
            .unwrap();
            insert_session(
                &conn,
                "s1",
                "gpt-x",
                100,
                10,
                5,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        let usage = usage_of(&sessions[0].1, "gpt-x");
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 10);
        assert!((sessions[0].2 - 0.5).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn residual_caps_reasoning_at_output() {
        // An inconsistent aggregate where reasoning (10) exceeds output (5) must
        // not emit reasoning tokens against zero output; reasoning is capped at
        // the output residual (5) so each token is still billed once.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                     reasoning_tokens, estimated_cost_usd, started_at, ended_at) \
                 VALUES ('s1', 'gpt-x', 100, 5, 10, 0.5, ?1, ?1)",
                [recent_epoch_secs()],
            )
            .unwrap();
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        let usage = usage_of(&sessions[0].1, "gpt-x");
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["reasoning_output_tokens"], 5);
        drop(dir);
    }

    #[test]
    fn residual_splits_reasoning_out_of_output() {
        // A residual-only session whose output_tokens (10) includes reasoning (4)
        // must report output 6 and reasoning 4, not output 10 / reasoning 0.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                     reasoning_tokens, estimated_cost_usd, started_at, ended_at) \
                 VALUES ('s1', 'gpt-x', 100, 10, 4, 0.5, ?1, ?1)",
                [recent_epoch_secs()],
            )
            .unwrap();
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        let usage = usage_of(&sessions[0].1, "gpt-x");
        assert_eq!(usage["output_tokens"], 6);
        assert_eq!(usage["reasoning_output_tokens"], 4);
        drop(dir);
    }
}
