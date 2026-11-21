//! OpenCode session reader for the local SQLite database (`~/.local/share/opencode/opencode.db`).
//!
//! - [`read_opencode_usage`] reads assistant messages for per-model tokens and stored cost, falling back to the legacy `session` table when no `message` table exists.
//! - [`read_opencode_analysis`] additionally folds tracked `part` tool calls (`read`, `edit`, `write`, `bash`, `todowrite`, `apply_patch`) into per-message file-operation metrics.
//!
//! Output uses the shared [`CodeAnalysis`] shape with disjoint token buckets (`input`, `output`, `reasoning`, `cache_read`, `cache_write`). Assistant messages carry their own `providerID` and `modelID`, splitting mid-session model switches into separate rows.

use crate::VERSION;
use crate::constants::FastHashMap;
use crate::models::TimeRange;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, CodeAnalysisToolCalls, ExtensionType};
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{
    DatabaseAnalysisRow, DatabaseUsageRead, UsageContribution, UsageTokenContribution,
};
use crate::session::sqlite::{LocalDateCache, day_start_millis, with_readonly_connection};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Result, anyhow};
use rusqlite::{Connection, types::ValueRef};
use serde::{Deserialize, Deserializer, de};
#[cfg(test)]
use serde_json::json;
use serde_json::{Value, value::RawValue};
use std::borrow::Cow;
#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
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
        transaction.commit()?;
        Ok(OpenCodeAnalysisRead {
            rows: collected.rows,
            expected_records: collected.expected_records,
            parsed_records: collected.parsed_records,
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

/// Per-record accumulator used while folding tool parts, allocating [`SessionParseState`] lazily only when a tracked tool part arrives.
struct AnalysisAccum {
    model_id: String,
    date: String,
    folder_path: String,
    session_id: String,
    last_ts: i64,
    usage: Value,
    state: Option<SessionParseState>,
}

impl AnalysisAccum {
    fn state_mut(&mut self, mode: ParseMode) -> &mut SessionParseState {
        self.state.get_or_insert_with(|| {
            let mut state = SessionParseState::with_mode(mode);
            state.folder_path = std::mem::take(&mut self.folder_path);
            state.task_id = self.session_id.clone();
            state.last_ts = self.last_ts;
            state
        })
    }

    fn into_record(self, usage_map: FastHashMap<String, Value>) -> (String, CodeAnalysisRecord) {
        let session_id = self.session_id;
        let record = if let Some(state) = self.state {
            state.into_record(usage_map)
        } else {
            CodeAnalysisRecord {
                total_unique_files: 0,
                total_write_lines: 0,
                total_read_lines: 0,
                total_edit_lines: 0,
                total_write_characters: 0,
                total_read_characters: 0,
                total_edit_characters: 0,
                write_file_details: Vec::new(),
                read_file_details: Vec::new(),
                edit_file_details: Vec::new(),
                run_command_details: Vec::new(),
                tool_call_counts: CodeAnalysisToolCalls::default(),
                conversation_usage: usage_map,
                advisor_usage: FastHashMap::default(),
                task_id: session_id.clone(),
                timestamp: self.last_ts,
                folder_path: self.folder_path,
                git_remote_url: String::new(),
            }
        };
        (session_id, record)
    }
}

struct CollectedAnalysis {
    rows: Vec<DatabaseAnalysisRow>,
    expected_records: usize,
    parsed_records: usize,
    failed_tool_parts: usize,
}

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

    let mut date_cache = LocalDateCache::default();
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
        let Some(date) = date_cache.format_ms(time_updated) else {
            continue;
        };

        // Legacy `session` rows hold conversation totals rather than single-request context, so tier level is always 0.
        let mut contribution = UsageContribution::single_model(
            session_id,
            date.to_owned(),
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
    let mut stmt = conn.prepare(
        "SELECT message.data, session.time_updated, message.session_id, session.directory \
         FROM message \
         JOIN session ON session.id = message.session_id \
         WHERE json_extract(message.data, '$.role') = 'assistant'",
    )?;
    let mut rows = stmt.query([])?;

    let mut classifier = tiers.map(TierClassifier::new);
    let mut date_cache = LocalDateCache::default();
    let mut out = Vec::new();
    let mut expected_records = 0usize;
    while let Some(row) = rows.next()? {
        let data_text = row.get_ref(0)?.as_str()?;
        let session_time_updated = row.get::<_, i64>(1)?;
        let parsed = serde_json::from_str::<BorrowedMessageData<'_>>(data_text).ok();
        let message_ts = parsed.as_ref().map_or(session_time_updated, |data| {
            resolve_message_timestamp(data.time, session_time_updated)
        });
        if let Some(cutoff) = cutoff_ms
            && message_ts < cutoff
        {
            continue;
        }
        expected_records += 1;
        let Some(data) = parsed else {
            continue;
        };
        let Some(message) = parse_borrowed_message_usage(&data) else {
            continue;
        };
        let Some(date) = date_cache.format_ms(message_ts) else {
            continue;
        };
        let session_id = row.get::<_, String>(2)?;
        let directory = row.get::<_, Option<String>>(3)?;

        let tier_level = classifier.as_mut().map_or(0, |classifier| {
            classifier.level(&message.model_id, request_context(&message.usage))
        });
        let mut contribution = UsageContribution::single_model(
            session_id,
            date.to_owned(),
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
    let mut date_cache = LocalDateCache::default();

    // 1. Load session metadata and seed one parse state per session.
    let mut sessions: FastHashMap<String, AnalysisAccum> = FastHashMap::default();
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
            let Some(date) = date_cache.format_ms(ts) else {
                continue;
            };

            // Analysis is never priced, so tier level is always 0.
            let usage = session_usage_value(input, output, reasoning, cache_read, cache_write)
                .into_value(0);

            sessions.insert(
                id.clone(),
                AnalysisAccum {
                    model_id,
                    date: date.to_owned(),
                    folder_path: directory,
                    session_id: id,
                    last_ts: ts,
                    usage,
                    state: None,
                },
            );
        }
    }

    // 2. Fold tool parts into their owning session's parse state without SQL-side `json_extract` re-parsing.
    let mut failed_tool_parts = 0usize;
    if !sessions.is_empty() {
        let sql = match mode {
            ParseMode::Full => "SELECT session_id, data FROM part ORDER BY id",
            ParseMode::UsageOnly => "SELECT session_id, data FROM part",
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query([])?;

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
            fold_tool_part_str(accum, mode, data_text, &mut failed_tool_parts);
        }
    }

    // 3. Convert each session into a CodeAnalysis.
    let parsed_records = sessions.len();
    let mut out = Vec::with_capacity(parsed_records);
    for (id, mut accum) in sessions {
        let mut usage_map = FastHashMap::default();
        let model_id = std::mem::take(&mut accum.model_id);
        let usage = std::mem::take(&mut accum.usage);
        let date = std::mem::take(&mut accum.date);
        usage_map.insert(model_id, usage);
        let (session_id, record) = accum.into_record(usage_map);
        out.push(DatabaseAnalysisRow {
            session_id,
            source_id: id,
            date,
            analysis: wrap_record(record, &user, &machine),
        });
    }

    Ok(CollectedAnalysis {
        rows: out,
        expected_records,
        parsed_records,
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
    let mut date_cache = LocalDateCache::default();

    let mut messages: FastHashMap<String, AnalysisAccum> = FastHashMap::default();
    let mut expected_records = 0usize;
    {
        let mut stmt = conn.prepare(
            "SELECT message.id, message.session_id, message.data, session.directory, session.time_updated \
             FROM message \
             JOIN session ON session.id = message.session_id \
             WHERE json_extract(message.data, '$.role') = 'assistant'",
        )?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let data_text = row.get_ref(2)?.as_str()?;
            let session_time_updated = row.get::<_, i64>(4)?;
            let parsed = serde_json::from_str::<BorrowedMessageData<'_>>(data_text).ok();
            let message_ts = parsed.as_ref().map_or(session_time_updated, |data| {
                resolve_message_timestamp(data.time, session_time_updated)
            });
            if let Some(cutoff) = cutoff_ms
                && message_ts < cutoff
            {
                continue;
            }
            expected_records += 1;
            let Some(data) = parsed else {
                continue;
            };
            let Some(message) = parse_borrowed_message_usage(&data) else {
                continue;
            };
            let Some(date) = date_cache.format_ms(message_ts) else {
                continue;
            };
            let message_id = row.get::<_, String>(0)?;
            let session_id = row.get::<_, String>(1)?;
            let directory = row.get::<_, String>(3)?;

            messages.insert(
                message_id,
                AnalysisAccum {
                    model_id: message.model_id,
                    date: date.to_owned(),
                    folder_path: directory,
                    session_id,
                    last_ts: message_ts,
                    // Analysis is never priced, so tier level is always 0.
                    usage: message.usage.into_value(0),
                    state: None,
                },
            );
        }
    }

    let mut failed_tool_parts = 0usize;
    if !messages.is_empty() {
        // Scalar counters and unique-file sets are commutative, so `UsageOnly` skips `ORDER BY id` sorting while keeping per-message granularity identical to `Full`.
        let sql = match mode {
            ParseMode::Full => "SELECT message_id, data FROM part ORDER BY id",
            ParseMode::UsageOnly => "SELECT message_id, data FROM part",
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query([])?;

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
            fold_tool_part_str(accum, mode, data_text, &mut failed_tool_parts);
        }
    }

    let parsed_records = messages.len();
    let mut out = Vec::with_capacity(parsed_records);
    for (id, mut accum) in messages {
        let mut usage_map = FastHashMap::default();
        let model_id = std::mem::take(&mut accum.model_id);
        let usage = std::mem::take(&mut accum.usage);
        let date = std::mem::take(&mut accum.date);
        usage_map.insert(model_id, usage);
        let (session_id, record) = accum.into_record(usage_map);
        out.push(DatabaseAnalysisRow {
            session_id,
            source_id: id,
            date,
            analysis: wrap_record(record, &user, &machine),
        });
    }

    Ok(CollectedAnalysis {
        rows: out,
        expected_records,
        parsed_records,
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

/// Accepts any JSON value, keeping strings (zero-copy when unescaped) and skipping other types cleanly.
#[derive(Default)]
struct MaybeStr<'a>(Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for MaybeStr<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct StrVisitor;

        impl<'de> de::Visitor<'de> for StrVisitor {
            type Value = MaybeStr<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }

            fn visit_borrowed_str<E: de::Error>(
                self,
                v: &'de str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(Some(Cow::Borrowed(v))))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(Some(Cow::Owned(v.to_owned()))))
            }

            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(Some(Cow::Owned(v))))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeStr(None))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(MaybeStr(None))
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while map
                    .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(MaybeStr(None))
            }
        }

        deserializer.deserialize_any(StrVisitor)
    }
}

