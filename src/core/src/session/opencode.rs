//! OpenCode session reader for the local SQLite database (`~/.local/share/opencode/opencode.db`).
//!
//! - [`read_opencode_usage`] reads assistant messages for per-model tokens and stored cost, falling back to the legacy `session` table when no `message` table exists.
//! - [`read_opencode_analysis`] additionally folds tracked `part` tool calls (`read`, `edit`, `write`, `bash`, `todowrite`, `apply_patch`) into per-message file-operation metrics.
//!
//! Output uses the shared [`CodeAnalysis`] shape with disjoint token buckets (`input`, `output`, `reasoning`, `cache_read`, `cache_write`). Assistant messages carry their own `providerID` and `modelID`, splitting mid-session model switches into separate rows.

use crate::VERSION;
use crate::constants::FastHashMap;
use crate::models::TimeRange;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, ExtensionType};
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{
    DatabaseAnalysisRow, DatabaseUsageRead, UsageContribution, UsageTokenContribution,
};
use crate::session::sqlite::with_readonly_connection;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Result, anyhow};
use rusqlite::{Connection, types::ValueRef};
use serde::{Deserialize, Deserializer};
#[cfg(test)]
use serde_json::json;
use serde_json::{Value, value::RawValue};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

/// Returns per-message token usage from the OpenCode database at `db_path` within `time_range`.
///
/// Each tuple is `(local YYYY-MM-DD, CodeAnalysis, stored_cost)`, where the [`CodeAnalysis`] holds one assistant message's `conversation_usage` keyed by its provider-qualified model id and `stored_cost` is OpenCode's stored cost for that message. Dates come from the message timestamp (`time.completed`, `time.created`, or `session.time_updated`). Databases without a `message` table fall back to one row per `session`, dated by `time_updated`.
///
/// # Errors
///
/// Returns an error if `db_path` cannot be opened or queried, or if expected usage records exist and none used a recognized schema.
pub fn read_opencode_usage(
    db_path: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let read = read_opencode_usage_contributions(db_path, time_range, None)?;
    if read.expected_records > 0 && read.parsed_records == 0 {
        return Err(anyhow!(
            "none of {} OpenCode usage records used a recognized schema",
            read.expected_records
        ));
    }
    if read.failed_records() > 0 {
        log::warn!(
            "{} OpenCode usage records used an unsupported schema",
            read.failed_records()
        );
    }
    Ok(read
        .rows
        .into_iter()
        .map(|row| row.into_public_row(ExtensionType::OpenCode, &user, &machine))
        .collect())
}

/// Reads compact OpenCode usage contributions within `time_range`, classifying each assistant message against `tiers` (or leaving all rows at base tier when `None`).
pub(crate) fn read_opencode_usage_contributions(
    db_path: &Path,
    time_range: TimeRange,
    tiers: Option<&TierThresholds>,
) -> Result<DatabaseUsageRead> {
    let cutoff_ms = cutoff_millis(time_range);
    with_readonly_connection(db_path, "session", "vct-opencode-", "OpenCode", |conn| {
        collect_usage(conn, cutoff_ms, tiers)
    })
}

/// Returns per-message file-operation metrics from the OpenCode database at `db_path` within `time_range`.
///
/// Like [`read_opencode_usage`], and also folds tracked `part` tool calls into `tool_call_counts` and `total_*` line/character metrics inside a single SQLite transaction for snapshot consistency. `mode` controls whether per-operation detail bodies are retained ([`ParseMode::Full`]) or skipped ([`ParseMode::UsageOnly`]).
///
/// # Errors
///
/// Returns an error if `db_path` cannot be opened or queried, or if expected analysis records exist and none used a recognized schema.
pub fn read_opencode_analysis(
    db_path: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let result = read_opencode_analysis_with_diagnostics(db_path, time_range, mode)?;
    if result.expected_records > 0 && result.parsed_records == 0 {
        return Err(anyhow!(
            "none of {} OpenCode analysis records used a recognized schema",
            result.expected_records
        ));
    }
    // Log record and tool-part failures separately because one message can contain many parts.
    let failed_records = result
        .expected_records
        .saturating_sub(result.parsed_records);
    if failed_records > 0 {
        log::warn!("skipped {failed_records} OpenCode analysis records with unsupported schema");
    }
    if result.failed_tool_parts > 0 {
        log::warn!(
            "skipped {} OpenCode tool parts with unsupported schema",
            result.failed_tool_parts
        );
    }
    Ok(result
        .rows
        .into_iter()
        .map(|row| (row.date, row.analysis))
        .collect())
}

/// OpenCode rows plus record-recognition diagnostics for batch collection.
pub(crate) struct OpenCodeAnalysisRead {
    /// Successfully normalized assistant/session rows.
    pub rows: Vec<DatabaseAnalysisRow>,
    /// Rows selected by the provider's current role/time predicates.
    pub expected_records: usize,
    /// Selected rows that produced a normalized `CodeAnalysis`.
    pub parsed_records: usize,
    /// Tool parts the analyzer could not normalize, including unreadable JSON.
    pub failed_tool_parts: usize,
}

