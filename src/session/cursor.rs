//! Cursor session reader (local SQLite blob stores).
//!
//! Cursor keeps session data in two places under `~/.cursor`:
//!
//! - `ai-tracking/ai-code-tracking.db` — one row per AI-authored code line,
//!   carrying the `model` that wrote it. Used to attribute each conversation to
//!   a model for the `analysis` view (`conversationId -> model`).
//! - `chats/<projectHash>/<conversationId>/store.db` — a content-addressed blob
//!   store holding the whole conversation. Assistant turns live in binary
//!   protobuf DAG nodes (`field 4` = the message JSON, `field 26` = timestamp,
//!   `field 5` = the running context-window gauge); tool results live in
//!   standalone JSON blobs. Parsed for `analysis` tool-call metrics.
//!
//! Cursor does **not** persist real billing tokens locally (only the context
//! gauge), so the `usage` view is a deliberately-rough **local estimate** from
//! that gauge (there is no dashboard-API path here; see `examples/quota.md` for
//! the raw endpoint if it is ever reintroduced), keeping Cursor consistent with
//! the other providers whose `usage` is likewise computed from local session
//! data.
//!
//! Both entry points return the same `(local YYYY-MM-DD, CodeAnalysis[, cost])`
//! shape the OpenCode reader produces, so the `usage` / `analysis` aggregators
//! fold Cursor in exactly like the other providers.

use crate::VERSION;
use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, ExtensionType};
use crate::session::diagnostics::{
    DatabaseAnalysisRow, DatabaseUsageRead, UsageContribution, UsageTokenContribution,
};
use crate::session::sqlite::{
    DatabaseFingerprint, optional_database_fingerprint, with_readonly_connection,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_current_user, get_machine_id};
use anyhow::{Result, anyhow};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ===========================================================================
// Public entry points
// ===========================================================================

/// Reads per-model token usage for Cursor.
///
/// Cursor does not persist real billing tokens locally (only a context gauge),
/// so this is a deliberately-rough estimate from the chat stores: the context
/// gauge is counted as cache-read tokens. The caller applies the shared
/// LiteLLM pricing map so a TUI refresh does not rebuild it per reader call.
///
/// Each returned tuple is `(local YYYY-MM-DD, CodeAnalysis, 0.0)` with the
/// analysis carrying one model's `conversation_usage`, matching the shape of
/// [`crate::session::read_opencode_usage`].
///
/// # Errors
///
/// Returns an error only if reading the local chat stores fails.
pub fn read_cursor_usage(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let result = read_cursor_usage_with_diagnostics(chats_dir, tracking_db, time_range);
    for failure in &result.failures {
        log::warn!(
            "failed to read Cursor usage store {}: {}",
            failure.path.display(),
            failure.error
        );
    }
    if result.candidates > 0 && result.parsed == 0 {
        return Err(anyhow!(
            "failed to read all {} Cursor usage stores",
            result.candidates
        ));
    }
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    Ok(result
        .rows
        .into_iter()
        .map(|row| row.into_public_row(ExtensionType::Cursor, &user, &machine))
        .collect())
}

/// Per-store Cursor usage result used by diagnostics-aware collection.
pub(crate) struct CursorUsageRead {
    /// Successfully aggregated date/model rows.
    pub rows: Vec<UsageContribution>,
    /// Number of discovered stores, or one traversal candidate on failure.
    pub candidates: usize,
    /// Stores decoded successfully, including valid empty stores.
    pub parsed: usize,
    /// Store-level open or decode failures.
    pub failures: Vec<CursorAnalysisFailure>,
}

/// Reads Cursor usage while preserving partial store failures.
pub(crate) fn read_cursor_usage_with_diagnostics(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
) -> CursorUsageRead {
    let read = approximation_events(chats_dir, tracking_db);
    CursorUsageRead {
        rows: aggregate_events(&read.events, time_range),
        candidates: read.candidates,
        parsed: read.parsed,
        failures: read.failures,
    }
}

/// Reads per-model file-operation metrics for Cursor from the chat stores.
///
/// Walks every `chats/*/*/store.db`, extracts each assistant turn's tool calls
/// (`Read` / `Write` / `StrReplace`→edit / `Shell`→bash / `TodoWrite`), and
/// attributes them to the conversation's model (from `ai-code-tracking.db`,
/// falling back to the store's `lastUsedModel`). Records are bucketed by the
/// assistant turn's local date and filtered by `time_range`.
///
/// # Errors
///
/// A bad store is logged and skipped when any other store succeeds. Returns an
/// error when stores exist but none can be read, allowing batch diagnostics to
/// distinguish total parser failure from an empty Cursor history.
pub fn read_cursor_analysis(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let result = read_cursor_analysis_with_diagnostics(chats_dir, tracking_db, time_range, mode);
    for failure in &result.failures {
        log::warn!(
            "failed to read Cursor store {}: {}",
            failure.path.display(),
            failure.error
        );
    }
    if result.candidates > 0 && result.parsed == 0 {
        return Err(anyhow!(
            "failed to read all {} Cursor chat stores",
            result.candidates
        ));
    }
    Ok(result
        .rows
        .into_iter()
        .map(|row| (row.date, row.analysis))
        .collect())
}

/// One Cursor store that could not be decoded.
pub(crate) struct CursorAnalysisFailure {
    /// Store path used for diagnostics.
    pub path: PathBuf,
    /// SQLite or schema error without any chat payload.
    pub error: String,
}

/// Accessible Cursor stores plus traversal failures discovered beside them.
pub(crate) struct CursorStoreDiscovery {
    pub stores: Vec<PathBuf>,
    pub failures: Vec<CursorAnalysisFailure>,
}

/// Per-store Cursor analysis result used by the batch collector.
pub(crate) struct CursorAnalysisRead {
    /// Successfully decoded date/session rows.
    pub rows: Vec<DatabaseAnalysisRow>,
    /// Number of discovered `store.db` files.
    pub candidates: usize,
    /// Number of stores decoded successfully, including valid empty stores.
    pub parsed: usize,
    /// Store-level failures retained for noninteractive diagnostics.
    pub failures: Vec<CursorAnalysisFailure>,
}

pub(crate) struct CursorStoreAnalysis {
    pub(crate) rows: Vec<(String, CodeAnalysis)>,
    pub(crate) normalized_messages: usize,
    pub(crate) failed_payloads: usize,
}

