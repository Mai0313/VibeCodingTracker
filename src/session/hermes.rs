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
use std::collections::HashSet;
use std::path::Path;

/// Reads per-model token usage from the Hermes database.
///
/// Each returned tuple is `(local YYYY-MM-DD date, CodeAnalysis, stored_cost)`,
/// where the `CodeAnalysis` holds one `session_model_usage` row's
/// `conversation_usage` keyed by that row's `model`, and `stored_cost` is
/// Hermes's own actual, included, or estimated cost. This compatibility API
/// represents a missing provider cost as zero; the compact aggregation path
/// retains that distinction and only falls back to LiteLLM when it is missing.
/// The date comes from the row's `last_seen` timestamp and is filtered by
/// `time_range`, matching the file-walker semantics.
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
    costs: Vec<RowCost>,
}

struct RowCost {
    output_index: Option<usize>,
    estimated: f64,
    cost: HermesCost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HermesCostBasis {
    Actual,
    Estimated,
    Included,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct HermesCost {
    basis: HermesCostBasis,
    amount: Option<f64>,
}

impl HermesCost {
    const fn unknown() -> Self {
        Self {
            basis: HermesCostBasis::Unknown,
            amount: None,
        }
    }
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

/// Returns the columns present in a known Hermes table.
fn table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    debug_assert!(matches!(table, "session_model_usage" | "sessions"));
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut columns = HashSet::new();
    while let Some(row) = rows.next()? {
        columns.insert(row.get(1)?);
    }
    Ok(columns)
}

/// Builds a stable select expression for a column added by newer Hermes
/// releases. The alias keeps row indexes identical on legacy databases.
fn optional_column(columns: &HashSet<String>, name: &str) -> String {
    if columns.contains(name) {
        name.to_string()
    } else {
        format!("NULL AS {name}")
    }
}

fn classify_cost(
    estimated: Option<f64>,
    actual: Option<f64>,
    status: Option<&str>,
    source: Option<&str>,
    billing_mode: Option<&str>,
) -> HermesCost {
    let estimated = valid_cost(estimated);
    let actual = valid_cost(actual);
    let status = status.map(str::trim).filter(|value| !value.is_empty());
    let source = source.map(str::trim).filter(|value| !value.is_empty());
    let billing_mode = billing_mode
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match status {
        Some(value) if value.eq_ignore_ascii_case("actual") => actual.map_or_else(
            || {
                estimated.map_or_else(HermesCost::unknown, |amount| HermesCost {
                    basis: HermesCostBasis::Estimated,
                    amount: Some(amount),
                })
            },
            |amount| HermesCost {
                basis: HermesCostBasis::Actual,
                amount: Some(amount),
            },
        ),
        Some(value) if value.eq_ignore_ascii_case("included") => HermesCost {
            basis: HermesCostBasis::Included,
            amount: Some(0.0),
        },
        _ if billing_mode
            .is_some_and(|value| value.eq_ignore_ascii_case("subscription_included")) =>
        {
            HermesCost {
                basis: HermesCostBasis::Included,
                amount: Some(0.0),
            }
        }
        Some(value) if value.eq_ignore_ascii_case("estimated") => HermesCost {
            basis: HermesCostBasis::Estimated,
            amount: estimated,
        },
        Some(value) if value.eq_ignore_ascii_case("unknown") => {
            positive_numeric_cost(estimated, actual).unwrap_or_else(HermesCost::unknown)
        }
        Some(value) => {
            log::warn!("Ignoring unrecognized Hermes cost status: {value}");
            positive_numeric_cost(estimated, actual).unwrap_or_else(HermesCost::unknown)
        }
        None => positive_numeric_cost(estimated, actual).unwrap_or_else(|| {
            if source.is_some_and(is_meaningful_cost_source) {
                HermesCost {
                    basis: HermesCostBasis::Estimated,
                    amount: estimated,
                }
            } else {
                HermesCost::unknown()
            }
        }),
    }
}

/// Retains a nonzero amount from legacy or mixed-status aggregates. Hermes
/// stores the latest status but accumulates costs, so an `unknown` final call
/// can coexist with valid cost from earlier calls in the same row.
fn positive_numeric_cost(estimated: Option<f64>, actual: Option<f64>) -> Option<HermesCost> {
    if actual.is_some_and(|value| value > 0.0) {
        Some(HermesCost {
            basis: HermesCostBasis::Actual,
            amount: actual,
        })
    } else if estimated.is_some_and(|value| value > 0.0) {
        Some(HermesCost {
            basis: HermesCostBasis::Estimated,
            amount: estimated,
        })
    } else {
        None
    }
}

fn valid_cost(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn is_meaningful_cost_source(source: &str) -> bool {
    !source.eq_ignore_ascii_case("none") && !source.eq_ignore_ascii_case("unknown")
}

/// Emits one contribution per `session_model_usage` row and accumulates the raw
/// per-session sums used by the residual reconciliation.
fn collect_per_model_rows(
    conn: &Connection,
    cutoff: &Option<String>,
    summed: &mut FastHashMap<String, RowSums>,
    out: &mut Vec<UsageContribution>,
) -> Result<()> {
    let columns = table_columns(conn, "session_model_usage")?;
    let query = format!(
        "SELECT session_id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, last_seen, first_seen, {}, {}, {} \
         FROM session_model_usage",
        optional_column(&columns, "cost_status"),
        optional_column(&columns, "cost_source"),
        optional_column(&columns, "billing_mode"),
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let model: String = row.get(1)?;
        let input = row.get::<_, i64>(2)?;
        let raw_output = row.get::<_, i64>(3)?;
        let cache_read = row.get::<_, i64>(4)?;
        let cache_write = row.get::<_, i64>(5)?;
        let reasoning = row.get::<_, i64>(6)?;
        let estimated = row.get::<_, Option<f64>>(7)?;
        let actual = row.get::<_, Option<f64>>(8)?;
        // `last_seen` (last activity) drives the date, falling back to `first_seen`.
        let seconds = row
            .get::<_, Option<f64>>(9)?
            .or(row.get::<_, Option<f64>>(10)?);
        let cost_status = row.get::<_, Option<String>>(11)?;
        let cost_source = row.get::<_, Option<String>>(12)?;
        let billing_mode = row.get::<_, Option<String>>(13)?;
        let cost = classify_cost(
            estimated,
            actual,
            cost_status.as_deref(),
            cost_source.as_deref(),
            billing_mode.as_deref(),
        );

        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        let Some(seconds) = seconds else { continue };
        let Some(date) = ms_to_local_date((seconds * 1000.0) as i64) else {
            continue;
        };

        // Accumulate raw sums for the residual, even for valid rows outside
        // the requested time window. Invalid rows cannot be emitted, so they
        // must not consume the session residual and make usage disappear.
        let acc = summed.entry(session_id).or_default();
        acc.input += input;
        acc.output += raw_output;
        acc.cache_read += cache_read;
        acc.cache_write += cache_write;
        acc.reasoning += reasoning;

        let output_index = if is_before_cutoff(&date, cutoff) {
            None
        } else {
            // Hermes bills through an OpenAI-compatible layer where
            // `output_tokens` already includes reasoning, so subtract it back
            // out to keep each token billed once.
            let output = (raw_output - reasoning).max(0);
            let output_index = out.len();
            out.push(UsageContribution::single_model(
                date,
                (seconds * 1000.0) as i64,
                model.to_string(),
                session_usage_value(input, output, reasoning, cache_read, cache_write),
                cost.amount,
            ));
            Some(output_index)
        };
        acc.costs.push(RowCost {
            output_index,
            estimated: estimated.unwrap_or(0.0),
            cost,
        });
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
    if !table_exists(conn, "sessions")? {
        return Ok(());
    }
    let columns = table_columns(conn, "sessions")?;
    let query = format!(
        "SELECT id, model, input_tokens, output_tokens, cache_read_tokens, \
                cache_write_tokens, reasoning_tokens, estimated_cost_usd, \
                actual_cost_usd, ended_at, started_at, {}, {}, {} \
         FROM sessions",
        optional_column(&columns, "cost_status"),
        optional_column(&columns, "cost_source"),
        optional_column(&columns, "billing_mode"),
    );
    let mut stmt = conn.prepare(&query)?;
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
        let reasoning =
            (row.get::<_, Option<i64>>(6)?.unwrap_or(0) - acc.map_or(0, |a| a.reasoning)).max(0);
        let output = (raw_output - reasoning).max(0);
        let session_estimated = row.get::<_, Option<f64>>(7)?;
        let session_actual = row.get::<_, Option<f64>>(8)?;
        let cost_status = row.get::<_, Option<String>>(11)?;
        let cost_source = row.get::<_, Option<String>>(12)?;
        let billing_mode = row.get::<_, Option<String>>(13)?;
        let session_cost = classify_cost(
            session_estimated,
            session_actual,
            cost_status.as_deref(),
            cost_source.as_deref(),
            billing_mode.as_deref(),
        );
        let cost_residual = reconcile_session_costs(acc, session_cost, out);
        if input == 0
            && raw_output == 0
            && cache_read == 0
            && cache_write == 0
            && cost_residual.unwrap_or(0.0) <= 0.0
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

        out.push(UsageContribution::single_model(
            date,
            (seconds * 1000.0) as i64,
            model.to_string(),
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost_residual,
        ));
    }
    Ok(())
}

/// Reconciles every emitted model row against one authoritative session cost
/// basis. Actual and estimated amounts are never stacked on top of each other.
fn reconcile_session_costs(
    acc: Option<&RowSums>,
    session_cost: HermesCost,
    out: &mut [UsageContribution],
) -> Option<f64> {
    let Some(acc) = acc else {
        return session_cost.amount;
    };
    let session_total = session_cost.amount?;

    // An authoritative zero aggregate leaves no cost to allocate. Preserve it
    // on every row so a missing per-model status cannot turn a confirmed free
    // session into a LiteLLM estimate.
    if session_total == 0.0 {
        for row in &acc.costs {
            set_row_stored_cost(out, row, Some(0.0));
        }
        return Some(0.0);
    }

    if session_cost.basis == HermesCostBasis::Actual {
        let actual_sum: f64 = acc
            .costs
            .iter()
            .filter(|row| row.cost.basis == HermesCostBasis::Actual)
            .filter_map(|row| row.cost.amount)
            .sum();
        if actual_sum > session_total {
            let scale = session_total / actual_sum;
            for row in &acc.costs {
                let stored_cost = match row.cost.basis {
                    HermesCostBasis::Actual => row.cost.amount.map(|amount| amount * scale),
                    HermesCostBasis::Estimated | HermesCostBasis::Included => Some(0.0),
                    HermesCostBasis::Unknown => Some(0.0),
                };
                set_row_stored_cost(out, row, stored_cost);
            }
            log::warn!(
                "Hermes per-model actual cost exceeds the session total; normalized row shares"
            );
            return Some(0.0);
        }

        let residual = (session_total - actual_sum).max(0.0);
        let estimate_weight: f64 = acc
            .costs
            .iter()
            .filter(|row| row.cost.basis == HermesCostBasis::Estimated)
            .map(|row| row.estimated.max(0.0))
            .sum();
        for row in &acc.costs {
            let stored_cost = match row.cost.basis {
                HermesCostBasis::Actual => row.cost.amount,
                HermesCostBasis::Estimated if estimate_weight > 0.0 => {
                    Some(residual * row.estimated.max(0.0) / estimate_weight)
                }
                HermesCostBasis::Estimated | HermesCostBasis::Included => Some(0.0),
                HermesCostBasis::Unknown => Some(0.0),
            };
            set_row_stored_cost(out, row, stored_cost);
        }
        return Some(if estimate_weight == 0.0 {
            residual
        } else {
            0.0
        });
    }

    if session_cost.basis == HermesCostBasis::Estimated {
        let covered_estimate: f64 = acc
            .costs
            .iter()
            .filter(|row| row.cost.basis != HermesCostBasis::Unknown)
            .map(|row| row.estimated.max(0.0))
            .sum();
        let scale = if covered_estimate > session_total && covered_estimate > 0.0 {
            log::warn!(
                "Hermes per-model estimated cost exceeds the session total; normalized row shares"
            );
            session_total / covered_estimate
        } else {
            1.0
        };
        for row in &acc.costs {
            let stored_cost = match row.cost.basis {
                HermesCostBasis::Actual => row.cost.amount,
                HermesCostBasis::Estimated => row.cost.amount.map(|amount| amount * scale),
                HermesCostBasis::Included => Some(0.0),
                HermesCostBasis::Unknown => Some(0.0),
            };
            set_row_stored_cost(out, row, stored_cost);
        }
        return Some((session_total - covered_estimate * scale).max(0.0));
    }

    if session_cost.basis == HermesCostBasis::Included {
        for row in &acc.costs {
            set_row_stored_cost(out, row, Some(0.0));
        }
        return Some(0.0);
    }

    None
}

fn set_row_stored_cost(out: &mut [UsageContribution], row: &RowCost, stored_cost: Option<f64>) {
    if let Some(output_index) = row.output_index {
        out[output_index].stored_cost = stored_cost;
    }
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

    fn add_cost_metadata_columns(conn: &Connection) {
        conn.execute_batch(
            "ALTER TABLE session_model_usage ADD COLUMN cost_status TEXT;
             ALTER TABLE session_model_usage ADD COLUMN cost_source TEXT;
             ALTER TABLE session_model_usage ADD COLUMN billing_mode TEXT;
             ALTER TABLE sessions ADD COLUMN cost_status TEXT;
             ALTER TABLE sessions ADD COLUMN cost_source TEXT;
             ALTER TABLE sessions ADD COLUMN billing_mode TEXT;",
        )
        .unwrap();
    }

    fn update_row_cost_metadata(
        conn: &Connection,
        session_id: &str,
        status: Option<&str>,
        source: Option<&str>,
        billing_mode: Option<&str>,
    ) {
        conn.execute(
            "UPDATE session_model_usage
             SET cost_status = ?2, cost_source = ?3, billing_mode = ?4
             WHERE session_id = ?1",
            rusqlite::params![session_id, status, source, billing_mode],
        )
        .unwrap();
    }

    fn update_session_cost_metadata(
        conn: &Connection,
        session_id: &str,
        status: Option<&str>,
        source: Option<&str>,
        billing_mode: Option<&str>,
    ) {
        conn.execute(
            "UPDATE sessions
             SET cost_status = ?2, cost_source = ?3, billing_mode = ?4
             WHERE id = ?1",
            rusqlite::params![session_id, status, source, billing_mode],
        )
        .unwrap();
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
    fn modern_cost_metadata_distinguishes_known_costs_from_unknown() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            add_cost_metadata_columns(&conn);

            insert_row(
                &conn,
                "actual-zero",
                "actual-zero-model",
                "provider",
                10,
                1,
                0,
                0,
                0,
                9.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(&conn, "actual-zero", Some("actual"), Some("provider"), None);

            insert_row(
                &conn,
                "included-zero",
                "included-zero-model",
                "openai-codex",
                20,
                2,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(
                &conn,
                "included-zero",
                Some("included"),
                Some("none"),
                Some("subscription_included"),
            );

            insert_row(
                &conn,
                "estimated",
                "estimated-model",
                "openai",
                30,
                3,
                0,
                0,
                0,
                0.75,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(
                &conn,
                "estimated",
                Some("estimated"),
                Some("official_docs_snapshot"),
                None,
            );

            insert_row(
                &conn,
                "estimated-zero",
                "estimated-zero-model",
                "openai",
                35,
                3,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(
                &conn,
                "estimated-zero",
                Some("estimated"),
                Some("official_docs_snapshot"),
                None,
            );

            insert_row(
                &conn,
                "unknown",
                "unknown-model",
                "custom",
                40,
                4,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(&conn, "unknown", Some("unknown"), Some("none"), None);
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        let stored_cost = |model: &str| {
            rows.iter()
                .find(|row| row.model == model)
                .unwrap()
                .stored_cost
        };
        assert_eq!(stored_cost("actual-zero-model"), Some(0.0));
        assert_eq!(stored_cost("included-zero-model"), Some(0.0));
        assert_eq!(stored_cost("estimated-model"), Some(0.75));
        assert_eq!(stored_cost("estimated-zero-model"), Some(0.0));
        assert_eq!(stored_cost("unknown-model"), None);
        drop(dir);
    }

    #[test]
    fn actual_status_without_actual_amount_falls_back_to_estimate() {
        let cost = classify_cost(Some(0.75), None, Some("actual"), Some("provider"), None);
        assert_eq!(cost.basis, HermesCostBasis::Estimated);
        assert_eq!(cost.amount, Some(0.75));
    }

    #[test]
    fn billing_mode_and_cost_source_preserve_explicit_zero_costs() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            add_cost_metadata_columns(&conn);
            insert_row(
                &conn,
                "included",
                "included-model",
                "openai-codex",
                10,
                1,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(&conn, "included", None, None, Some("subscription_included"));
            insert_row(
                &conn,
                "estimated",
                "source-model",
                "openai",
                10,
                1,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(
                &conn,
                "estimated",
                None,
                Some("official_docs_snapshot"),
                None,
            );
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.stored_cost == Some(0.0)));
        drop(dir);
    }

    #[test]
    fn legacy_schema_uses_positive_costs_and_leaves_zero_unknown() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "estimated",
                "estimated-model",
                "provider",
                10,
                1,
                0,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );
            insert_row(
                &conn,
                "unknown",
                "unknown-model",
                "provider",
                10,
                1,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        let stored_cost = |model: &str| {
            rows.iter()
                .find(|row| row.model == model)
                .unwrap()
                .stored_cost
        };
        assert_eq!(stored_cost("estimated-model"), Some(0.5));
        assert_eq!(stored_cost("unknown-model"), None);
        drop(dir);
    }

    #[test]
    fn session_actual_zero_overrides_its_nonzero_estimate() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            add_cost_metadata_columns(&conn);
            insert_row(
                &conn,
                "s1",
                "actual-zero-model",
                "provider",
                100,
                10,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(&conn, "s1", Some("unknown"), Some("none"), None);
            insert_session(
                &conn,
                "s1",
                "actual-zero-model",
                100,
                10,
                0,
                0,
                1.5,
                0.0,
                recent_epoch_secs(),
            );
            update_session_cost_metadata(&conn, "s1", Some("actual"), Some("provider"), None);
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stored_cost, Some(0.0));
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

    #[test]
    fn filtered_cost_reconciliation_includes_out_of_range_row_metadata() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            let old = chrono::Local::now().timestamp() as f64 - 10.0 * 86_400.0;
            insert_row(&conn, "s1", "old-model", "p", 60, 6, 0, 0, 0, 0.0, 6.0, old);
            insert_row(
                &conn,
                "s1",
                "recent-model",
                "p",
                40,
                4,
                0,
                0,
                0,
                0.0,
                4.0,
                recent_epoch_secs(),
            );
            insert_session(
                &conn,
                "s1",
                "recent-model",
                100,
                10,
                0,
                0,
                0.0,
                10.0,
                recent_epoch_secs(),
            );
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::Daily)
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "recent-model");
        assert_eq!(rows[0].stored_cost, Some(4.0));
        let public = read_hermes_usage(&db_path, TimeRange::Daily).unwrap();
        assert_eq!(public.len(), 1);
        assert!((public[0].2 - 4.0).abs() < 1e-9);
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
    fn invalid_per_model_rows_do_not_consume_session_residual() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "blank-model",
                "   ",
                "provider",
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
                "blank-model",
                "recovered-blank",
                100,
                10,
                0,
                0,
                0.5,
                0.0,
                recent_epoch_secs(),
            );

            conn.execute(
                "INSERT INTO session_model_usage (
                     session_id, model, billing_provider, input_tokens, output_tokens,
                     estimated_cost_usd, actual_cost_usd, first_seen, last_seen
                 ) VALUES ('missing-time', 'ignored-model', 'provider', 70, 7, 0.35, 0, NULL, NULL)",
                [],
            )
            .unwrap();
            insert_session(
                &conn,
                "missing-time",
                "recovered-time",
                120,
                12,
                0,
                0,
                0.6,
                0.0,
                recent_epoch_secs(),
            );
        }

        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(
            total_bucket(&sessions, "recovered-blank", "input_tokens"),
            100
        );
        assert_eq!(
            total_bucket(&sessions, "recovered-time", "input_tokens"),
            120
        );
        assert!(sessions.iter().all(|(_, analysis, _)| {
            !analysis.records[0]
                .conversation_usage
                .contains_key("ignored-model")
        }));
        drop(dir);
    }