/// Reads OpenCode analysis while retaining unsupported-record counts.
pub(crate) fn read_opencode_analysis_with_diagnostics(
    db_path: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<OpenCodeAnalysisRead> {
    let cutoff_ms = cutoff_millis(time_range);
    with_readonly_connection(db_path, "session", "vct-opencode-", "OpenCode", |conn| {
        let transaction = conn.unchecked_transaction()?;
        let collected = collect_analysis(&transaction, cutoff_ms, mode)?;
        let parsed_records = collected.rows.len();
        transaction.commit()?;
        Ok(OpenCodeAnalysisRead {
            rows: collected.rows,
            expected_records: collected.expected_records,
            parsed_records,
            failed_tool_parts: collected.failed_tool_parts,
        })
    })
}

/// Parsed usage for one OpenCode assistant message.
struct MessageUsage {
    model_id: String,
    usage: UsageTokenContribution,
    cost: f64,
}

/// Per-record accumulator used while folding tool parts.
struct AnalysisAccum {
    model_id: String,
    date: String,
    usage: Value,
    state: SessionParseState,
}

struct CollectedAnalysis {
    rows: Vec<DatabaseAnalysisRow>,
    expected_records: usize,
    failed_tool_parts: usize,
}

/// SQL expression resolving an assistant message's epoch-ms timestamp (`time.completed`, `time.created`, or `session.time_updated`); requires both `message` and `session` in scope.
// Guard with `typeof` so non-numeric JSON values (e.g. ISO-8601 strings) fall through instead of sorting above integers or casting their leading digits; `CAST` converts reals so rusqlite can read `i64`.
const MESSAGE_TIMESTAMP_SQL: &str = "COALESCE( \
     CASE WHEN typeof(json_extract(message.data, '$.time.completed')) IN ('integer', 'real') \
          THEN CAST(json_extract(message.data, '$.time.completed') AS INTEGER) END, \
     CASE WHEN typeof(json_extract(message.data, '$.time.created')) IN ('integer', 'real') \
          THEN CAST(json_extract(message.data, '$.time.created') AS INTEGER) END, \
     session.time_updated \
     )";

/// Collects the `usage` view from assistant messages when available.
fn collect_usage(
    conn: &Connection,
    cutoff_ms: Option<i64>,
    tiers: Option<&TierThresholds>,
) -> Result<DatabaseUsageRead> {
    if table_exists(conn, "message")? {
        return collect_message_usage(conn, cutoff_ms, tiers);
    }

    collect_session_usage(conn, cutoff_ms)
}

/// Collects the `usage` view from the legacy `session` columns.
fn collect_session_usage(conn: &Connection, cutoff_ms: Option<i64>) -> Result<DatabaseUsageRead> {
    let sql = match cutoff_ms {
        Some(_) => {
            "SELECT model, tokens_input, tokens_output, tokens_reasoning, \
                    tokens_cache_read, tokens_cache_write, time_updated, cost, id, directory \
             FROM session WHERE model IS NOT NULL AND model != '' AND time_updated >= ?1"
        }
        None => {
            "SELECT model, tokens_input, tokens_output, tokens_reasoning, \
                    tokens_cache_read, tokens_cache_write, time_updated, cost, id, directory \
             FROM session WHERE model IS NOT NULL AND model != ''"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = match cutoff_ms {
        Some(cutoff_ms) => stmt.query([cutoff_ms])?,
        None => stmt.query([])?,
    };

    let mut out = Vec::new();
    let mut expected_records = 0usize;
    while let Some(row) = rows.next()? {
        expected_records += 1;
        let model = row.get::<_, String>(0)?;
        let input = row.get::<_, i64>(1)?;
        let output = row.get::<_, i64>(2)?;
        let reasoning = row.get::<_, i64>(3)?;
        let cache_read = row.get::<_, i64>(4)?;
        let cache_write = row.get::<_, i64>(5)?;
        let time_updated = row.get::<_, i64>(6)?;
        let cost = row.get::<_, f64>(7)?;
        let session_id = row.get::<_, String>(8)?;
        let directory = row.get::<_, Option<String>>(9)?;
        let Some(model_id) = parse_model_id(&model) else {
            continue;
        };
        let Some(date) = ms_to_local_date(time_updated) else {
            continue;
        };

        // Legacy `session` rows hold conversation totals rather than single-request context, so tier level is always 0.
        let mut contribution = UsageContribution::single_model(
            session_id,
            date,
            time_updated,
            model_id,
            session_usage_value(input, output, reasoning, cache_read, cache_write),
            cost,
            0,
        );
        contribution.cwd = directory.filter(|directory| !directory.is_empty());
        out.push(contribution);
    }

    let parsed_records = out.len();
    Ok(DatabaseUsageRead {
        rows: out,
        expected_records,
        parsed_records,
    })
}

/// Collects the `usage` view from assistant messages.
fn collect_message_usage(
    conn: &Connection,
    cutoff_ms: Option<i64>,
    tiers: Option<&TierThresholds>,
) -> Result<DatabaseUsageRead> {
    let sql = match cutoff_ms {
        Some(_) => format!(
            "SELECT {MESSAGE_TIMESTAMP_SQL}, message.data, message.session_id, session.directory \
             FROM message \
             JOIN session ON session.id = message.session_id \
             WHERE json_extract(message.data, '$.role') = 'assistant' \
               AND {MESSAGE_TIMESTAMP_SQL} >= ?1"
        ),
        None => format!(
            "SELECT {MESSAGE_TIMESTAMP_SQL}, message.data, message.session_id, session.directory \
             FROM message \
             JOIN session ON session.id = message.session_id \
             WHERE json_extract(message.data, '$.role') = 'assistant'"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = match cutoff_ms {
        Some(cutoff_ms) => stmt.query([cutoff_ms])?,
        None => stmt.query([])?,
    };

    let mut classifier = tiers.map(TierClassifier::new);
    let mut out = Vec::new();
    let mut expected_records = 0usize;
    while let Some(row) = rows.next()? {
        expected_records += 1;
        let message_ts = row.get::<_, i64>(0)?;
        let data_text = row.get_ref(1)?.as_str()?;
        let Some(message) = parse_message_usage(data_text) else {
            continue;
        };
        let Some(date) = ms_to_local_date(message_ts) else {
            continue;
        };
        let session_id = row.get::<_, String>(2)?;
        let directory = row.get::<_, Option<String>>(3)?;

        let tier_level = classifier.as_mut().map_or(0, |classifier| {
            classifier.level(&message.model_id, request_context(&message.usage))
        });
        let mut contribution = UsageContribution::single_model(
            session_id,
            date,
            message_ts,
            message.model_id,
            message.usage,
            message.cost,
            tier_level,
        );
        contribution.cwd = directory.filter(|directory| !directory.is_empty());
        out.push(contribution);
    }

    let parsed_records = out.len();
    Ok(DatabaseUsageRead {
        rows: out,
        expected_records,
        parsed_records,
    })
}

/// Collects the `analysis` view from assistant messages + parts when available.
fn collect_analysis(
    conn: &Connection,
    cutoff_ms: Option<i64>,
    mode: ParseMode,
) -> Result<CollectedAnalysis> {
    if table_exists(conn, "message")? {
        return collect_message_analysis(conn, cutoff_ms, mode);
    }

    collect_session_analysis(conn, cutoff_ms, mode)
}

/// Collects the legacy `analysis` view from `session` + `part`.
fn collect_session_analysis(
    conn: &Connection,
    cutoff_ms: Option<i64>,
    mode: ParseMode,
) -> Result<CollectedAnalysis> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();

    // 1. Load session metadata and seed one parse state per session.
    let mut sessions: HashMap<String, AnalysisAccum> = HashMap::new();
    let mut expected_records = 0usize;
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
            expected_records += 1;
            let id = row.get::<_, String>(0)?;
            let model = row.get_ref(1)?.as_str()?;
            let directory = row.get::<_, String>(2)?;
            let ts = row.get::<_, i64>(3)?;
            let input = row.get::<_, i64>(4)?;
            let output = row.get::<_, i64>(5)?;
            let reasoning = row.get::<_, i64>(6)?;
            let cache_read = row.get::<_, i64>(7)?;
            let cache_write = row.get::<_, i64>(8)?;
            let Some(model_id) = parse_model_id(model) else {
                continue;
            };
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };

            // Analysis is never priced, so it classifies nothing.
            let usage = session_usage_value(input, output, reasoning, cache_read, cache_write)
                .into_value(0);
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
    let mut failed_tool_parts = 0usize;
    {
        let sql = match cutoff_ms {
            Some(_) => {
                "SELECT part.session_id, part.data \
                 FROM part \
                 JOIN session ON session.id = part.session_id \
                 WHERE json_extract(part.data, '$.type') = 'tool' \
                   AND session.model IS NOT NULL \
                   AND session.model != '' \
                   AND session.time_updated >= ?1 \
                 ORDER BY part.id"
            }
            None => {
                "SELECT session_id, data \
                 FROM part \
                 WHERE json_extract(data, '$.type') = 'tool' \
                 ORDER BY id"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let ValueRef::Text(session_id_bytes) = row.get_ref(0)? else {
                continue;
            };
            let Ok(session_id) = std::str::from_utf8(session_id_bytes) else {
                continue;
            };
            let Some(accum) = sessions.get_mut(session_id) else {
                continue;
            };
            let data_text = row.get_ref(1)?.as_str()?;
            fold_tool_part_str(&mut accum.state, data_text, &mut failed_tool_parts);
        }
    }

    // 3. Convert each session into a CodeAnalysis.
    let mut out = Vec::with_capacity(sessions.len());
    for (id, accum) in sessions {
        let mut usage_map = FastHashMap::default();
        usage_map.insert(accum.model_id, accum.usage);
        let record = accum.state.into_record(usage_map);
        out.push(DatabaseAnalysisRow {
            session_id: id.clone(),
            source_id: id,
            date: accum.date,
            analysis: wrap_record(record, &user, &machine),
        });
    }

    Ok(CollectedAnalysis {
        rows: out,
        expected_records,
        failed_tool_parts,
    })
}

/// Collects the `analysis` view from assistant messages and their parts.
fn collect_message_analysis(
    conn: &Connection,
    cutoff_ms: Option<i64>,
    mode: ParseMode,
) -> Result<CollectedAnalysis> {
    let user = get_current_user();
    let machine = get_machine_id().to_string();

    let mut messages: HashMap<String, AnalysisAccum> = HashMap::new();
    let mut expected_records = 0usize;
    {
        let sql = match cutoff_ms {
            Some(_) => format!(
                "SELECT message.id, message.session_id, message.data, session.directory, {MESSAGE_TIMESTAMP_SQL} \
                 FROM message \
                 JOIN session ON session.id = message.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant' \
                   AND {MESSAGE_TIMESTAMP_SQL} >= ?1"
            ),
            None => format!(
                "SELECT message.id, message.session_id, message.data, session.directory, {MESSAGE_TIMESTAMP_SQL} \
                 FROM message \
                 JOIN session ON session.id = message.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant'"
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            expected_records += 1;
            let message_id = row.get::<_, String>(0)?;
            let session_id = row.get::<_, String>(1)?;
            let data_text = row.get_ref(2)?.as_str()?;
            let directory = row.get::<_, String>(3)?;
            let message_ts = row.get::<_, i64>(4)?;
            let Some(message) = parse_message_usage(data_text) else {
                continue;
            };
            let Some(date) = ms_to_local_date(message_ts) else {
                continue;
            };

            let mut state = SessionParseState::with_mode(mode);
            state.folder_path = directory;
            state.task_id = session_id;
            state.last_ts = message_ts;

            messages.insert(
                message_id,
                AnalysisAccum {
                    model_id: message.model_id,
                    date,
                    // Analysis is never priced, so tier level is always 0.
                    usage: message.usage.into_value(0),
                    state,
                },
            );
        }
    }

    let mut failed_tool_parts = 0usize;
    {
        let sql = match cutoff_ms {
            Some(_) => format!(
                "SELECT part.message_id, part.data \
                 FROM part \
                 JOIN message ON message.id = part.message_id \
                 JOIN session ON session.id = part.session_id \
                 WHERE json_extract(message.data, '$.role') = 'assistant' \
                   AND json_extract(part.data, '$.type') = 'tool' \
                   AND {MESSAGE_TIMESTAMP_SQL} >= ?1 \
                 ORDER BY part.id"
            ),
            // Without a cutoff, filter in memory against `messages` instead of joining and re-parsing `message.data` per part.
            None => "SELECT message_id, data \
                 FROM part \
                 WHERE json_extract(data, '$.type') = 'tool' \
                 ORDER BY id"
                .to_string(),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match cutoff_ms {
            Some(cutoff_ms) => stmt.query([cutoff_ms])?,
            None => stmt.query([])?,
        };

        while let Some(row) = rows.next()? {
            let ValueRef::Text(message_id_bytes) = row.get_ref(0)? else {
                continue;
            };
            let Ok(message_id) = std::str::from_utf8(message_id_bytes) else {
                continue;
            };
            let Some(accum) = messages.get_mut(message_id) else {
                continue;
            };
            let data_text = row.get_ref(1)?.as_str()?;
            fold_tool_part_str(&mut accum.state, data_text, &mut failed_tool_parts);
        }
    }

    let mut out = Vec::with_capacity(messages.len());
    for (id, accum) in messages {
        let mut usage_map = FastHashMap::default();
        usage_map.insert(accum.model_id, accum.usage);
        let session_id = accum.state.task_id.clone();
        let record = accum.state.into_record(usage_map);
        out.push(DatabaseAnalysisRow {
            session_id,
            source_id: id,
            date: accum.date,
            analysis: wrap_record(record, &user, &machine),
        });
    }

    Ok(CollectedAnalysis {
        rows: out,
        expected_records,
        failed_tool_parts,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolPartOutcome {
    Irrelevant,
    Normalized,
    Unsupported,
}

fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    <&'de RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BorrowedToolPart<'a> {
    #[serde(
        default,
        borrow,
        rename = "type",
        deserialize_with = "deserialize_present_raw"
    )]
    kind: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    tool: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    state: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedToolState<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    status: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    input: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    output: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    time: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedToolTime<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    start: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedToolInput<'a> {
    #[serde(
        default,
        borrow,
        rename = "filePath",
        deserialize_with = "deserialize_present_raw"
    )]
    file_path: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "oldString",
        deserialize_with = "deserialize_present_raw"
    )]
    old_string: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "newString",
        deserialize_with = "deserialize_present_raw"
    )]
    new_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    content: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    command: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    description: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "patchText",
        deserialize_with = "deserialize_present_raw"
    )]
    patch_text: Option<&'a RawValue>,
}