/// Reads Cursor stores while retaining one diagnostic per store.
pub(crate) fn read_cursor_analysis_with_diagnostics(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> CursorAnalysisRead {
    let (conv_models, tracking_failure) = match load_conversation_models(tracking_db) {
        Ok(models) => (models, None),
        Err(error) => (
            FastHashMap::default(),
            Some(CursorAnalysisFailure {
                path: tracking_db.to_path_buf(),
                error: error.to_string(),
            }),
        ),
    };
    let user = get_current_user();
    let machine = get_machine_id().to_string();

    let discovery = discover_cursor_store_dbs(chats_dir);
    let candidates = discovery.stores.len() + discovery.failures.len();
    let mut parsed = 0usize;
    let mut out = Vec::new();
    let mut failures = discovery.failures;
    failures.extend(tracking_failure);
    for store_db in discovery.stores {
        match read_store_analysis(&store_db, &conv_models, time_range, mode, &user, &machine) {
            Ok(store) if store.normalized_messages == 0 && store.failed_payloads > 0 => {
                failures.push(CursorAnalysisFailure {
                    path: store_db,
                    error: format!(
                        "none of {} analyzer payloads used a supported schema",
                        store.failed_payloads
                    ),
                });
            }
            Ok(store) => {
                parsed += 1;
                for (date, analysis) in store.rows {
                    out.push(DatabaseAnalysisRow {
                        source_id: store_db.to_string_lossy().into_owned(),
                        date,
                        analysis,
                    });
                }
                if store.failed_payloads > 0 {
                    failures.push(CursorAnalysisFailure {
                        path: store_db,
                        error: format!(
                            "{} analyzer payloads used an unsupported schema",
                            store.failed_payloads
                        ),
                    });
                }
            }
            Err(err) => {
                failures.push(CursorAnalysisFailure {
                    path: store_db,
                    error: err.to_string(),
                });
            }
        }
    }
    CursorAnalysisRead {
        rows: out,
        candidates,
        parsed,
        failures,
    }
}

// ===========================================================================
// usage: local estimate
// ===========================================================================

/// One usage aggregation row keyed by `(date, model)`, so any time range can
/// filter it locally. A purely in-memory intermediate — never serialized.
#[derive(Debug)]
struct UsageEvent {
    date: String,
    timestamp_ms: i64,
    model: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

struct CursorUsageEvents {
    events: Vec<UsageEvent>,
    candidates: usize,
    parsed: usize,
    failures: Vec<CursorAnalysisFailure>,
}

/// Turns usage events into compact summary contributions.
fn aggregate_events(events: &[UsageEvent], time_range: TimeRange) -> Vec<UsageContribution> {
    let cutoff = cutoff_string(time_range);
    let mut out = Vec::new();
    for e in events {
        if is_before_cutoff(&e.date, &cutoff) {
            continue;
        }
        out.push(UsageContribution::single_model(
            e.date.clone(),
            e.timestamp_ms,
            e.model.clone(),
            cursor_usage_value(e.input, e.output, e.cache_read, e.cache_write),
            e.cost,
        ));
    }
    out
}

/// Reads one Cursor store into compact usage contributions.
///
/// The caller owns discovery and the shared tracking index so an incremental
/// scan can fingerprint and refresh each store independently.
pub(crate) fn read_cursor_usage_store(
    store_db: &Path,
    conv_models: &FastHashMap<String, String>,
    time_range: TimeRange,
) -> Result<DatabaseUsageRead> {
    let conv_id = conversation_id_from_path(store_db);
    let read = read_store_context(store_db, conv_models, &conv_id)?;
    let events = read
        .turns
        .into_iter()
        .filter_map(|(timestamp_ms, cache_read)| {
            ms_to_local_date(timestamp_ms).map(|date| UsageEvent {
                date,
                timestamp_ms,
                model: read.model.clone(),
                input: 0,
                output: 0,
                cache_read,
                cache_write: 0,
                cost: 0.0,
            })
        })
        .collect::<Vec<_>>();
    Ok(DatabaseUsageRead {
        rows: aggregate_events(&events, time_range),
        expected_records: read.expected_records,
        parsed_records: read.parsed_records,
    })
}

// ===========================================================================
// usage: local estimate
// ===========================================================================

/// Builds all-time usage-estimate events from the local context gauge.
///
/// Cursor stores only the running context-window size per assistant turn, not
/// billed tokens. Each turn re-sends (and prompt-cache-reads) the accumulated
/// context, so summing the gauge across a conversation's turns approximates the
/// **cache-read** token volume — reported in the cache-read bucket both because
/// that is the honest bucket and because it is then priced at the much cheaper
/// cache rate rather than a wildly-inflated full-input rate. Input/output are
/// unknown (`0`) and the stored cost is `0` (models Cursor prices itself, e.g.
/// `composer-*`, have no LiteLLM entry and stay `$0`). Deliberately rough.
/// Returns all dates; the caller filters by time range.
fn approximation_events(chats_dir: &Path, tracking_db: &Path) -> CursorUsageEvents {
    let (conv_models, tracking_failure) = match load_conversation_models(tracking_db) {
        Ok(models) => (models, None),
        Err(error) => (
            FastHashMap::default(),
            Some(CursorAnalysisFailure {
                path: tracking_db.to_path_buf(),
                error: error.to_string(),
            }),
        ),
    };
    // (date, model) -> (summed context-window gauge, latest timestamp)
    let mut agg: HashMap<(String, String), (i64, i64)> = HashMap::new();
    let discovery = discover_cursor_store_dbs(chats_dir);
    let candidates = discovery.stores.len() + discovery.failures.len();
    let mut parsed = 0usize;
    let mut failures = discovery.failures;
    failures.extend(tracking_failure);
    for store_db in discovery.stores {
        let conv_id = conversation_id_from_path(&store_db);
        let read = match read_store_context(&store_db, &conv_models, &conv_id) {
            Ok(read) if read.expected_records > 0 && read.parsed_records == 0 => {
                failures.push(CursorAnalysisFailure {
                    path: store_db,
                    error: format!(
                        "none of {} Cursor usage payloads used a supported schema",
                        read.expected_records
                    ),
                });
                continue;
            }
            Ok(read) => {
                parsed += 1;
                if read.expected_records > read.parsed_records {
                    failures.push(CursorAnalysisFailure {
                        path: store_db.clone(),
                        error: format!(
                            "{} Cursor usage payloads used an unsupported schema",
                            read.expected_records - read.parsed_records
                        ),
                    });
                }
                read
            }
            Err(error) => {
                failures.push(CursorAnalysisFailure {
                    path: store_db,
                    error: error.to_string(),
                });
                continue;
            }
        };
        for (ts, ctx) in read.turns {
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };
            let entry = agg.entry((date, read.model.clone())).or_insert((0, ts));
            entry.0 += ctx;
            entry.1 = entry.1.max(ts);
        }
    }

    CursorUsageEvents {
        events: agg
            .into_iter()
            .map(|((date, model), (ctx, timestamp_ms))| UsageEvent {
                date,
                timestamp_ms,
                model,
                input: 0,
                output: 0,
                cache_read: ctx,
                cache_write: 0,
                cost: 0.0,
            })
            .collect(),
        candidates,
        parsed,
        failures,
    }
}

// ===========================================================================
// analysis: store.db parsing
// ===========================================================================

/// Enumerates every `chats/<projectHash>/<conversationId>/store.db` under the
/// chats root (exactly two directory levels deep).
pub(crate) fn discover_cursor_store_dbs(chats_dir: &Path) -> CursorStoreDiscovery {
    let mut dbs = Vec::new();
    let mut failures = Vec::new();
    let projects = match std::fs::read_dir(chats_dir) {
        Ok(projects) => projects,
        Err(error) => {
            return CursorStoreDiscovery {
                stores: dbs,
                failures: vec![CursorAnalysisFailure {
                    path: chats_dir.to_path_buf(),
                    error: error.to_string(),
                }],
            };
        }
    };
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                failures.push(CursorAnalysisFailure {
                    path: chats_dir.to_path_buf(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        match project.file_type() {
            Ok(kind) if !kind.is_dir() => continue,
            Ok(_) => {}
            Err(error) => {
                failures.push(CursorAnalysisFailure {
                    path: project.path(),
                    error: error.to_string(),
                });
                continue;
            }
        }
        let conversations = match std::fs::read_dir(project.path()) {
            Ok(conversations) => conversations,
            Err(error) => {
                failures.push(CursorAnalysisFailure {
                    path: project.path(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        for conv in conversations {
            let conv = match conv {
                Ok(conv) => conv,
                Err(error) => {
                    failures.push(CursorAnalysisFailure {
                        path: project.path(),
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            match conv.file_type() {
                Ok(kind) if !kind.is_dir() => continue,
                Ok(_) => {}
                Err(error) => {
                    failures.push(CursorAnalysisFailure {
                        path: conv.path(),
                        error: error.to_string(),
                    });
                    continue;
                }
            }
            let db = conv.path().join("store.db");
            match std::fs::metadata(&db) {
                Ok(metadata) if metadata.is_file() => dbs.push(db),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => failures.push(CursorAnalysisFailure {
                    path: db,
                    error: error.to_string(),
                }),
            }
        }
    }
    dbs.sort_unstable();
    failures.sort_by(|left, right| left.path.cmp(&right.path));
    CursorStoreDiscovery {
        stores: dbs,
        failures,
    }
}

/// The conversationId is the store.db's parent directory name.
fn conversation_id_from_path(store_db: &Path) -> String {
    store_db
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Loads `conversationId -> model` from `ai-code-tracking.db`.
///
/// Each conversation is authored by a single model in practice; when more than
/// one appears, the one with the most tracked lines wins. Returns an empty map
/// when the DB is absent; read and schema failures remain retryable errors.
pub(crate) fn load_conversation_models(tracking_db: &Path) -> Result<FastHashMap<String, String>> {
    match std::fs::metadata(tracking_db) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FastHashMap::default());
        }
        Err(error) => return Err(error.into()),
    }
    with_readonly_connection(
        tracking_db,
        "ai_code_hashes",
        "vct-cursor-",
        "Cursor",
        |conn| {
            let mut stmt = conn.prepare(
                "SELECT conversationId, model, COUNT(*) AS c FROM ai_code_hashes \
             WHERE conversationId IS NOT NULL AND conversationId != '' \
               AND model IS NOT NULL AND model != '' \
             GROUP BY conversationId, model ORDER BY c DESC, model ASC",
            )?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut m: FastHashMap<String, String> = FastHashMap::default();
            for row in rows {
                let row = row?;
                // Rows are ordered by descending line count, so the first model seen
                // for a conversation is its dominant one.
                m.entry(row.0).or_insert(row.1);
            }
            Ok(m)
        },
    )
}

/// Model attribution read from one stable tracking-database fingerprint.
pub(crate) struct ConversationModelSnapshot {
    pub(crate) models: FastHashMap<String, String>,
    pub(crate) fingerprint: Option<DatabaseFingerprint>,
}

/// Loads Cursor model attribution without pairing map A with fingerprint B.
pub(crate) fn load_conversation_model_snapshot(
    tracking_db: &Path,
) -> Result<ConversationModelSnapshot> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 0..MAX_ATTEMPTS {
        let before = optional_database_fingerprint(tracking_db)?;
        let models = load_conversation_models(tracking_db)?;
        let after = optional_database_fingerprint(tracking_db)?;
        if before == after {
            return Ok(ConversationModelSnapshot {
                models,
                fingerprint: after,
            });
        }
        if attempt + 1 == MAX_ATTEMPTS {
            anyhow::bail!(
                "Cursor tracking DB changed while being read after {MAX_ATTEMPTS} attempts: {}",
                tracking_db.display()
            );
        }
    }
    unreachable!("tracking snapshot loop always returns")
}

/// Parses one chat store into per-(date) analysis records for its model.
pub(crate) fn read_store_analysis(
    store_db: &Path,
    conv_models: &FastHashMap<String, String>,
    time_range: TimeRange,
    mode: ParseMode,
    user: &str,
    machine: &str,
) -> Result<CursorStoreAnalysis> {
    let conv_id = conversation_id_from_path(store_db);
    let cutoff = cutoff_string(time_range);

    with_readonly_connection(store_db, "blobs", "vct-cursor-", "Cursor", |conn| {
        let transaction = conn.unchecked_transaction()?;
        let model = resolve_store_model(&transaction, conv_models, &conv_id);
        let blobs = load_blobs(&transaction)?;

        // Pass 1: index Read tool results by tool-call id so their file lines can
        // be attributed to the matching Read call in pass 2.
        let read_results = collect_read_results(&blobs);
        let mut failed_payloads = 0usize;

        // Pass 2: fold each assistant turn's tool calls into a per-date state.
        let mut per_date: HashMap<String, SessionParseState> = HashMap::new();
        let mut normalized_messages = 0usize;
        for data in &blobs {
            if data.first() != Some(&0x0A) {
                continue;
            }
            let node = walk_node(data);
            let Some(msg_bytes) = node.msg else {
                // Cursor also writes aggregate DAG nodes with the normal
                // context-gauge field and a timestamp but no message. They are
                // not assistant turns. A timestamped node with neither the
                // message nor the known gauge remains suspicious schema drift.
                if node.ctx_msg.is_none()
                    && let Some(ts) = node.ts
                    && ms_to_local_date(ts).is_some_and(|date| !is_before_cutoff(&date, &cutoff))
                {
                    failed_payloads += 1;
                }
                continue;
            };
            if let Some(ts) = node.ts
                && ms_to_local_date(ts).is_some_and(|date| is_before_cutoff(&date, &cutoff))
            {
                continue;
            }
            let Ok(msg) = serde_json::from_slice::<Value>(msg_bytes) else {
                failed_payloads += 1;
                continue;
            };
            let Some(role) = msg.get("role").and_then(Value::as_str) else {
                failed_payloads += 1;
                continue;
            };
            // Only assistant turns carry tool calls. Guard before creating a
            // date bucket so a non-assistant node never emits a zero-metric row
            // or inflates the Cursor active-day count.
            if role != "assistant" {
                continue;
            }
            let Some(ts) = node.ts else {
                failed_payloads += 1;
                continue;
            };
            let Some(date) = ms_to_local_date(ts) else {
                failed_payloads += 1;
                continue;
            };
            if is_before_cutoff(&date, &cutoff) {
                continue;
            }
            if msg.get("content").and_then(Value::as_array).is_none() {
                failed_payloads += 1;
                continue;
            }
            let state = per_date.entry(date).or_insert_with(|| {
                let mut s = SessionParseState::with_mode(mode);
                s.task_id = conv_id.clone();
                s
            });
            state.last_ts = ts.max(state.last_ts);
            match apply_assistant_tools(state, &msg, &read_results, ts) {
                Ok(tool_failures) => {
                    normalized_messages += 1;
                    failed_payloads += tool_failures;
                }
                Err(()) => failed_payloads += 1,
            }
        }

        let mut out = Vec::with_capacity(per_date.len());
        for (date, state) in per_date {
            let mut usage = FastHashMap::default();
            // The analysis aggregator only reads the model key; the value is a
            // placeholder (real tokens come from the usage API path).
            usage.insert(model.clone(), json!({}));
            let record = state.into_record(usage);
            out.push((date, wrap_record(record, user, machine)));
        }
        transaction.commit()?;
        Ok(CursorStoreAnalysis {
            rows: out,
            normalized_messages,
            failed_payloads,
        })
    })
}

/// Reads a store's per-turn context-occupancy gauge for the usage approximation.
///
/// Returns the conversation's model plus `(timestamp_ms, context_tokens)` for
/// every assistant turn that carries the gauge.
struct CursorStoreContextRead {
    model: String,
    turns: Vec<(i64, i64)>,
    expected_records: usize,
    parsed_records: usize,
}

fn read_store_context(
    store_db: &Path,
    conv_models: &FastHashMap<String, String>,
    conv_id: &str,
) -> Result<CursorStoreContextRead> {
    with_readonly_connection(store_db, "blobs", "vct-cursor-", "Cursor", |conn| {
        let transaction = conn.unchecked_transaction()?;
        let model = resolve_store_model(&transaction, conv_models, conv_id);
        let blobs = load_blobs(&transaction)?;
        let mut turns = Vec::new();
        let mut expected_records = 0usize;
        let mut parsed_records = 0usize;
        for data in &blobs {
            if data.first() != Some(&0x0A) {
                continue;
            }
            let node = walk_node(data);
            let (Some(msg_bytes), Some(ts), Some(ctx_msg)) = (node.msg, node.ts, node.ctx_msg)
            else {
                continue;
            };
            // Only assistant turns represent a real per-request context. Cursor
            // also stores intermediate DAG nodes that carry the running gauge but
            // no assistant message; counting those would roughly double the
            // approximation's tokens and inflate its active-day count.
            let role = message_role(msg_bytes);
            match role.as_deref() {
                Some("assistant") => {
                    expected_records += 1;
                    if let Some(ctx) = context_tokens(ctx_msg) {
                        turns.push((ts, ctx));
                        parsed_records += 1;
                    }
                }
                Some("user" | "tool" | "system") => {}
                Some(_) | None => expected_records += 1,
            }
        }
        transaction.commit()?;
        Ok(CursorStoreContextRead {
            model,
            turns,
            expected_records,
            parsed_records,
        })
    })
}

/// Resolves a store's model: the tracking DB attribution, else the store's own
/// `lastUsedModel`, else `"unknown"`.
fn resolve_store_model(
    conn: &Connection,
    conv_models: &FastHashMap<String, String>,
    conv_id: &str,
) -> String {
    conv_models
        .get(conv_id)
        .cloned()
        .or_else(|| store_meta_model(conn))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Reads `lastUsedModel` from the store's `meta` row.
///
/// The `meta.value` is a hex-encoded JSON string; decode then read the field.
/// Tolerates a plain-JSON value too, in case a future build stops hex-encoding.
fn store_meta_model(conn: &Connection) -> Option<String> {
    let value: String = conn
        .query_row("SELECT value FROM meta LIMIT 1", [], |r| r.get(0))
        .ok()?;
    let bytes = decode_hex(&value).unwrap_or_else(|| value.clone().into_bytes());
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    json.get("lastUsedModel")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Loads every blob's raw bytes from a store.
fn load_blobs(conn: &Connection) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare("SELECT data FROM blobs ORDER BY id")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Returns `(assistant message JSON bytes, timestamp_ms)` for a binary DAG node.
///
/// Binary nodes start with `field 1` (`0x0A`) and embed exactly one assistant
/// message in `field 4`; `field 26` is the epoch-ms timestamp. Non-node blobs
/// (JSON messages) return `None`, as do nodes missing the timestamp — an
/// undateable turn is skipped rather than mis-bucketed to the epoch (1970).
#[cfg(test)]
fn assistant_node(data: &[u8]) -> Option<(&[u8], i64)> {
    if data.first() != Some(&0x0A) {
        return None;
    }
    let node = walk_node(data);
    Some((node.msg?, node.ts?))
}

/// Whether a message JSON blob is an assistant turn.
fn message_role(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|message| {
            message
                .get("role")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

/// Applies one assistant message's tool calls to `state`.
fn apply_assistant_tools(
    state: &mut SessionParseState,
    msg: &Value,
    read_results: &HashMap<String, CursorReadResult>,
    ts: i64,
) -> std::result::Result<usize, ()> {
    if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return Err(());
    }
    let Some(content) = msg.get("content").and_then(|v| v.as_array()) else {
        return Err(());
    };
    let mut failures = 0usize;
    for c in content {
        if c.get("type").and_then(|v| v.as_str()) != Some("tool-call") {
            continue;
        }
        let Some(tool) = c
            .get("toolName")
            .and_then(Value::as_str)
            .filter(|tool| !tool.is_empty())
        else {
            failures += 1;
            continue;
        };
        let args = c.get("args").and_then(Value::as_object);
        let arg = |key: &str| -> Option<&str> { args?.get(key)?.as_str() };
        match tool {
            "Write" => {
                let (Some(path), Some(contents)) =
                    (arg("path").filter(|path| !path.is_empty()), arg("contents"))
                else {
                    failures += 1;
                    continue;
                };
                state.add_write_detail(path, contents, ts);
            }
            "StrReplace" => {
                let (Some(path), Some(old), Some(new)) = (
                    arg("path").filter(|path| !path.is_empty()),
                    arg("old_string"),
                    arg("new_string"),
                ) else {
                    failures += 1;
                    continue;
                };
                state.add_edit_detail(path, old, new, ts);
            }
            "Read" | "ReadFile" => {
                let (Some(path), Some(tool_call_id)) = (
                    arg("path").filter(|path| !path.is_empty()),
                    c.get("toolCallId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty()),
                ) else {
                    failures += 1;
                    continue;
                };
                match read_results.get(tool_call_id) {
                    Some(CursorReadResult::Content(content)) if !content.is_empty() => {
                        state.add_read_detail(path, content, ts);
                    }
                    Some(CursorReadResult::Unsupported) => {
                        failures += 1;
                        state.tool_counts.read += 1;
                    }
                    _ => {
                        // A read whose result we could not recover still counts as a
                        // read invocation, matching the OpenCode reader.
                        state.tool_counts.read += 1;
                    }
                }
            }
            "Shell" => {
                let Some(command) = arg("command").filter(|command| !command.trim().is_empty())
                else {
                    failures += 1;
                    continue;
                };
                state.add_run_command(command, arg("description").unwrap_or(""), ts);
            }
            "TodoWrite" => state.tool_counts.todo_write += 1,
            // Grep / Glob / Delete etc. are not part of the tracked tool set.
            _ => {}
        }
    }
    Ok(failures)
}

enum CursorReadResult {
    Content(String),
    Unsupported,
}

/// Indexes `Read` tool results by tool-call id, with line-number prefixes
/// stripped so the recovered content is the file's own lines.
fn collect_read_results(blobs: &[Vec<u8>]) -> HashMap<String, CursorReadResult> {
    let mut map = HashMap::new();
    for data in blobs {
        if data.first() != Some(&b'{') {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(data) else {
            continue;
        };
        let role = msg.get("role").and_then(Value::as_str);
        // Assistant messages are also stored as standalone content-addressed
        // JSON blobs and referenced from binary DAG nodes. Pass 2 reads the
        // dated node, so this undated payload copy is expected and ignored.
        if role == Some("assistant") {
            continue;
        }
        if role != Some("tool") {
            continue;
        }
        let Some(content) = msg.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in content {
            if c.get("type").and_then(|v| v.as_str()) != Some("tool-result") {
                continue;
            }
            if !matches!(
                c.get("toolName").and_then(|v| v.as_str()),
                Some("Read" | "ReadFile")
            ) {
                continue;
            }
            let Some(id) = c
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if let Some(result) = c.get("result").and_then(|v| v.as_str()) {
                map.insert(
                    id.to_string(),
                    CursorReadResult::Content(strip_cursor_line_numbers(result)),
                );
            } else {
                map.entry(id.to_string())
                    .or_insert(CursorReadResult::Unsupported);
            }
        }
    }
    map
}

/// Strips Cursor's `"<spaces><digits>|"` read-output line prefixes, keeping only
/// the numbered content lines so the recovered text has the file's line count.
fn strip_cursor_line_numbers(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        if let Some(content) = strip_line_number_prefix(line) {
            lines.push(content);
        }
    }
    lines.join("\n")
}

/// Returns the content after a `"<spaces><digits>|"` prefix, or `None` when the
/// line has no such prefix.
fn strip_line_number_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    let digits_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits_end > 0 && trimmed[digits_end..].starts_with('|') {
        Some(&trimmed[digits_end + 1..])
    } else {
        None
    }
}

// ===========================================================================
// protobuf DAG node decoding
// ===========================================================================

/// The subset of a binary DAG node fields the reader needs.
#[derive(Default)]
struct NodeFields<'a> {
    /// `field 4`: the embedded message JSON bytes.
    msg: Option<&'a [u8]>,
    /// `field 26`: epoch-ms timestamp.
    ts: Option<i64>,
    /// `field 5`: the nested context-gauge message bytes.
    ctx_msg: Option<&'a [u8]>,
}

/// Walks a node's top-level protobuf fields, extracting `field 4/5/26`.
///
/// Deliberately does not traverse the DAG (child hash refs in `field 1/3`); it
/// only reads the three fields the reader cares about, so it stays robust to
/// unrelated schema additions.
fn walk_node(data: &[u8]) -> NodeFields<'_> {
    let mut nf = NodeFields::default();
    let mut i = 0;
    while i < data.len() {
        let Some((tag, ni)) = read_varint(data, i) else {
            break;
        };
        i = ni;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => {
                let Some((v, ni)) = read_varint(data, i) else {
                    break;
                };
                i = ni;
                if field == 26 {
                    nf.ts = Some(v as i64);
                }
            }
            2 => {
                let Some((len, ni)) = read_varint(data, i) else {
                    break;
                };
                i = ni;
                let Some(end) = i.checked_add(len as usize).filter(|e| *e <= data.len()) else {
                    break;
                };
                match field {
                    4 => nf.msg = Some(&data[i..end]),
                    5 => nf.ctx_msg = Some(&data[i..end]),
                    _ => {}
                }
                i = end;
            }
            5 => i = i.saturating_add(4),
            1 => i = i.saturating_add(8),
            _ => break,
        }
    }
    nf
}

/// Reads the context-occupancy value (`field 1`) out of a `field 5` gauge message.
fn context_tokens(ctx_msg: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i < ctx_msg.len() {
        let (tag, ni) = read_varint(ctx_msg, i)?;
        i = ni;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => {
                let (v, ni) = read_varint(ctx_msg, i)?;
                i = ni;
                if field == 1 {
                    return Some(v as i64);
                }
            }
            2 => {
                let (len, ni) = read_varint(ctx_msg, i)?;
                i = ni;
                i = i.checked_add(len as usize)?;
            }
            5 => i = i.checked_add(4)?,
            1 => i = i.checked_add(8)?,
            _ => break,
        }
    }
    None
}

/// Reads a base-128 varint at `pos`, returning `(value, next_pos)`.
///
/// Bounded to 10 bytes and to the slice length so a truncated blob can never
/// spin or read out of bounds.
fn read_varint(data: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    let mut i = pos;
    while i < data.len() && shift < 64 {
        let byte = data[i];
        result |= u64::from(byte & 0x7f) << shift;
        i += 1;
        if byte & 0x80 == 0 {
            return Some((result, i));
        }
        shift += 7;
    }
    None
}

// ===========================================================================
// shared helpers
// ===========================================================================

/// Builds the Claude-style flat usage value the token extractor understands.
fn cursor_usage_value(
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
) -> UsageTokenContribution {
    UsageTokenContribution {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: 0,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
    }
}

/// Wraps a record into a Cursor-tagged [`CodeAnalysis`].
fn wrap_record(record: CodeAnalysisRecord, user: &str, machine: &str) -> CodeAnalysis {
    CodeAnalysis {
        user: user.to_string(),
        extension_name: ExtensionType::Cursor.to_string(),
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

/// Decodes an even-length ASCII hex string into bytes, or `None` when it is not
/// valid hex.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_roundtrips_json() {
        let json = r#"{"lastUsedModel":"composer-2"}"#;
        let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
        let decoded = decode_hex(&hex).unwrap();
        assert_eq!(decoded, json.as_bytes());
    }

    #[test]
    fn hex_decode_rejects_non_hex() {
        assert!(decode_hex("not-hex!!").is_none());
        assert!(decode_hex("abc").is_none()); // odd length
    }

    #[test]
    fn strips_cursor_line_number_prefixes() {
        let raw = "     1|fn main() {\n     2|    let x = 1;\n     3|}";
        assert_eq!(
            strip_cursor_line_numbers(raw),
            "fn main() {\n    let x = 1;\n}"
        );
    }

    #[test]
    fn strip_ignores_unnumbered_lines() {
        // Only numbered content lines survive, so the recovered line count is the
        // file's line count.
        let raw = "header without number\n     1|real line";
        assert_eq!(strip_cursor_line_numbers(raw), "real line");
    }

    /// Builds a minimal binary DAG node: `field 4` = message JSON, `field 26` =
    /// timestamp varint, optional `field 5` = context gauge.
    fn make_node(msg: &str, ts: i64, ctx: Option<i64>) -> Vec<u8> {
        fn varint(mut v: u64, out: &mut Vec<u8>) {
            loop {
                let mut b = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    b |= 0x80;
                }
                out.push(b);
                if v == 0 {
                    break;
                }
            }
        }
        // A protobuf tag is itself a varint `(field << 3) | wire`; field 26's tag
        // (208) needs two bytes, so encode every tag as a varint.
        fn tag(field: u64, wire: u64, out: &mut Vec<u8>) {
            varint((field << 3) | wire, out);
        }
        let mut out = Vec::new();
        // field 1 marker (a dummy 1-byte child ref) so the blob starts with 0x0A.
        tag(1, 2, &mut out);
        varint(1, &mut out);
        out.push(0x00);
        // field 4 (message JSON)
        tag(4, 2, &mut out);
        varint(msg.len() as u64, &mut out);
        out.extend_from_slice(msg.as_bytes());
        // field 5 (context gauge: field 1 = ctx)
        if let Some(ctx) = ctx {
            let mut inner = Vec::new();
            tag(1, 0, &mut inner);
            varint(ctx as u64, &mut inner);
            tag(5, 2, &mut out);
            varint(inner.len() as u64, &mut out);
            out.extend_from_slice(&inner);
        }
        // field 26 (timestamp)
        tag(26, 0, &mut out);
        varint(ts as u64, &mut out);
        out
    }

    #[test]
    fn assistant_node_extracts_message_and_ts() {
        let node = make_node(
            r#"{"role":"assistant","content":[]}"#,
            1_700_000_000_000,
            None,
        );
        let (msg, ts) = assistant_node(&node).unwrap();
        assert_eq!(ts, 1_700_000_000_000);
        let parsed: Value = serde_json::from_slice(msg).unwrap();
        assert_eq!(parsed["role"], "assistant");
    }

    #[test]
    fn node_context_gauge_decodes() {
        let node = make_node(r#"{"role":"assistant"}"#, 1, Some(568_964));
        let nf = walk_node(&node);
        assert_eq!(context_tokens(nf.ctx_msg.unwrap()), Some(568_964));
    }

    #[test]
    fn json_blob_is_not_an_assistant_node() {
        let blob = br#"{"role":"assistant","content":[]}"#.to_vec();
        assert!(assistant_node(&blob).is_none());
    }

    #[test]
    fn load_blobs_uses_stable_id_order() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB)")
            .unwrap();
        conn.execute("INSERT INTO blobs VALUES ('z', X'02')", [])
            .unwrap();
        conn.execute("INSERT INTO blobs VALUES ('a', X'01')", [])
            .unwrap();

        assert_eq!(load_blobs(&conn).unwrap(), vec![vec![1], vec![2]]);
    }

    #[test]
    fn dominant_model_ties_use_lexical_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracking.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ai_code_hashes (conversationId TEXT, model TEXT); \
             INSERT INTO ai_code_hashes VALUES ('conversation', 'z-model'); \
             INSERT INTO ai_code_hashes VALUES ('conversation', 'a-model');",
        )
        .unwrap();
        drop(conn);

        let models = load_conversation_models(&path).unwrap();
        assert_eq!(
            models.get("conversation").map(String::as_str),
            Some("a-model")
        );
    }

    #[test]
    fn apply_tools_counts_and_lines() {
        let mut state = SessionParseState::with_mode(ParseMode::Full);
        let msg = json!({
            "role": "assistant",
            "content": [
                {"type": "tool-call", "toolName": "Write", "toolCallId": "a",
                 "args": {"path": "/repo/x.rs", "contents": "line1\nline2"}},
                {"type": "tool-call", "toolName": "StrReplace", "toolCallId": "b",
                 "args": {"path": "/repo/y.rs", "old_string": "old", "new_string": "new1\nnew2"}},
                {"type": "tool-call", "toolName": "Shell", "toolCallId": "c",
                 "args": {"command": "ls -la", "description": "list"}},
                {"type": "tool-call", "toolName": "TodoWrite", "toolCallId": "d", "args": {}},
                {"type": "tool-call", "toolName": "Read", "toolCallId": "e",
                 "args": {"path": "/repo/z.rs"}},
                {"type": "tool-call", "toolName": "Grep", "toolCallId": "f",
                 "args": {"pattern": "foo"}},
                // Cursor also emits the read tool as `ReadFile` in some versions.
                {"type": "tool-call", "toolName": "ReadFile", "toolCallId": "g",
                 "args": {"path": "/repo/w.rs"}},
            ]
        });
        let mut reads = HashMap::new();
        reads.insert(
            "e".to_string(),
            CursorReadResult::Content("r1\nr2\nr3".to_string()),
        );
        reads.insert(
            "g".to_string(),
            CursorReadResult::Content("w1\nw2".to_string()),
        );
        apply_assistant_tools(&mut state, &msg, &reads, 42).unwrap();

        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.tool_counts.todo_write, 1);
        // Read + ReadFile both count as reads.
        assert_eq!(state.tool_counts.read, 2);
        assert_eq!(state.total_write_lines, 2);
        assert_eq!(state.total_edit_lines, 2);
        assert_eq!(state.total_read_lines, 5);
    }

    #[test]
    fn aggregate_events_filters_and_builds_records() {
        let events = vec![
            UsageEvent {
                date: "2999-01-01".to_string(),
                timestamp_ms: 32_470_920_000_000,
                model: "claude-sonnet-4.6".to_string(),
                input: 100,
                output: 20,
                cache_read: 50,
                cache_write: 10,
                cost: 1.5,
            },
            UsageEvent {
                date: "2000-01-01".to_string(),
                timestamp_ms: 946_684_800_000,
                model: "composer-2".to_string(),
                input: 5,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                cost: 0.0,
            },
        ];
        // Daily cutoff drops the ancient 2000 row but keeps the far-future one.
        let rows = aggregate_events(&events, TimeRange::Daily);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.date, "2999-01-01");
        assert_eq!(row.timestamp_ms, 32_470_920_000_000);
        assert!((row.stored_cost - 1.5).abs() < 1e-9);
        assert_eq!(row.model, "claude-sonnet-4.6");
    }

    /// Builds a temp `store.db` with the given binary nodes and JSON blobs.
    fn make_store_db(nodes: &[Vec<u8>], json_blobs: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        write_store_db(&path, nodes, json_blobs);
        (dir, path)
    }

    fn write_store_db(path: &Path, nodes: &[Vec<u8>], json_blobs: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); \
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        for (i, n) in nodes.iter().enumerate() {
            conn.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("n{i}"), n],
            )
            .unwrap();
        }
        for (i, j) in json_blobs.iter().enumerate() {
            conn.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("j{i}"), j.as_bytes()],
            )
            .unwrap();
        }
        drop(conn);
    }

    #[test]
    fn read_store_analysis_over_real_db_counts_tools_and_ignores_non_assistant() {
        let assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Write","toolCallId":"a","args":{"path":"/r/x.rs","contents":"l1\nl2"}},
                {"type":"tool-call","toolName":"Shell","toolCallId":"b","args":{"command":"ls","description":"d"}},
                {"type":"tool-call","toolName":"Read","toolCallId":"z","args":{"path":"/r/y.rs"}}
            ]}"#,
            1_700_000_000_000,
            Some(50_000),
        );
        // A non-assistant node must NOT create a date bucket or an active day.
        let user_node = make_node(r#"{"role":"user","content":[]}"#, 1_700_000_100_000, None);
        let tool_result = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","toolCallId":"z","result":"     1|line one\n     2|line two"}]}"#;
        let assistant_payload = r#"{"role":"assistant","content":[]}"#;
        let (_dir, path) =
            make_store_db(&[assistant, user_node], &[tool_result, assistant_payload]);

        let conv_models = FastHashMap::default();
        let store = read_store_analysis(
            &path,
            &conv_models,
            TimeRange::All,
            ParseMode::Full,
            "u",
            "m",
        )
        .unwrap();

        // Exactly one assistant turn -> one (date) record; the user node is dropped.
        assert_eq!(store.rows.len(), 1);
        assert_eq!(store.normalized_messages, 1);
        assert_eq!(store.failed_payloads, 0);
        let rec = &store.rows[0].1.records[0];
        assert_eq!(rec.tool_call_counts.write, 1);
        assert_eq!(rec.tool_call_counts.bash, 1);
        assert_eq!(rec.tool_call_counts.read, 1);
        assert_eq!(rec.total_write_lines, 2);
        // Read result lines were recovered and prefix-stripped (2 numbered lines).
        assert_eq!(rec.total_read_lines, 2);
        // No tracking DB / meta -> model falls back to "unknown".
        assert!(rec.conversation_usage.contains_key("unknown"));
    }

    #[test]
    fn read_store_context_recovers_gauge_per_assistant_turn_only() {
        let a = make_node(
            r#"{"role":"assistant","content":[]}"#,
            1_700_000_000_000,
            Some(42_000),
        );
        let b = make_node(
            r#"{"role":"assistant","content":[]}"#,
            1_700_000_500_000,
            Some(88_000),
        );
        // A gauge-bearing node that is NOT an assistant turn must be excluded so
        // the offline approximation does not double-count context.
        let non_assistant = make_node(
            r#"{"role":"user","content":[]}"#,
            1_700_000_600_000,
            Some(99_999),
        );
        let (_dir, path) = make_store_db(&[a, b, non_assistant], &[]);

        let conv_models = FastHashMap::default();
        let mut read = read_store_context(&path, &conv_models, "conv").unwrap();
        read.turns.sort();
        assert_eq!(read.model, "unknown");
        assert_eq!(
            read.turns,
            vec![(1_700_000_000_000, 42_000), (1_700_000_500_000, 88_000)]
        );
        assert_eq!(read.expected_records, 2);
        assert_eq!(read.parsed_records, 2);
    }

    #[test]
    fn cursor_usage_reports_unknown_message_schema() {
        for message in ["not json", r#"{"content":[]}"#] {
            let dir = tempfile::tempdir().unwrap();
            let chats = dir.path().join("chats");
            let store = chats.join("project/conversation/store.db");
            let node = make_node(message, 1_700_000_000_000, Some(123));
            write_store_db(&store, &[node], &[]);

            let result = read_cursor_usage_with_diagnostics(
                &chats,
                &dir.path().join("tracking.db"),
                TimeRange::All,
            );
            assert_eq!(result.candidates, 1);
            assert_eq!(result.parsed, 0);
            assert!(result.rows.is_empty());
            assert_eq!(result.failures.len(), 1);
            assert!(result.failures[0].error.contains("none of 1"));
        }

        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let valid = make_node(
            r#"{"role":"assistant","content":[]}"#,
            1_700_000_000_000,
            Some(123),
        );
        let invalid = make_node(r#"{"content":[]}"#, 1_700_000_001_000, Some(456));
        write_store_db(&store, &[valid, invalid], &[]);
        let result = read_cursor_usage_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
        );
        assert_eq!(result.candidates, 1);
        assert_eq!(result.parsed, 1);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].error.contains("1 Cursor usage payload"));
    }

    #[test]
    fn cursor_store_diagnostics_reject_undecodable_assistant_messages() {
        for message in ["not json", r#"{"role":"assistant","futureContent":[]}"#] {
            let dir = tempfile::tempdir().unwrap();
            let chats = dir.path().join("chats");
            let store = chats.join("project/conversation/store.db");
            let node = make_node(message, 1_700_000_000_000, None);
            write_store_db(&store, &[node], &[]);

            let result = read_cursor_analysis_with_diagnostics(
                &chats,
                &dir.path().join("tracking.db"),
                TimeRange::All,
                ParseMode::Full,
            );
            assert_eq!(result.candidates, 1);
            assert_eq!(result.parsed, 0);
            assert!(result.rows.is_empty());
            assert_eq!(result.failures.len(), 1);
            assert!(result.failures[0].error.contains("none of 1"));
        }

        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let mut unknown_node = make_node(
            r#"{"role":"assistant","content":[]}"#,
            1_700_000_000_000,
            None,
        );
        assert_eq!(unknown_node[3], 0x22);
        unknown_node[3] = 0x32;
        write_store_db(&store, &[unknown_node], &[]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
            ParseMode::Full,
        );
        assert_eq!(result.candidates, 1);
        assert_eq!(result.parsed, 0);
        assert_eq!(result.failures.len(), 1);
    }

    #[test]
    fn cursor_store_diagnostics_ignore_context_aggregate_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let message = r#"{"role":"assistant","content":[]}"#;
        let mut aggregate = make_node(message, 1_700_000_000_000, Some(42_000));
        aggregate.drain(3..5 + message.len());
        write_store_db(&store, &[aggregate], &[]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
            ParseMode::Full,
        );
        assert_eq!(result.candidates, 1);
        assert_eq!(result.parsed, 1);
        assert!(result.rows.is_empty());
        assert!(result.failures.is_empty());
    }

    #[test]
    fn cursor_store_diagnostics_report_known_tool_schema_drift() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Write","toolCallId":"w","args":{"futurePath":"/repo/a","futureContents":"text"}},
                {"type":"tool-call","toolName":"Read","toolCallId":"r","args":{"path":"/repo/b"}}
            ]}"#,
            1_700_000_000_000,
            None,
        );
        let read_result = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","toolCallId":"r","futureResult":"text"}]}"#;
        write_store_db(&store, &[assistant], &[read_result]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
            ParseMode::Full,
        );
        assert_eq!(result.candidates, 1);
        assert_eq!(result.parsed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].error.contains("2 analyzer payloads"));
        assert_eq!(result.rows.len(), 1);
        let record = &result.rows[0].analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
    }

    #[test]
    fn cursor_store_diagnostics_ignore_out_of_range_malformed_read_results() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let old_assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Read","toolCallId":"old","args":{"path":"/repo/old"}}
            ]}"#,
            946_684_800_000,
            None,
        );
        let malformed_result = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","toolCallId":"old","futureResult":"text"}]}"#;
        write_store_db(&store, &[old_assistant], &[malformed_result]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::Daily,
            ParseMode::Full,
        );
        assert_eq!(result.candidates, 1);
        assert_eq!(result.parsed, 1);
        assert!(result.rows.is_empty());
        assert!(result.failures.is_empty());
    }

    #[test]
    fn out_of_range_malformed_read_result_does_not_taint_current_turn() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let old_assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Read","toolCallId":"old","args":{"path":"/repo/old"}}
            ]}"#,
            946_684_800_000,
            None,
        );
        let current_assistant = make_node(
            r#"{"role":"assistant","content":[]}"#,
            4_102_444_800_000,
            None,
        );
        let malformed_result = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","toolCallId":"old","futureResult":"text"}]}"#;
        write_store_db(
            &store,
            &[old_assistant, current_assistant],
            &[malformed_result],
        );

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::Daily,
            ParseMode::Full,
        );
        assert_eq!(result.parsed, 1);
        assert_eq!(result.rows.len(), 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn in_range_malformed_read_result_remains_a_partial_failure() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Read","toolCallId":"current","args":{"path":"/repo/current"}}
            ]}"#,
            4_102_444_800_000,
            None,
        );
        let malformed_result = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","toolCallId":"current","futureResult":"text"}]}"#;
        write_store_db(&store, &[assistant], &[malformed_result]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::Daily,
            ParseMode::Full,
        );
        assert_eq!(result.parsed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].error.contains("1 analyzer payload"));
        let record = &result.rows[0].analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
    }

    #[test]
    fn unkeyed_tool_blobs_do_not_taint_an_in_range_read() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        let assistant = make_node(
            r#"{"role":"assistant","content":[
                {"type":"tool-call","toolName":"Read","toolCallId":"missing","args":{"path":"/repo/current"}}
            ]}"#,
            4_102_444_800_000,
            None,
        );
        let non_array = r#"{"role":"tool","content":"future"}"#;
        let missing_id = r#"{"role":"tool","content":[{"type":"tool-result","toolName":"Read","result":"text"}]}"#;
        write_store_db(&store, &[assistant], &[non_array, missing_id]);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::Daily,
            ParseMode::Full,
        );
        assert_eq!(result.parsed, 1);
        assert!(result.failures.is_empty());
        let record = &result.rows[0].analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
    }

    #[test]
    fn read_cursor_analysis_errors_when_every_store_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let store = chats.join("project/conversation/store.db");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        let conn = Connection::open(&store).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data TEXT); \
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO blobs VALUES ('bad', 'not a blob');",
        )
        .unwrap();
        drop(conn);

        let error = read_cursor_analysis(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
            ParseMode::Full,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to read all 1 Cursor chat stores")
        );
    }

    #[test]
    fn cursor_store_diagnostics_preserve_partial_failures() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");

        let valid = chats.join("project/valid/store.db");
        std::fs::create_dir_all(valid.parent().unwrap()).unwrap();
        let conn = Connection::open(&valid).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); \
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        drop(conn);

        let invalid = chats.join("project/invalid/store.db");
        std::fs::create_dir_all(invalid.parent().unwrap()).unwrap();
        let conn = Connection::open(&invalid).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data TEXT); \
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO blobs VALUES ('bad', 'not a blob');",
        )
        .unwrap();
        drop(conn);

        let result = read_cursor_analysis_with_diagnostics(
            &chats,
            &dir.path().join("tracking.db"),
            TimeRange::All,
            ParseMode::Full,
        );
        assert_eq!(result.candidates, 2);
        assert_eq!(result.parsed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, invalid);
    }
}
