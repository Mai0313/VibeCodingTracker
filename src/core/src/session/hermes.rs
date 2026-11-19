//! Hermes session reader for the local SQLite database (`~/.hermes/state.db`).
//!
//! Only the `usage` view is supported; the database stores no file-operation detail and has no `analysis` reader.
//!
//! Per-model rows come from `session_model_usage` keyed by the bare `model` column (merging billing providers for the same model), and each session is reconciled against its `sessions` aggregate to cover partial or missing per-model rows. Output uses the shared [`CodeAnalysis`] shape.

use crate::constants::FastHashMap;
use crate::models::TimeRange;
use crate::models::{CodeAnalysis, ExtensionType};
use crate::session::diagnostics::{DatabaseUsageRead, UsageContribution, UsageTokenContribution};
use crate::session::sqlite::with_readonly_connection;
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Result, anyhow};
use rusqlite::Connection;
#[cfg(test)]
use serde_json::Value;
use std::path::Path;

/// Returns per-model token usage from the Hermes database at `db_path` within `time_range`.
///
/// Each tuple is `(local YYYY-MM-DD, CodeAnalysis, stored_cost)`, where the [`CodeAnalysis`] holds one model's `conversation_usage` and `stored_cost` is the positive `actual_cost_usd` or else `estimated_cost_usd`. Tuples come from `session_model_usage` rows and positive per-session residuals against `sessions`, dated by `last_seen` / `ended_at` (falling back to `first_seen` / `started_at`).
///
/// # Errors
///
/// Returns an error if `db_path` cannot be opened or queried, or if expected usage records exist and none used a recognized schema.
pub fn read_hermes_usage(
    db_path: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let read = read_hermes_usage_contributions(db_path, time_range)?;
    if read.expected_records > 0 && read.parsed_records == 0 {
        return Err(anyhow!(
            "none of {} Hermes usage records used a recognized schema",
            read.expected_records
        ));
    }
    if read.failed_records() > 0 {
        log::warn!(
            "{} Hermes usage records used an unsupported schema",
            read.failed_records()
        );
    }
    Ok(read
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
        collect_usage(conn, time_range)
    })
}

/// Per-session raw (reasoning-inclusive) column sums used to compute `sessions` residuals.
///
/// `unattributed` counts non-empty per-model rows omitted from the sums to fall through to the session residual; any remainder after reconciliation is reported as dropped.
#[derive(Default)]
struct RowSums {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    estimated: f64,
    actual: f64,
    unattributed: usize,
}

/// Counts of expected usage records and records whose tokens could not be attributed.
///
/// Out-of-window rows and per-model rows rescued by a session residual count as attributed; `unusable` tracks only lost tokens and failed queries.
#[derive(Default)]
struct RowTally {
    expected: usize,
    unusable: usize,
}

impl RowTally {
    /// Records one row this reader could place.
    fn attributed(&mut self) {
        self.expected += 1;
    }

    /// Records `count` rows whose tokens this reader could not place.
    fn dropped(&mut self, count: usize) {
        self.expected += count;
        self.unusable += count;
    }
}

/// Collects `session_model_usage` rows and reconciles each session's positive residual against `sessions`.
///
/// Unplaceable per-model rows are omitted from the per-session sums; their tokens fall through to the session residual and are re-attributed to the session's model.
fn collect_usage(conn: &Connection, time_range: TimeRange) -> Result<DatabaseUsageRead> {
    let cutoff = cutoff_string(time_range);

    let mut out = Vec::new();
    let mut summed: FastHashMap<String, RowSums> = FastHashMap::default();
    let mut tally = RowTally::default();

    // Older Hermes databases predate `session_model_usage`; check table existence so a missing table falls back to `sessions` without reporting schema drift.
    if table_exists(conn, "session_model_usage")?
        && let Err(error) = collect_per_model_rows(conn, &cutoff, &mut summed, &mut out, &mut tally)
    {
        report_unreadable("per-model usage rows", &error, &mut tally);
    }

    if let Err(error) =
        reconcile_session_residuals(conn, &cutoff, &mut summed, &mut out, &mut tally)
    {
        report_unreadable("session residuals", &error, &mut tally);
    }
    // Report only unattributed per-model rows that no session residual picked up.
    tally.dropped(summed.values().map(|sums| sums.unattributed).sum());
    Ok(DatabaseUsageRead {
        rows: out,
        expected_records: tally.expected,
        parsed_records: tally.expected - tally.unusable,
    })
}