fn decode_raw_str(raw: Option<&RawValue>) -> Option<Cow<'_, str>> {
    serde_json::from_str::<Cow<'_, str>>(raw?.get()).ok()
}

fn fold_tool_part_str(
    state: &mut SessionParseState,
    data_text: &str,
    failed_tool_parts: &mut usize,
) {
    let part = match serde_json::from_str::<BorrowedToolPart<'_>>(data_text) {
        Ok(part) => part,
        Err(_) => {
            if serde_json::from_str::<Value>(data_text).is_err() {
                *failed_tool_parts += 1;
            }
            return;
        }
    };
    if decode_raw_str(part.kind).as_deref() != Some("tool") {
        return;
    }
    if apply_tool_part(state, &part) == ToolPartOutcome::Unsupported {
        *failed_tool_parts += 1;
    }
}

/// Folds one tracked `tool` part (`read`, `edit`, `write`, `bash`, `todowrite`, `apply_patch`) into `state`, ignoring auxiliary tools.
fn apply_tool_part(state: &mut SessionParseState, data: &BorrowedToolPart<'_>) -> ToolPartOutcome {
    let tool = decode_raw_str(data.tool);
    let tool = tool.as_deref().unwrap_or("");
    if !matches!(
        tool,
        "read" | "edit" | "write" | "bash" | "todowrite" | "apply_patch"
    ) {
        return ToolPartOutcome::Irrelevant;
    }

    let Some(state_raw) = data.state.filter(|raw| raw.get().starts_with('{')) else {
        return ToolPartOutcome::Unsupported;
    };
    let Ok(st) = serde_json::from_str::<BorrowedToolState<'_>>(state_raw.get()) else {
        return ToolPartOutcome::Unsupported;
    };
    let Some(status) = decode_raw_str(st.status) else {
        return ToolPartOutcome::Unsupported;
    };
    match status.as_ref() {
        "completed" => {}
        "pending" | "running" | "error" => return ToolPartOutcome::Irrelevant,
        _ => return ToolPartOutcome::Unsupported,
    }

    let input = st
        .input
        .filter(|raw| raw.get().starts_with('{'))
        .and_then(|raw| serde_json::from_str::<BorrowedToolInput<'_>>(raw.get()).ok());
    let ts = st
        .time
        .filter(|raw| raw.get().starts_with('{'))
        .and_then(|raw| serde_json::from_str::<BorrowedToolTime<'_>>(raw.get()).ok())
        .and_then(|t| t.start)
        .and_then(|raw| serde_json::from_str::<i64>(raw.get()).ok())
        .unwrap_or(state.last_ts);

    let str_in =
        |field: for<'a> fn(&'a BorrowedToolInput<'a>) -> Option<&'a RawValue>| -> Option<Cow<'_, str>> {
            decode_raw_str(input.as_ref().and_then(field))
        };

    match tool {
        "read" => {
            let (Some(path), Some(output)) = (
                str_in(|i| i.file_path).filter(|path| !path.is_empty()),
                decode_raw_str(st.output),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            let content = extract_read_content(&output);
            if !content.is_empty() {
                state.add_read_detail(&path, &content, ts);
            } else {
                // Directory listings and empty reads still count as read invocations.
                state.tool_counts.read += 1;
            }
        }
        "edit" => {
            let (Some(path), Some(old), Some(new)) = (
                str_in(|i| i.file_path).filter(|path| !path.is_empty()),
                str_in(|i| i.old_string),
                str_in(|i| i.new_string),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            state.add_edit_detail(&path, &old, &new, ts);
        }
        "write" => {
            let (Some(path), Some(content)) = (
                str_in(|i| i.file_path).filter(|path| !path.is_empty()),
                str_in(|i| i.content),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            state.add_write_detail(&path, &content, ts);
        }
        "bash" => {
            let Some(command) = str_in(|i| i.command).filter(|command| !command.trim().is_empty())
            else {
                return ToolPartOutcome::Unsupported;
            };
            let desc = str_in(|i| i.description);
            state.add_run_command(&command, desc.as_deref().unwrap_or(""), ts);
        }
        "todowrite" => {
            state.tool_counts.todo_write += 1;
        }
        "apply_patch" => {
            let Some(patch_text) = str_in(|i| i.patch_text) else {
                return ToolPartOutcome::Unsupported;
            };
            let patches = parse_apply_patch_text(&patch_text);
            if !patches.iter().any(|patch| !patch.file_path.is_empty()) {
                return ToolPartOutcome::Unsupported;
            }
            apply_parsed_patches(state, patches, ts);
        }
        _ => unreachable!("tracked OpenCode tool was filtered above"),
    }

    ToolPartOutcome::Normalized
}

/// Folds parsed OpenCode `apply_patch` sections into file-operation counts.
fn apply_parsed_patches(state: &mut SessionParseState, patches: Vec<OpenCodePatch>, ts: i64) {
    for patch in patches {
        let (old_str, new_str) = extract_patch_strings(&patch.lines);

        match patch.action.as_str() {
            "add" => state.add_write_detail(&patch.file_path, &new_str, ts),
            "delete" => state.add_edit_detail(&patch.file_path, &old_str, "", ts),
            // Updates target existing files, so insert-only hunks stay edits.
            _ => state.add_edit_detail_raw(&patch.file_path, &old_str, &new_str, ts),
        }
    }
}

/// One file's patch section extracted from an OpenCode `apply_patch` tool call.
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

/// Packs OpenCode's token counters directly into [`UsageTokenContribution`].
///
/// OpenCode's `input`, `output`, `reasoning`, `cache_read`, and `cache_write` counters are already mutually disjoint (`Session.getUsage` splits them before persistence); do not subtract `reasoning` from `output` or cache from `input`.
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

/// Returns the total prompt context (`input + cache_read + cache_creation`) of one assistant message for tier classification.
// Classify at message granularity to keep the `part` table off the usage path; assistant messages almost always represent a single billed step, and some lack `step-finish` parts.
fn request_context(tokens: &UsageTokenContribution) -> i64 {
    tokens.input_tokens + tokens.cache_read_tokens + tokens.cache_creation_tokens
}

/// Resolves `session.model` from either a `{"id", "providerID"}` JSON object (formatted as `providerID/id`) or a legacy bare model string.
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
        // Non-JSON text is the legacy plain-string column format.
        Err(_) => Some(raw.to_string()),
        // Reject non-string JSON scalars/arrays (such as literal `"null"`, which bypasses SQL `IS NOT NULL`).
        _ => None,
    }
}

#[derive(Deserialize)]
struct BorrowedMessageData<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    role: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "modelID",
        deserialize_with = "deserialize_present_raw"
    )]
    model_id_camel: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model_id: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "providerID",
        deserialize_with = "deserialize_present_raw"
    )]
    provider_id_camel: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    provider_id: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    cost: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedNestedModel<'a> {
    #[serde(
        default,
        borrow,
        rename = "modelID",
        deserialize_with = "deserialize_present_raw"
    )]
    model_id_camel: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    id: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "providerID",
        deserialize_with = "deserialize_present_raw"
    )]
    provider_id_camel: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    provider_id: Option<&'a RawValue>,
}