/// Accepts any JSON value, keeping integers and skipping other types cleanly.
#[derive(Default)]
struct MaybeI64(Option<i64>);

impl<'de> Deserialize<'de> for MaybeI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct I64Visitor;

        impl<'de> de::Visitor<'de> for I64Visitor {
            type Value = MaybeI64;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(Some(v)))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(i64::try_from(v).ok()))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(None))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(None))
            }

            fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(None))
            }

            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(None))
            }

            fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(None))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(MaybeI64(None))
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while map
                    .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(MaybeI64(None))
            }
        }

        deserializer.deserialize_any(I64Visitor)
    }
}

/// Accepts any JSON value, deserializing objects into `T` in the same single pass and skipping non-objects cleanly.
struct MaybeObject<T>(Option<T>);

impl<T> Default for MaybeObject<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for MaybeObject<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ObjVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for ObjVisitor<T> {
            type Value = MaybeObject<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(|value| MaybeObject(Some(value)))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(MaybeObject(None))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(MaybeObject(None))
            }
        }

        deserializer.deserialize_any(ObjVisitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct BorrowedToolPart<'a> {
    #[serde(default, borrow, rename = "type")]
    kind: MaybeStr<'a>,
    #[serde(default, borrow)]
    tool: MaybeStr<'a>,
    #[serde(default, borrow)]
    state: MaybeObject<BorrowedToolState<'a>>,
}