/// Logs a failed pass query and records one dropped entry so the other pass can still contribute.
fn report_unreadable(what: &str, error: &rusqlite::Error, tally: &mut RowTally) {
    // Exact lost row count is unknowable after a query failure; record a floor of 1.
    tally.dropped(1);
    log::warn!("Hermes {what} unavailable: {error}");
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

/// Emits in-range `session_model_usage` contributions and accumulates raw per-session sums for residual reconciliation.
///
/// Unplaceable rows are skipped from `summed` and marked `unattributed` on their session.
fn collect_per_model_rows(
    conn: &Connection,
    cutoff: &Option<String>,
    summed: &mut FastHashMap<String, RowSums>,
    out: &mut Vec<UsageContribution>,
    tally: &mut RowTally,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, last_seen, first_seen \
         FROM session_model_usage",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        // `session_id` has no fallback; propagate read errors to stop this pass rather than silently dropping the row.
        let session_id: String = row.get(0)?;
        let model: Option<String> = row.get(1)?;
        // Treat any NULL numeric column as unplaceable so the session residual carries the row; defaulting a NULL `reasoning_tokens` (e.g. pre-migration) to 0 would mis-bill all output tokens and lose reasoning.
        let (
            Some(input),
            Some(raw_output),
            Some(cache_read),
            Some(cache_write),
            Some(reasoning),
            Some(estimated),
            Some(actual),
        ) = (
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<f64>>(7)?,
            row.get::<_, Option<f64>>(8)?,
        )
        else {
            // Token columns were incomplete, so assume the row carried usage rather than dropping it silently.
            mark_unattributed(summed, session_id, true);
            continue;
        };
        // Zero-usage rows have nothing to carry or lose, so skipping them is not reported.
        let carries_usage = input != 0
            || raw_output != 0
            || reasoning != 0
            || cache_read != 0
            || cache_write != 0
            || estimated != 0.0
            || actual != 0.0;
        // Prefer `last_seen` and only read `first_seen` on NULL so an unreadable `first_seen` does not discard a valid `last_seen`.
        let seconds = match row.get::<_, Option<f64>>(9)? {
            Some(seconds) => Some(seconds),
            None => row.get::<_, Option<f64>>(10)?,
        };

        let Some(seconds) = seconds else {
            mark_unattributed(summed, session_id, carries_usage);
            continue;
        };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            mark_unattributed(summed, session_id, carries_usage);
            continue;
        };
        let model = model.as_deref().unwrap_or("").trim();
        if model.is_empty() {
            // Only mark missing-model rows that fall inside the active window; dateless rows above are unplaceable in every window.
            if !is_before_cutoff(&date, cutoff) {
                mark_unattributed(summed, session_id, carries_usage);
            }
            continue;
        }
        tally.attributed();

        // Accumulate raw sums even for pre-cutoff rows; residuals are session-level and cutoff-filtered separately, so omitting old rows would double-count them.
        let acc = summed.entry(session_id.clone()).or_default();
        acc.input += input;
        acc.output += raw_output;
        acc.cache_read += cache_read;
        acc.cache_write += cache_write;
        acc.reasoning += reasoning;
        acc.estimated += estimated;
        acc.actual += actual;

        if is_before_cutoff(&date, cutoff) {
            continue;
        }

        // Hermes's `output_tokens` includes reasoning; subtract it so the disjoint buckets bill each token once.
        let output = (raw_output - reasoning).max(0);
        // Prefer the real billed cost; fall back to Hermes's own estimate.
        let cost = if actual > 0.0 { actual } else { estimated };
        // Per-model rows are session-aggregated with no per-request context, so tier level is always 0.
        out.push(UsageContribution::single_model(
            session_id,
            date,
            (seconds * 1000.0) as i64,
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost,
            0,
        ));
    }
    Ok(())
}