/// Parses one assistant `message.data` payload into usage and cost.
fn parse_message_usage(raw: &str) -> Option<MessageUsage> {
    let data = serde_json::from_str::<BorrowedMessageData<'_>>(raw).ok()?;
    if decode_raw_str(data.role).as_deref() != Some("assistant") {
        return None;
    }

    let model_id = message_model_id(&data)?;
    let tokens_val = serde_json::from_str::<Value>(data.tokens?.get()).ok()?;
    let usage = parse_message_tokens(&tokens_val)?;
    let cost = data
        .cost
        .and_then(|raw| serde_json::from_str::<f64>(raw.get()).ok())
        .unwrap_or(0.0);

    Some(MessageUsage {
        model_id,
        usage,
        cost,
    })
}

fn parse_message_tokens(tokens: &Value) -> Option<UsageTokenContribution> {
    let tokens = tokens.as_object()?;
    let read_i64 = |object: &serde_json::Map<String, Value>, key: &str| -> Option<i64> {
        match object.get(key) {
            Some(value) => value.as_i64(),
            None => Some(0),
        }
    };

    let has_flat_key = ["input", "output", "reasoning"]
        .iter()
        .any(|key| tokens.contains_key(*key));
    let input = read_i64(tokens, "input")?;
    let output = read_i64(tokens, "output")?;
    let reasoning = read_i64(tokens, "reasoning")?;

    let (cache_read, cache_write, has_cache_key) = match tokens.get("cache") {
        Some(cache) => {
            let cache = cache.as_object()?;
            let has_known_cache_key = ["read", "write"].iter().any(|key| cache.contains_key(*key));
            if !cache.is_empty() && !has_known_cache_key {
                return None;
            }
            (read_i64(cache, "read")?, read_i64(cache, "write")?, true)
        }
        None => (0, 0, false),
    };

    if !tokens.is_empty() && !has_flat_key && !has_cache_key {
        return None;
    }

    Some(session_usage_value(
        input,
        output,
        reasoning,
        cache_read,
        cache_write,
    ))
}