#[derive(Deserialize)]
struct BorrowedToolState<'a> {
    #[serde(default, borrow)]
    status: MaybeStr<'a>,
    #[serde(default, borrow)]
    input: MaybeObject<BorrowedToolInput<'a>>,
    // Keep `output` raw so `bash`/`edit`/`write` outputs are never unescaped or allocated; only `"read"` decodes it.
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    output: Option<&'a RawValue>,
    #[serde(default)]
    time: MaybeObject<BorrowedToolTime>,
}

#[derive(Deserialize)]
struct BorrowedToolTime {
    #[serde(default)]
    start: MaybeI64,
}

#[derive(Deserialize)]
struct BorrowedToolInput<'a> {
    #[serde(default, borrow, rename = "filePath")]
    file_path: MaybeStr<'a>,
    #[serde(default, borrow, rename = "oldString")]
    old_string: MaybeStr<'a>,
    #[serde(default, borrow, rename = "newString")]
    new_string: MaybeStr<'a>,
    #[serde(default, borrow)]
    content: MaybeStr<'a>,
    #[serde(default, borrow)]
    command: MaybeStr<'a>,
    #[serde(default, borrow)]
    description: MaybeStr<'a>,
    #[serde(default, borrow, rename = "patchText")]
    patch_text: MaybeStr<'a>,
}