    #[test]
    fn session_actual_does_not_stack_on_row_estimates() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "model-a",
                "provider",
                60,
                6,
                0,
                0,
                0,
                0.0,
                6.0,
                recent_epoch_secs(),
            );
            insert_row(
                &conn,
                "s1",
                "model-b",
                "provider",
                40,
                4,
                0,
                0,
                0,
                4.0,
                0.0,
                recent_epoch_secs(),
            );
            insert_session(
                &conn,
                "s1",
                "model-a",
                100,
                10,
                0,
                0,
                0.0,
                10.0,
                recent_epoch_secs(),
            );
        }

        let sessions = read_hermes_usage(&db_path, TimeRange::All).unwrap();
        let total: f64 = sessions.iter().map(|(_, _, cost)| *cost).sum();
        assert!((total - 10.0).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn authoritative_session_cost_suppresses_fallback_for_unknown_rows() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_row(
                &conn,
                "s1",
                "actual-model",
                "provider",
                60,
                6,
                0,
                0,
                0,
                0.0,
                6.0,
                recent_epoch_secs(),
            );
            insert_row(
                &conn,
                "s1",
                "unknown-model",
                "provider",
                40,
                4,
                0,
                0,
                0,
                0.0,
                0.0,
                recent_epoch_secs(),
            );
            insert_session(
                &conn,
                "s1",
                "session-model",
                100,
                10,
                0,
                0,
                0.0,
                10.0,
                recent_epoch_secs(),
            );
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        let pricing = crate::pricing::ModelPricing {
            input_cost_per_token: 1.0,
            output_cost_per_token: 1.0,
            ..Default::default()
        };
        let priced_total: f64 = rows
            .iter()
            .map(|row| {
                row.stored_cost.unwrap_or_else(|| {
                    crate::pricing::calculate_cost(
                        row.tokens.input_tokens,
                        row.tokens.output_tokens,
                        row.tokens.reasoning_tokens,
                        row.tokens.cache_read_tokens,
                        row.tokens.cache_creation_tokens,
                        0,
                        &pricing,
                    )
                })
            })
            .sum();
        let unknown = rows
            .iter()
            .find(|row| row.model == "unknown-model")
            .unwrap();
        assert_eq!(unknown.stored_cost, Some(0.0));
        assert!((priced_total - 10.0).abs() < 1e-9);
        drop(dir);
    }

    #[test]
    fn estimated_session_preserves_actual_row_and_only_estimates_residual() {
        let (dir, db_path) = make_db();
        {
            let conn = Connection::open(&db_path).unwrap();
            add_cost_metadata_columns(&conn);
            insert_row(
                &conn,
                "s1",
                "actual-model",
                "provider",
                60,
                6,
                0,
                0,
                0,
                6.0,
                8.0,
                recent_epoch_secs(),
            );
            update_row_cost_metadata(&conn, "s1", Some("actual"), Some("provider"), None);
            insert_session(
                &conn,
                "s1",
                "residual-model",
                100,
                10,
                0,
                0,
                10.0,
                0.0,
                recent_epoch_secs(),
            );
            update_session_cost_metadata(
                &conn,
                "s1",
                Some("estimated"),
                Some("official_docs_snapshot"),
                None,
            );
        }

        let rows = read_hermes_usage_contributions(&db_path, TimeRange::All)
            .unwrap()
            .rows;
        let actual = rows.iter().find(|row| row.model == "actual-model").unwrap();
        let residual = rows
            .iter()
            .find(|row| row.model == "residual-model")
            .unwrap();
        assert_eq!(actual.stored_cost, Some(8.0));
        assert_eq!(residual.stored_cost, Some(4.0));
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