/// Resolves the model from an assistant message payload.
fn message_model_id(data: &BorrowedMessageData<'_>) -> Option<String> {
    let nested = data
        .model
        .filter(|raw| raw.get().starts_with('{'))
        .and_then(|raw| serde_json::from_str::<BorrowedNestedModel<'_>>(raw.get()).ok());

    let model_cow = decode_raw_str(data.model_id_camel.or(data.model_id)).or_else(|| {
        let nested = nested.as_ref()?;
        decode_raw_str(nested.model_id_camel.or(nested.id))
    })?;
    let model = model_cow.trim();
    if model.is_empty() {
        return None;
    }

    let provider_cow = decode_raw_str(data.provider_id_camel.or(data.provider_id)).or_else(|| {
        let nested = nested.as_ref()?;
        decode_raw_str(nested.provider_id_camel.or(nested.provider_id))
    });

    Some(provider_qualified_model_id(provider_cow.as_deref(), model))
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

/// Returns the inclusive local-date cutoff as an epoch-millis lower bound, or `None` only for [`TimeRange::All`].
fn cutoff_millis(time_range: TimeRange) -> Option<i64> {
    time_range
        .cutoff_date()
        .map(|date| day_start_millis(&chrono::Local, date))
}

/// Returns the epoch-millis of the first instant of `date` in `tz`.
fn day_start_millis<Tz: chrono::TimeZone>(tz: &Tz, date: chrono::NaiveDate) -> i64 {
    use chrono::{Offset, TimeDelta};

    let midnight = date.and_time(chrono::NaiveTime::MIN);
    // `earliest()` handles fall-back DST transitions where local midnight occurs twice.
    if let Some(start) = tz.from_local_datetime(&midnight).earliest() {
        return start.timestamp_millis();
    }
    // Spring-forward zones that jump at local midnight (e.g. `America/Santiago`) have no `00:00`; compute the transition instant using the pre-jump offset probed 24h earlier.
    let before = tz.offset_from_utc_datetime(&(midnight - TimeDelta::days(1)));
    (midnight - TimeDelta::seconds(i64::from(before.fix().local_minus_utc())))
        .and_utc()
        .timestamp_millis()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        // JSON scalars/arrays name no model even though they bypass SQL `IS NOT NULL`.
        assert_eq!(parse_model_id("null"), None);
        assert_eq!(parse_model_id("42"), None);
        assert_eq!(parse_model_id("[]"), None);
        // A quoted string is still the legacy plain-name column.
        assert_eq!(parse_model_id(r#""gpt-5""#).as_deref(), Some("gpt-5"));
    }

    #[test]
    fn test_message_model_id_preserves_provider() {
        let raw = serde_json::json!({
            "role": "assistant",
            "modelID": "gpt-4.1",
            "providerID": "azure",
        })
        .to_string();
        let message: BorrowedMessageData<'_> = serde_json::from_str(&raw).unwrap();
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

    /// Legacy `session`-table usage keeps `reasoning` disjoint from `output`.
    #[test]
    fn test_legacy_session_usage_keeps_reasoning_disjoint() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE message", []).unwrap();
        conn.execute("DROP TABLE part", []).unwrap();
        conn.execute(
	            "INSERT INTO session (id, model, directory, time_updated, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write)
	             VALUES ('s1', '{\"id\":\"gpt-5.4\"}', '/repo', 1780757088080, 0.02, 19691, 21, 26, 0, 0)",
	            [],
	        )
	        .unwrap();
        drop(conn);

        let rows = read_opencode_usage(&db_path, TimeRange::All).unwrap();
        assert_eq!(rows.len(), 1);
        let usage = &rows[0].1.records[0].conversation_usage["gpt-5.4"];
        // Real rows can have more reasoning than output; subtracting reasoning from output would wrongly floor output at 0.
        assert_eq!(usage["output_tokens"], 21);
        assert_eq!(usage["reasoning_output_tokens"], 26);
    }

    fn tier_snapshot() -> TierThresholds {
        TierThresholds::from_entries([("azure/gpt-5.5", vec![272_000])].into_iter())
    }

    /// Seeds two assistant messages straddling one tiered model's boundary and returns their usage contributions.
    fn straddling_tier_rows(tiers: Option<&TierThresholds>) -> Vec<UsageContribution> {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('s1', '/repo', 1780757089000)",
            [],
        )
        .unwrap();
        for (id, input, cache_read) in [("m-big", 200_000, 80_000), ("m-small", 1_000, 0)] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, 's1', ?2)",
                rusqlite::params![
                    id,
                    assistant_message_with_provider(
                        "gpt-5.5",
                        Some("azure"),
                        input,
                        100,
                        10,
                        cache_read,
                        0,
                        0.5,
                    )
                ],
            )
            .unwrap();
        }
        drop(conn);

        read_opencode_usage_contributions(&db_path, TimeRange::All, tiers)
            .unwrap()
            .rows
    }

    #[test]
    fn request_context_counts_the_whole_prompt() {
        // Prompt context includes fresh input and both cache buckets because OpenCode's buckets are disjoint.
        let tokens = session_usage_value(200_000, 100, 10, 80_000, 5_000);
        assert_eq!(request_context(&tokens), 285_000);
    }

    #[test]
    fn a_request_above_the_boundary_is_billed_at_its_level() {
        let tiers = tier_snapshot();
        let rows = straddling_tier_rows(Some(&tiers));
        assert_eq!(rows.len(), 2);

        let big = rows.iter().find(|row| row.tokens.input_tokens == 200_000);
        let small = rows.iter().find(|row| row.tokens.input_tokens == 1_000);
        // 200k input plus 80k cache reads clears 272k; 1k on its own does not.
        assert_eq!(big.map(|row| row.tier_level), Some(1));
        assert_eq!(small.map(|row| row.tier_level), Some(0));

        let above = big.unwrap().tokens.into_value(1);
        assert_eq!(above["above_tier"]["level_1_input_tokens"], 200_000);
        assert_eq!(above["above_tier"]["level_1_cache_read_tokens"], 80_000);
        assert_eq!(above["above_tier"]["level_1_output_tokens"], 100);
        assert_eq!(above["above_tier"]["level_1_reasoning_tokens"], 10);
        // The slice is a subset of the totals, never tokens on top of them.
        assert_eq!(above["input_tokens"], 200_000);
        assert!(
            small
                .unwrap()
                .tokens
                .into_value(0)
                .get("above_tier")
                .is_none()
        );
    }

    #[test]
    fn no_thresholds_classifies_nothing() {
        // Without pricing thresholds, every request stays at base tier 0.
        let rows = straddling_tier_rows(None);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.tier_level == 0));
    }

    #[test]
    fn legacy_session_rows_are_never_classified() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE message", []).unwrap();
        conn.execute("DROP TABLE part", []).unwrap();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write) \
             VALUES ('s1', '{\"id\":\"gpt-5.5\",\"providerID\":\"azure\"}', '/repo', 1780757088080, 0.02, 400000, 21, 26, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let tiers = tier_snapshot();
        let rows = read_opencode_usage_contributions(&db_path, TimeRange::All, Some(&tiers))
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        // Legacy session rows hold whole-conversation totals, so they stay at tier 0 regardless of size.
        assert_eq!(rows[0].tier_level, 0);
    }

    #[test]
    fn analysis_rejects_assistant_rows_with_completely_unknown_schema() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('s1', '/repo', 1780757089000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [r#"{"role":"assistant","futureUsage":{"input":10}}"#],
        )
        .unwrap();
        drop(conn);

        let result =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                .unwrap();
        assert_eq!(result.expected_records, 1);
        assert_eq!(result.parsed_records, 0);
        assert!(result.rows.is_empty());

        let error = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::Full).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("none of 1 OpenCode analysis records")
        );
    }

    #[test]
    fn analysis_candidate_count_skips_messages_without_a_session() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('s1', '/repo', 1780757089000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("model", 1, 2, 0, 0, 0, 0.5)],
        )
        .unwrap();
        // Messages whose session is missing are excluded by the `session` join and not counted as expected.
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m2', 'gone', ?1)",
            [assistant_message("model", 3, 4, 0, 0, 0, 0.5)],
        )
        .unwrap();
        drop(conn);

        let result =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                .unwrap();
        assert_eq!(result.expected_records, 1);
        assert_eq!(result.parsed_records, 1);
        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn analysis_accepts_a_database_whose_assistant_rows_are_all_orphaned() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 'gone', ?1)",
            [assistant_message("model", 1, 2, 0, 0, 0, 0.5)],
        )
        .unwrap();
        drop(conn);

        let result =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                .unwrap();
        assert_eq!(result.expected_records, 0);
        assert_eq!(result.parsed_records, 0);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::Full).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn analysis_rejects_unknown_only_and_wrong_type_token_objects() {
        for tokens in [
            json!({ "prompt": 10, "completion": 2 }),
            json!({ "input": "10" }),
            json!({ "cache": { "futureRead": 10 } }),
        ] {
            let (_dir, db_path) = make_db();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO session (id, directory, time_updated) VALUES ('s1', '/repo', 1780757089000)",
                [],
            )
            .unwrap();
            let message = json!({
                "role": "assistant",
                "modelID": "model",
                "tokens": tokens,
            })
            .to_string();
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
                [message],
            )
            .unwrap();
            drop(conn);

            let result =
                read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                    .unwrap();
            assert_eq!(result.expected_records, 1);
            assert_eq!(result.parsed_records, 0);
            assert!(result.rows.is_empty());
        }
    }

    #[test]
    fn analysis_accepts_zero_and_partial_known_token_objects() {
        for tokens in [json!({}), json!({ "input": 0 }), json!({ "cache": {} })] {
            let raw = json!({
                "role": "assistant",
                "modelID": "model",
                "tokens": tokens,
            })
            .to_string();
            let parsed = parse_message_usage(&raw).expect("known zero token shape");
            assert_eq!(parsed.usage.input_tokens, 0);
            assert_eq!(parsed.usage.output_tokens, 0);
        }
    }

    #[test]
    fn analysis_reports_known_tool_part_schema_drift() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('s1', '/repo', 1780757089000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("model", 0, 0, 0, 0, 0, 0.0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('p1', 'm1', 's1', ?1)",
            [r#"{"type":"tool","tool":"write","state":{"status":"completed","input":{"futurePath":"/repo/a","futureContent":"text"}}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('p2', 'm1', 's1', ?1)",
            [r#"{"type":"tool","tool":"write","state":{"status":"success","input":{"filePath":"/repo/a","content":"text"}}}"#],
        )
        .unwrap();
        drop(conn);

        let result =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                .unwrap();
        assert_eq!(result.expected_records, 1);
        assert_eq!(result.parsed_records, 1);
        assert_eq!(result.failed_tool_parts, 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].analysis.records[0].tool_call_counts.write, 0);
    }

    #[test]
    fn test_time_range_filters_old_sessions() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        let now_ms = chrono::Local::now().timestamp_millis();
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

    /// Test timezone that skips local midnight (`00:00` -> `01:00`), like `America/Santiago` on 2026-09-06.
    #[derive(Clone)]
    struct MidnightGapZone;

    impl MidnightGapZone {
        const BEFORE: i32 = -4 * 3600;
        const AFTER: i32 = -3 * 3600;

        fn gap_date() -> chrono::NaiveDate {
            chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap()
        }

        /// The instant the clock jumps, as a UTC datetime.
        fn transition() -> chrono::NaiveDateTime {
            Self::gap_date().and_hms_opt(4, 0, 0).unwrap()
        }

        fn offset(seconds: i32) -> chrono::FixedOffset {
            chrono::FixedOffset::east_opt(seconds).unwrap()
        }
    }

    impl chrono::TimeZone for MidnightGapZone {
        type Offset = chrono::FixedOffset;

        fn from_offset(_: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(
            &self,
            _: &chrono::NaiveDate,
        ) -> chrono::MappedLocalTime<Self::Offset> {
            chrono::MappedLocalTime::Single(Self::offset(Self::AFTER))
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::MappedLocalTime<Self::Offset> {
            let midnight = Self::gap_date().and_hms_opt(0, 0, 0).unwrap();
            let resumes = Self::gap_date().and_hms_opt(1, 0, 0).unwrap();
            if *local < midnight {
                chrono::MappedLocalTime::Single(Self::offset(Self::BEFORE))
            } else if *local < resumes {
                chrono::MappedLocalTime::None
            } else {
                chrono::MappedLocalTime::Single(Self::offset(Self::AFTER))
            }
        }

        fn offset_from_utc_date(&self, _: &chrono::NaiveDate) -> Self::Offset {
            Self::offset(Self::AFTER)
        }

        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> Self::Offset {
            if *utc < Self::transition() {
                Self::offset(Self::BEFORE)
            } else {
                Self::offset(Self::AFTER)
            }
        }
    }

    #[test]
    fn day_start_falls_forward_when_local_midnight_is_skipped() {
        // `00:00` never happens, so the day begins where the clock jumps.
        assert_eq!(
            day_start_millis(&MidnightGapZone, MidnightGapZone::gap_date()),
            MidnightGapZone::transition().and_utc().timestamp_millis()
        );
        // The next day is ordinary and still starts at its own local midnight.
        let ordinary = chrono::NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        assert_eq!(
            day_start_millis(&MidnightGapZone, ordinary),
            ordinary
                .and_hms_opt(3, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        );
    }

    #[test]
    fn message_timestamp_resolves_the_same_way_for_the_count_and_the_collectors() {
        let now_ms = chrono::Local::now().timestamp_millis();
        let stale_ms = 1000000000000i64;
        // Resolve timestamps once in SQL so malformed non-integer `$.time.*` values behave consistently across count and collection.
        for (completed, created, kept) in [
            // A JSON null is not a value, so `created` gets its turn.
            (json!(null), now_ms, true),
            (json!(now_ms as f64), stale_ms, true),
            // Non-numeric strings must fall through instead of sorting above integers in SQLite.
            (json!(now_ms.to_string()), stale_ms, false),
            // ISO-8601 strings fall through to `created` instead of casting their leading year digits.
            (json!("2026-08-20T00:00:00Z"), now_ms, true),
        ] {
            let (_dir, db_path) = make_db();
            let conn = Connection::open(&db_path).unwrap();
            // Keep `session.time_updated` stale so only the message timestamp can place the row in today's window.
            conn.execute(
                "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m1\"}', '/repo', ?1)",
                rusqlite::params![stale_ms],
            )
            .unwrap();
            let message = json!({
                "role": "assistant",
                "modelID": "m1",
                "cost": 0.01,
                "tokens": { "input": 10, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": { "created": created, "completed": completed },
            });
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
                [message.to_string()],
            )
            .unwrap();
            drop(conn);

            let expected = usize::from(kept);
            let result = read_opencode_analysis_with_diagnostics(
                &db_path,
                TimeRange::Daily,
                ParseMode::Full,
            )
            .unwrap();
            assert_eq!(result.expected_records, expected, "counted {completed}");
            assert_eq!(result.parsed_records, expected, "collected {completed}");
            assert_eq!(
                read_opencode_usage(&db_path, TimeRange::Daily)
                    .unwrap()
                    .len(),
                expected,
                "usage {completed}"
            );
        }
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
    fn test_read_analysis_orders_details_by_part_id() {
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
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('z', 'm1', 's1', ?1)",
            [r#"{"type":"tool","tool":"write","state":{"status":"completed","input":{"filePath":"/repo/z.rs","content":"z"}}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('a', 'm1', 's1', ?1)",
            [r#"{"type":"tool","tool":"write","state":{"status":"completed","input":{"filePath":"/repo/a.rs","content":"a"}}}"#],
        )
        .unwrap();
        drop(conn);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::Full).unwrap();
        let details = &rows[0].1.records[0].write_file_details;
        assert_eq!(details[0].base.file_path, "/repo/a.rs");
        assert_eq!(details[1].base.file_path, "/repo/z.rs");
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
    fn test_read_analysis_keeps_patch_insertions_as_edits() {
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
            [r#"{"type":"tool","tool":"apply_patch","state":{"status":"completed","input":{"patchText":"*** Begin Patch\n*** Update File: src/main.rs\n@@\n+inserted\n+lines\n*** End Patch"}}}"#],
        )
        .unwrap();
        drop(conn);

        let rows = read_opencode_analysis(&db_path, TimeRange::All, ParseMode::UsageOnly).unwrap();
        assert_eq!(rows.len(), 1);
        let record = &rows[0].1.records[0];
        // Insert-only update hunks target existing files and stay classified as edits.
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 2);
        assert_eq!(record.total_write_lines, 0);
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