fn decode_raw_str(raw: Option<&RawValue>) -> Option<Cow<'_, str>> {
    serde_json::from_str::<Cow<'_, str>>(raw?.get()).ok()
}

fn fold_tool_part_str(
    accum: &mut AnalysisAccum,
    mode: ParseMode,
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
    if part.kind.0.as_deref() != Some("tool") {
        return;
    }
    if apply_tool_part(accum, mode, &part) == ToolPartOutcome::Unsupported {
        *failed_tool_parts += 1;
    }
}

/// Folds one tracked `tool` part (`read`, `edit`, `write`, `bash`, `todowrite`, `apply_patch`) into `accum`, ignoring auxiliary tools.
fn apply_tool_part(
    accum: &mut AnalysisAccum,
    mode: ParseMode,
    data: &BorrowedToolPart<'_>,
) -> ToolPartOutcome {
    let tool = data.tool.0.as_deref().unwrap_or("");
    if !matches!(
        tool,
        "read" | "edit" | "write" | "bash" | "todowrite" | "apply_patch"
    ) {
        return ToolPartOutcome::Irrelevant;
    }

    let Some(st) = data.state.0.as_ref() else {
        return ToolPartOutcome::Unsupported;
    };
    let Some(status) = st.status.0.as_deref() else {
        return ToolPartOutcome::Unsupported;
    };
    match status {
        "completed" => {}
        "pending" | "running" | "error" => return ToolPartOutcome::Irrelevant,
        _ => return ToolPartOutcome::Unsupported,
    }

    let input = st.input.0.as_ref();
    let ts = st
        .time
        .0
        .as_ref()
        .and_then(|t| t.start.0)
        .unwrap_or(accum.last_ts);

    match tool {
        "read" => {
            let (Some(path), Some(output)) = (
                input
                    .and_then(|i| i.file_path.0.as_deref())
                    .filter(|path| !path.is_empty()),
                decode_raw_str(st.output),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            record_read_output(accum.state_mut(mode), path, &output, ts);
        }
        "edit" => {
            let (Some(path), Some(old), Some(new)) = (
                input
                    .and_then(|i| i.file_path.0.as_deref())
                    .filter(|path| !path.is_empty()),
                input.and_then(|i| i.old_string.0.as_deref()),
                input.and_then(|i| i.new_string.0.as_deref()),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            accum.state_mut(mode).add_edit_detail(path, old, new, ts);
        }
        "write" => {
            let (Some(path), Some(content)) = (
                input
                    .and_then(|i| i.file_path.0.as_deref())
                    .filter(|path| !path.is_empty()),
                input.and_then(|i| i.content.0.as_deref()),
            ) else {
                return ToolPartOutcome::Unsupported;
            };
            accum.state_mut(mode).add_write_detail(path, content, ts);
        }
        "bash" => {
            let Some(command) = input
                .and_then(|i| i.command.0.as_deref())
                .filter(|command| !command.trim().is_empty())
            else {
                return ToolPartOutcome::Unsupported;
            };
            let desc = input.and_then(|i| i.description.0.as_deref()).unwrap_or("");
            accum.state_mut(mode).add_run_command(command, desc, ts);
        }
        "todowrite" => {
            accum.state_mut(mode).tool_counts.todo_write += 1;
        }
        "apply_patch" => {
            let Some(patch_text) = input.and_then(|i| i.patch_text.0.as_deref()) else {
                return ToolPartOutcome::Unsupported;
            };
            let patches = parse_apply_patch_text(patch_text);
            if !patches.iter().any(|patch| !patch.file_path.is_empty()) {
                return ToolPartOutcome::Unsupported;
            }
            apply_parsed_patches(accum.state_mut(mode), patches, ts);
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
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    time: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedMessageTime<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    completed: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    created: Option<&'a RawValue>,
}

/// Parses a JSON integer or finite float as `i64`, rejecting strings, nulls, and booleans (matching SQL `typeof IN ('integer', 'real')`).
fn parse_json_int_or_real(raw: Option<&RawValue>) -> Option<i64> {
    let text = raw?.get();
    if let Ok(value) = text.parse::<i64>() {
        return Some(value);
    }
    let first = *text.as_bytes().first()?;
    if first != b'-' && !first.is_ascii_digit() {
        return None;
    }
    serde_json::from_str::<f64>(text)
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value as i64)
}

/// Resolves an assistant message's epoch-ms timestamp (`time.completed`, `time.created`, or `session_time_updated`).
fn resolve_message_timestamp(time_raw: Option<&RawValue>, session_time_updated: i64) -> i64 {
    let Some(raw) = time_raw.filter(|raw| raw.get().starts_with('{')) else {
        return session_time_updated;
    };
    let Ok(time) = serde_json::from_str::<BorrowedMessageTime<'_>>(raw.get()) else {
        return session_time_updated;
    };
    parse_json_int_or_real(time.completed)
        .or_else(|| parse_json_int_or_real(time.created))
        .unwrap_or(session_time_updated)
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
#[cfg(test)]
fn parse_message_usage(raw: &str) -> Option<MessageUsage> {
    let data = serde_json::from_str::<BorrowedMessageData<'_>>(raw).ok()?;
    parse_borrowed_message_usage(&data)
}

fn parse_borrowed_message_usage(data: &BorrowedMessageData<'_>) -> Option<MessageUsage> {
    if decode_raw_str(data.role).as_deref() != Some("assistant") {
        return None;
    }

    let model_id = message_model_id(data)?;
    let usage = serde_json::from_str::<OpenCodeTokens>(data.tokens?.get())
        .ok()?
        .0;
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

/// Zero-allocation deserializer for OpenCode's `message.data.tokens` object with strict key/type validation.
struct OpenCodeTokens(UsageTokenContribution);

struct OpenCodeCacheTokens {
    read: i64,
    write: i64,
}

impl<'de> Deserialize<'de> for OpenCodeCacheTokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CacheVisitor;

        impl<'de> de::Visitor<'de> for CacheVisitor {
            type Value = OpenCodeCacheTokens;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an OpenCode cache token object")
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut total_keys = 0usize;
                let mut has_known = false;
                let mut read = 0i64;
                let mut write = 0i64;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    total_keys += 1;
                    match key.as_ref() {
                        "read" => {
                            has_known = true;
                            read = map.next_value::<i64>()?;
                        }
                        "write" => {
                            has_known = true;
                            write = map.next_value::<i64>()?;
                        }
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                if total_keys > 0 && !has_known {
                    return Err(de::Error::custom("unknown cache token keys"));
                }
                Ok(OpenCodeCacheTokens { read, write })
            }
        }

        deserializer.deserialize_map(CacheVisitor)
    }
}

impl<'de> Deserialize<'de> for OpenCodeTokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct TokensVisitor;

        impl<'de> de::Visitor<'de> for TokensVisitor {
            type Value = OpenCodeTokens;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an OpenCode tokens object")
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut total_keys = 0usize;
                let mut has_known = false;
                let mut input = 0i64;
                let mut output = 0i64;
                let mut reasoning = 0i64;
                let mut cache_read = 0i64;
                let mut cache_write = 0i64;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    total_keys += 1;
                    match key.as_ref() {
                        "input" => {
                            has_known = true;
                            input = map.next_value::<i64>()?;
                        }
                        "output" => {
                            has_known = true;
                            output = map.next_value::<i64>()?;
                        }
                        "reasoning" => {
                            has_known = true;
                            reasoning = map.next_value::<i64>()?;
                        }
                        "cache" => {
                            has_known = true;
                            let cache = map.next_value::<OpenCodeCacheTokens>()?;
                            cache_read = cache.read;
                            cache_write = cache.write;
                        }
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                if total_keys > 0 && !has_known {
                    return Err(de::Error::custom("unknown token keys"));
                }
                Ok(OpenCodeTokens(session_usage_value(
                    input,
                    output,
                    reasoning,
                    cache_read,
                    cache_write,
                )))
            }
        }

        deserializer.deserialize_map(TokensVisitor)
    }
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
    if model
        .strip_prefix(provider)
        .is_some_and(|rest| rest.starts_with('/'))
    {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

/// Returns the inclusive local-date cutoff as an epoch-millis lower bound, or `None` only for [`TimeRange::All`].
fn cutoff_millis(time_range: TimeRange) -> Option<i64> {
    time_range
        .cutoff_date()
        .map(|date| day_start_millis(&chrono::Local, date))
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

/// Records an OpenCode `read` output, counting lines and characters in a single pass under [`ParseMode::UsageOnly`] without allocating the stripped body.
fn record_read_output(state: &mut SessionParseState, path: &str, output: &str, ts: i64) {
    if matches!(state.mode, ParseMode::Full) {
        let content = extract_read_content(output);
        if !content.is_empty() {
            state.add_read_detail(path, &content, ts);
        } else {
            // Directory listings and empty reads still count as read invocations.
            state.tool_counts.read += 1;
        }
        return;
    }

    let mut stats = ReadContentStats::default();
    if let Some(start) = output.find("<content>") {
        let after = &output[start + "<content>".len()..];
        let inner = match after.find("</content>") {
            Some(end) => &after[..end],
            None => after,
        };
        let inner = inner.strip_prefix('\n').unwrap_or(inner);
        for line in inner.split('\n') {
            stats.push(strip_line_number_prefix(
                line.strip_suffix('\r').unwrap_or(line),
            ));
        }
    } else {
        let mut lines = output.split('\n');
        let Some(_path) = lines.next() else {
            state.tool_counts.read += 1;
            return;
        };
        let Some(kind) = lines.next() else {
            state.tool_counts.read += 1;
            return;
        };
        if kind.trim_end_matches('\r') != "file" {
            state.tool_counts.read += 1;
            return;
        }
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
                stats.push(strip_line_number_prefix(line));
                saw_content = true;
            } else if saw_content {
                break;
            }
        }
        if !saw_separator {
            state.tool_counts.read += 1;
            return;
        }
    }
    stats.apply(state, path);
}

#[derive(Default)]
struct ReadContentStats {
    total_lines: usize,
    last_line_empty: bool,
    non_empty_lines: usize,
    chars_so_far: usize,
    chars_up_to_last_non_empty: usize,
}

impl ReadContentStats {
    fn push(&mut self, line: &str) {
        if self.total_lines > 0 {
            self.chars_so_far += 1;
        }
        self.chars_so_far += bytecount::num_chars(line.as_bytes());
        self.total_lines += 1;
        self.last_line_empty = line.is_empty();
        if !self.last_line_empty {
            self.non_empty_lines = self.total_lines;
            self.chars_up_to_last_non_empty = self.chars_so_far;
        }
    }

    fn apply(self, state: &mut SessionParseState, path: &str) {
        // Matches `strip_numbered_content_lines_from_iter`, which pops at most one trailing empty line before joining.
        let effective_len = if self.last_line_empty {
            self.total_lines.saturating_sub(1)
        } else {
            self.total_lines
        };
        if self.non_empty_lines > 0 {
            let resolved = state.normalize_path(path);
            if resolved.is_empty() {
                return;
            }
            state.unique_files.insert(resolved);
            state.total_read_lines += self.non_empty_lines;
            state.total_read_characters += self.chars_up_to_last_non_empty;
            state.tool_counts.read += 1;
        } else if effective_len <= 1 {
            // Directory listings and empty reads still count as read invocations.
            state.tool_counts.read += 1;
        }
    }
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
            content.push(strip_line_number_prefix(line));
            saw_content = true;
        } else if saw_content {
            break;
        }
    }

    if !saw_separator {
        return String::new();
    }
    if content.last().is_some_and(|line| line.is_empty()) {
        content.pop();
    }
    content.join("\n")
}