/// Attributes each session's positive residual (`sessions` aggregate minus `summed`) to the session's model.
///
/// Placing a residual clears the session's `unattributed` marks; unplaceable in-window residuals are counted as dropped in `tally`.
fn reconcile_session_residuals(
    conn: &Connection,
    cutoff: &Option<String>,
    summed: &mut FastHashMap<String, RowSums>,
    out: &mut Vec<UsageContribution>,
    tally: &mut RowTally,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, ended_at, started_at \
         FROM sessions",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let model: Option<String> = row.get(1)?;
        let acc = summed.get(&id);
        let input =
            (row.get::<_, Option<i64>>(2)?.unwrap_or(0) - acc.map_or(0, |a| a.input)).max(0);
        // `sessions.output_tokens` includes reasoning; subtract the reasoning residual from output just like per-model rows.
        let raw_output =
            (row.get::<_, Option<i64>>(3)?.unwrap_or(0) - acc.map_or(0, |a| a.output)).max(0);
        let cache_read =
            (row.get::<_, Option<i64>>(4)?.unwrap_or(0) - acc.map_or(0, |a| a.cache_read)).max(0);
        let cache_write =
            (row.get::<_, Option<i64>>(5)?.unwrap_or(0) - acc.map_or(0, |a| a.cache_write)).max(0);
        // Cap the reasoning residual at `raw_output` so an inconsistent aggregate never bills reasoning beyond output.
        let reasoning = (row.get::<_, Option<i64>>(6)?.unwrap_or(0)
            - acc.map_or(0, |a| a.reasoning))
        .max(0)
        .min(raw_output);
        let output = raw_output - reasoning;
        let estimated = residual_cost(
            row.get::<_, Option<f64>>(7)?.unwrap_or(0.0) - acc.map_or(0.0, |a| a.estimated),
        );
        let actual = residual_cost(
            row.get::<_, Option<f64>>(8)?.unwrap_or(0.0) - acc.map_or(0.0, |a| a.actual),
        );
        if input == 0
            && raw_output == 0
            && cache_read == 0
            && cache_write == 0
            && estimated <= 0.0
            && actual <= 0.0
        {
            // Keep existing `unattributed` marks: a zero residual alongside marked rows means the aggregate never counted those tokens either.
            continue;
        }

        let Some(seconds) = row
            .get::<_, Option<f64>>(9)?
            .or(row.get::<_, Option<f64>>(10)?)
        else {
            tally.dropped(1);
            continue;
        };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            tally.dropped(1);
            continue;
        };
        let model = model.as_deref().unwrap_or("").trim();
        let out_of_window = is_before_cutoff(&date, cutoff);
        if model.is_empty() && !out_of_window {
            tally.dropped(1);
            continue;
        }
        tally.attributed();
        // Clear `unattributed` marks: the residual carries any unplaced per-model tokens (or the whole session falls outside the active window).
        clear_unattributed(summed, &id);
        if out_of_window {
            continue;
        }

        let cost = if actual > 0.0 { actual } else { estimated };
        out.push(UsageContribution::single_model(
            id,
            date,
            (seconds * 1000.0) as i64,
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost,
            0,
        ));
    }
    Ok(())
}

/// Increments `session_id`'s `unattributed` count when `carries_usage` is true.
fn mark_unattributed(
    summed: &mut FastHashMap<String, RowSums>,
    session_id: String,
    carries_usage: bool,
) {
    if carries_usage {
        summed.entry(session_id).or_default().unattributed += 1;
    }
}

/// Drops the marks a session's unplaceable per-model rows left behind.
fn clear_unattributed(summed: &mut FastHashMap<String, RowSums>, id: &str) {
    if let Some(sums) = summed.get_mut(id) {
        sums.unattributed = 0;
    }
}