fn strip_numbered_content_lines(inner: &str) -> String {
    let mut lines: Vec<&str> = inner
        .split('\n')
        .map(|line| strip_line_number_prefix(line.strip_suffix('\r').unwrap_or(line)))
        .collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
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

    #[test]
    fn record_read_output_usage_only_matches_full_mode() {
        let samples = [
            "<path>/a/b.py</path>\n<type>file</type>\n<content>\n1: import os\n2: \n3: print(1)\n</content>",
            "<content>\n1: 你好世界\r\n2: 🚀🦀\r\n3: \r\n</content>",
            "<content>\n1: \n</content>",
            "<content>\n1: \n2: \n</content>",
            "<content>\n</content>",
            "/a/b.py\nfile\n\n1: 你好\n2: \n3: 世界\n\n(End of file - total 3 lines)",
            "/a/b.py\r\nfile\r\n\r\n1: a\r\n2: b\r\n",
            "/a\ndirectory\n\nx.py",
            "<path>/a</path>\n<type>directory</type>\n<entries>\nx.py\n</entries>",
            "",
        ];
        for sample in samples {
            let mut full = SessionParseState::with_mode(ParseMode::Full);
            let mut usage_only = SessionParseState::with_mode(ParseMode::UsageOnly);
            record_read_output(&mut full, "/repo/file.rs", sample, 1);
            record_read_output(&mut usage_only, "/repo/file.rs", sample, 1);
            assert_eq!(
                usage_only.tool_counts.read, full.tool_counts.read,
                "read count mismatch on {sample:?}"
            );
            assert_eq!(
                usage_only.total_read_lines, full.total_read_lines,
                "line count mismatch on {sample:?}"
            );
            assert_eq!(
                usage_only.total_read_characters, full.total_read_characters,
                "char count mismatch on {sample:?}"
            );
            assert_eq!(
                usage_only.unique_files, full.unique_files,
                "unique_files mismatch on {sample:?}"
            );
        }
    }

    #[test]
    fn usage_only_matches_full_per_message_scalars_and_diagnostics() {
        let (_dir, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, model, directory, time_updated) VALUES ('s1', '{\"id\":\"m1\"}', '/repo', 1780757089000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m1', 's1', ?1)",
            [assistant_message("m1", 10, 2, 0, 0, 0, 0.01)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m2', 's1', ?1)",
            [assistant_message("m1", 20, 4, 0, 0, 0, 0.02)],
        )
        .unwrap();
        // Zero-tool-part message exercises the lazy `SessionParseState` fallback path.
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m3', 's1', ?1)",
            [assistant_message("m1", 5, 1, 0, 0, 0, 0.005)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('m_bad', 's1', ?1)",
            [r#"{"role":"assistant","modelID":"m1","tokens":{"unknown":1}}"#],
        )
        .unwrap();
        let parts = [
            (
                "p1",
                "m1",
                r#"{"type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"/repo/a.rs"},"output":"<content>\n1: one\n2: two\n</content>"}}"#,
            ),
            (
                "p2",
                "m2",
                r#"{"type":"tool","tool":"write","state":{"status":"completed","input":{"filePath":"/repo/b.rs","content":"x\ny\nz"}}}"#,
            ),
            (
                "p3",
                "m2",
                r#"{"type":"tool","tool":"grep","state":{"status":"completed","input":{"pattern":"x"}}}"#,
            ),
            (
                "p4",
                "m2",
                r#"{"type":"tool","tool":"write","state":{"status":"unknown_status","input":{"filePath":"/repo/c.rs","content":"z"}}}"#,
            ),
        ];
        for (pid, mid, data) in parts {
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, data) VALUES (?1, ?2, 's1', ?3)",
                rusqlite::params![pid, mid, data],
            )
            .unwrap();
        }
        drop(conn);

        let mut usage_only =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::UsageOnly)
                .unwrap();
        let mut full =
            read_opencode_analysis_with_diagnostics(&db_path, TimeRange::All, ParseMode::Full)
                .unwrap();

        assert_eq!(usage_only.expected_records, 4);
        assert_eq!(usage_only.parsed_records, 3);
        assert_eq!(usage_only.failed_tool_parts, 1);
        assert_eq!(usage_only.expected_records, full.expected_records);
        assert_eq!(usage_only.parsed_records, full.parsed_records);
        assert_eq!(usage_only.failed_tool_parts, full.failed_tool_parts);
        assert_eq!(usage_only.rows.len(), full.rows.len());

        usage_only
            .rows
            .sort_by(|a, b| a.source_id.cmp(&b.source_id));
        full.rows.sort_by(|a, b| a.source_id.cmp(&b.source_id));
        for (u, f) in usage_only.rows.iter().zip(full.rows.iter()) {
            assert_eq!(u.source_id, f.source_id);
            assert_eq!(u.session_id, f.session_id);
            assert_eq!(u.date, f.date);
            let ur = &u.analysis.records[0];
            let fr = &f.analysis.records[0];
            assert_eq!(ur.task_id, fr.task_id);
            assert_eq!(ur.folder_path, fr.folder_path);
            assert_eq!(ur.timestamp, fr.timestamp);
            assert_eq!(ur.total_unique_files, fr.total_unique_files);
            assert_eq!(ur.total_read_lines, fr.total_read_lines);
            assert_eq!(ur.total_write_lines, fr.total_write_lines);
            assert_eq!(ur.total_edit_lines, fr.total_edit_lines);
            assert_eq!(ur.total_read_characters, fr.total_read_characters);
            assert_eq!(ur.total_write_characters, fr.total_write_characters);
            assert_eq!(ur.total_edit_characters, fr.total_edit_characters);
            assert_eq!(ur.tool_call_counts.read, fr.tool_call_counts.read);
            assert_eq!(ur.tool_call_counts.write, fr.tool_call_counts.write);
            assert_eq!(ur.tool_call_counts.edit, fr.tool_call_counts.edit);
            assert_eq!(ur.tool_call_counts.bash, fr.tool_call_counts.bash);
            assert_eq!(
                ur.tool_call_counts.todo_write,
                fr.tool_call_counts.todo_write
            );
            assert_eq!(ur.conversation_usage, fr.conversation_usage);
            assert!(ur.read_file_details.is_empty());
            assert!(ur.write_file_details.is_empty());
            assert!(ur.edit_file_details.is_empty());
            assert!(ur.run_command_details.is_empty());
        }
    }

    #[test]
    fn provider_qualified_model_id_avoids_duplicate_prefix() {
        assert_eq!(
            provider_qualified_model_id(Some("azure"), "gpt-4.1"),
            "azure/gpt-4.1"
        );
        assert_eq!(
            provider_qualified_model_id(Some("azure"), "azure/gpt-4.1"),
            "azure/gpt-4.1"
        );
        assert_eq!(
            provider_qualified_model_id(Some("azure"), "azure-extra/gpt-4.1"),
            "azure/azure-extra/gpt-4.1"
        );
        assert_eq!(provider_qualified_model_id(None, "gpt-4.1"), "gpt-4.1");
    }
}