/// Minimum residual USD cost treated as real money rather than `f64` subtraction noise (below a single token at $0.05/M).
const COST_EPSILON: f64 = 1e-9;

/// Clamps residual costs below [`COST_EPSILON`] (including negatives) to `0.0`, preventing `f64` rounding noise from registering as nonzero activity.
fn residual_cost(value: f64) -> f64 {
    if value < COST_EPSILON { 0.0 } else { value }
}

/// Packs disjoint token counts into a [`UsageTokenContribution`]; `output` must already exclude `reasoning`.
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

    /// Builds a temp Hermes database with the `session_model_usage` and `sessions` tables.
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

    /// Inserts a `sessions` aggregate row.
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
        // `reasoning_tokens` defaults to 0; tests needing a non-zero reasoning residual insert directly.
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
        // Rows sharing a model across billing providers are keyed by the bare model so aggregation merges them.
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
        // A session with no per-model rows (legacy or interrupted migration) is counted from its aggregate.
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
        // Per-model rows plus the session residual equal the aggregate without double counting.
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
    fn per_model_row_with_no_model_leaves_its_tokens_to_the_residual() {
        // A blank-model row must not shrink the residual, or its tokens vanish from both sides.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "",
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
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        assert_eq!(total_bucket(&sessions, "gpt-x", "output_tokens"), 10);
        assert!((total_cost(&sessions, "gpt-x") - 0.5).abs() < 1e-9);
        // The residual carried every token, so there is nothing to report.
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.failed_records(), 0);
        drop(dir);
    }

    #[test]
    fn unattributable_row_with_no_session_row_is_reported() {
        // With no model and no session aggregate to rescue it, the row's tokens are lost.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "",
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
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.failed_records(), 1);
        // All expected records failed, so the public entry point errors instead of returning empty.
        assert!(read_hermes_usage(&db_path, TimeRange::All).is_err());
        drop(dir);
    }

    #[test]
    fn per_model_row_with_no_timestamp_leaves_its_tokens_to_the_residual() {
        // Null `last_seen` and `first_seen` make the per-model row undatable, leaving its tokens to the residual.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO session_model_usage (session_id, model, billing_provider, \
                     input_tokens, output_tokens, estimated_cost_usd) \
                 VALUES ('s1', 'gpt-x', 'openai-api', 60, 6, 0.3)",
                [],
            )
            .unwrap();
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
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        assert_eq!(total_bucket(&sessions, "gpt-x", "output_tokens"), 10);
        assert!((total_cost(&sessions, "gpt-x") - 0.5).abs() < 1e-9);
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.failed_records(), 0);
        drop(dir);
    }

    #[test]
    fn unpreparable_sessions_query_is_reported() {
        // A renamed `sessions` column loses all residuals while keeping per-model rows, reported as 1 unreadable aggregate pass.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "ALTER TABLE sessions RENAME COLUMN reasoning_tokens TO thinking_tokens;",
            )
            .unwrap();
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
            for id in ["s1", "s2"] {
                conn.execute(
                    "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                         estimated_cost_usd, started_at, ended_at) \
                     VALUES (?1, 'gpt-x', 100, 10, 0.5, ?2, ?2)",
                    rusqlite::params![id, recent_epoch_secs()],
                )
                .unwrap();
            }
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn session_residual_with_no_model_is_reported() {
        // An aggregate with unclaimed tokens and a blank model is reported as a lost record.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(
                &conn,
                "s1",
                "",
                100,
                10,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn session_residual_outside_the_time_range_is_not_reported_as_drift() {
        // A blank-model aggregate outside the active time window is not reported as a failure.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let old = chrono::Local::now().timestamp() as f64 - 10.0 * 86_400.0;
            insert_session(&conn, "s1", "", 100, 10, 0, 0, 0.5, 0.0, old);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::Daily).unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.failed_records(), 0);
        // The same database over the full range does have a loss to report.
        let all = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(all.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn session_residual_with_no_timestamp_is_reported() {
        // An aggregate with NULL timestamps cannot be dated in any range (uses a nullable `started_at` schema to reach this guard).
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY,
                     model TEXT,
                     input_tokens INTEGER DEFAULT 0,
                     output_tokens INTEGER DEFAULT 0,
                     cache_read_tokens INTEGER DEFAULT 0,
                     cache_write_tokens INTEGER DEFAULT 0,
                     reasoning_tokens INTEGER DEFAULT 0,
                     estimated_cost_usd REAL DEFAULT 0,
                     actual_cost_usd REAL DEFAULT 0,
                     started_at REAL,
                     ended_at REAL
                 );
                 INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                     estimated_cost_usd) \
                 VALUES ('s1', 'gpt-x', 100, 10, 0.5);",
            )
            .unwrap();
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn rows_outside_the_time_range_are_not_reported_as_drift() {
        // Rows filtered out by the time range still count as attributed so history never triggers drift warnings.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let old = chrono::Local::now().timestamp() as f64 - 10.0 * 86_400.0;
            insert_row(&conn, "old", "m", "p", 1, 1, 0, 0, 0, 0.0, 0.0, old);
            insert_session(&conn, "old", "m", 1, 1, 0, 0, 0.0, 0.0, old);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::Daily).unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.expected_records, 1);
        assert_eq!(read.failed_records(), 0);
        drop(dir);
    }

    #[test]
    fn reads_sessions_when_per_model_table_is_missing() {
        // A pre-migration Hermes DB with only `sessions` falls back to the session aggregate without erroring.
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
        // Reasoning (10) exceeding output (5) is capped at the output residual (5) so tokens are not over-billed.
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
    fn residual_of_float_noise_is_not_emitted() {
        // `0.8 - (0.7 + 0.1)` leaves `1.1e-16` in `f64`, which must snap to zero rather than emitting a zero-token residual.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                0.7,
                0.0,
                now,
            );
            insert_row(
                &conn,
                "s1",
                "gpt-y",
                "openai-api",
                40,
                4,
                0,
                0,
                0,
                0.1,
                0.0,
                now,
            );
            insert_session(&conn, "s1", "gpt-x", 100, 10, 0, 0, 0.8, 0.0, now);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 2);
        assert!(read.rows.iter().all(|row| row.tokens.has_activity()));
        drop(dir);
    }

    #[test]
    fn dust_in_the_actual_residual_no_longer_hides_a_real_estimated_one() {
        // An `actual` residual of float noise (`0.8 - (0.7 + 0.1)`) must snap to zero so the real `estimated` residual (0.3) is used.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                0.1,
                0.7,
                now,
            );
            insert_row(
                &conn,
                "s1",
                "gpt-y",
                "openai-api",
                30,
                3,
                0,
                0,
                0,
                0.1,
                0.1,
                now,
            );
            insert_session(&conn, "s1", "gpt-x", 100, 10, 0, 0, 0.5, 0.8, now);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 3);
        let residual = read.rows.last().unwrap();
        assert_eq!(residual.tokens.input_tokens, 10);
        assert!((residual.stored_cost - 0.3).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn covered_session_does_not_report_a_row_that_held_nothing() {
        // A zero-usage blank-model row is never marked unattributed, so a fully covered session with no residual reports no failure.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                now,
            );
            insert_row(&conn, "s1", "", "openai-api", 0, 0, 0, 0, 0, 0.0, 0.0, now);
            insert_session(&conn, "s1", "gpt-x", 100, 10, 0, 0, 0.5, 0.0, now);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.failed_records(), 0);
        drop(dir);
    }

    #[test]
    fn covered_session_still_reports_a_row_that_held_something() {
        // A fully covered session with no residual still reports an unplaceable row that carried nonzero tokens.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                now,
            );
            insert_row(&conn, "s1", "", "openai-api", 40, 4, 0, 0, 0, 0.0, 0.0, now);
            insert_session(&conn, "s1", "gpt-x", 100, 10, 0, 0, 0.5, 0.0, now);
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn unreadable_per_model_query_falls_back_to_the_session_aggregate() {
        // A failed `session_model_usage` query falls back to `sessions` totals instead of aborting the read.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                now,
            );
            conn.execute_batch(
                "ALTER TABLE session_model_usage \
                 RENAME COLUMN reasoning_tokens TO thinking_tokens;",
            )
            .unwrap();
            insert_session(&conn, "s1", "gpt-x", 100, 10, 5, 0, 0.5, 0.0, now);
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        assert_eq!(
            total_bucket(&sessions, "gpt-x", "cache_read_input_tokens"),
            5
        );
        assert!((total_cost(&sessions, "gpt-x") - 0.5).abs() < 1e-9);
        // The per-model breakdown was lost even though totals survived, so the scan reports one failure.
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn unreadable_session_row_keeps_the_per_model_rows() {
        // An unreadable `sessions` column loses only residuals and keeps already-collected per-model rows.
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let now = recent_epoch_secs();
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
                now,
            );
            // SQLite keeps text in an INTEGER-affinity column, triggering a read error on `input_tokens`.
            conn.execute(
                "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                     estimated_cost_usd, started_at, ended_at) \
                 VALUES ('s1', 'gpt-x', 'not-a-number', 10, 0.5, ?1, ?1)",
                [now],
            )
            .unwrap();
        }
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.failed_records(), 1);
        drop(dir);
    }

    #[test]
    fn per_model_row_with_a_null_column_leaves_its_tokens_to_the_residual() {
        // Pre-migration NULL numeric columns leave the row unplaced so the session aggregate carries the whole session with reasoning intact.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_model_usage (
                     session_id TEXT NOT NULL,
                     model TEXT NOT NULL,
                     input_tokens INTEGER NOT NULL DEFAULT 0,
                     output_tokens INTEGER NOT NULL DEFAULT 0,
                     cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                     cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                     reasoning_tokens INTEGER,
                     estimated_cost_usd REAL NOT NULL DEFAULT 0,
                     actual_cost_usd REAL NOT NULL DEFAULT 0,
                     first_seen REAL,
                     last_seen REAL
                 );
                 CREATE TABLE sessions (
                     id TEXT PRIMARY KEY,
                     model TEXT,
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
            let now = recent_epoch_secs();
            // `reasoning_tokens` is NULL while `output_tokens` still includes those 4 tokens.
            conn.execute(
                "INSERT INTO session_model_usage (session_id, model, input_tokens, \
                     output_tokens, estimated_cost_usd, first_seen, last_seen) \
                 VALUES ('s1', 'gpt-x', 100, 10, 0.5, ?1, ?1)",
                [now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, model, input_tokens, output_tokens, \
                     reasoning_tokens, estimated_cost_usd, started_at, ended_at) \
                 VALUES ('s1', 'gpt-x', 100, 10, 4, 0.5, ?1, ?1)",
                [now],
            )
            .unwrap();
        }
        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(total_bucket(&sessions, "gpt-x", "input_tokens"), 100);
        // Defaulting NULL to 0 would bill all 10 output tokens as output and leave reasoning at 0.
        assert_eq!(total_bucket(&sessions, "gpt-x", "output_tokens"), 6);
        assert_eq!(
            total_bucket(&sessions, "gpt-x", "reasoning_output_tokens"),
            4
        );
        assert!((total_cost(&sessions, "gpt-x") - 0.5).abs() < 1e-9);
        // The residual carried everything, so there is no loss to report.
        let read = read_hermes_usage_contributions(&db_path, TimeRange::All).unwrap();
        assert_eq!(read.failed_records(), 0);
        drop(dir);
    }

    #[test]
    fn residual_splits_reasoning_out_of_output() {
        // A residual-only session with output_tokens=10 and reasoning_tokens=4 reports output=6 and reasoning=4.
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
