//! Cursor session reader (SQLite blob stores + dashboard usage API).
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
//! gauge), so the `usage` view fetches authoritative per-model tokens + cost
//! from Cursor's dashboard usage-events API using the same `WorkosCursorSessionToken`
//! the quota panel already reads, cached briefly in `~/.vct`. When the API is
//! unreachable it falls back to a clearly-approximate local estimate derived
//! from the context gauge.
//!
//! Both entry points return the same `(local YYYY-MM-DD, CodeAnalysis[, cost])`
//! shape the OpenCode reader produces, so the `usage` / `analysis` aggregators
//! fold Cursor in exactly like the other providers.

use crate::VERSION;
use crate::cli::TimeRange;
use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, CodeAnalysisRecord, ExtensionType};
use crate::quota::cursor::{cursor_ua, read_cursor_session};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_cache_dir, get_current_user, get_cursor_auth_path, get_machine_id};
use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Cursor's individual (cookie-authed) per-request usage-events endpoint.
const CURSOR_EVENTS_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
/// `teamId` value that scopes the dashboard API to the individual's own usage.
const CURSOR_INDIVIDUAL_TEAM_ID: i64 = 0;
/// Page size for the paginated usage-events fetch.
const CURSOR_EVENTS_PAGE_SIZE: i64 = 300;
/// Hard cap on pages fetched, so a runaway `totalUsageEventsCount` can't spin.
const CURSOR_EVENTS_MAX_PAGES: i64 = 200;
/// How long a cached usage-events fetch stays fresh (seconds). Billing events
/// are immutable history, so a short TTL keeps the `usage` TUI from hammering
/// the endpoint on every refresh while staying current enough.
const CURSOR_USAGE_CACHE_TTL_SECS: i64 = 120;
/// HTTP timeout for the usage-events request, so an offline `vct usage` fails
/// fast into the local approximation instead of hanging.
const CURSOR_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

// ===========================================================================
// Public entry points
// ===========================================================================

/// Reads per-model token usage + cost for Cursor.
///
/// Primary source is Cursor's dashboard usage-events API (real billing tokens
/// and Cursor's own cost), cached in `~/.vct`. When the API cannot be reached
/// and no cache exists, falls back to a local context-occupancy approximation
/// derived from the chat stores (the context gauge counted as cache-read tokens,
/// so it prices at the cache rate or `$0` for models Cursor prices itself).
///
/// Each returned tuple is `(local YYYY-MM-DD, CodeAnalysis, cost_usd)` with the
/// analysis carrying one model's `conversation_usage`, matching
/// [`crate::session::read_opencode_usage`].
///
/// # Errors
///
/// Returns an error only if the local approximation fallback itself fails; API
/// and cache problems degrade rather than propagate.
pub fn read_cursor_usage(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
) -> Result<Vec<(String, CodeAnalysis, f64)>> {
    let now = chrono::Local::now().timestamp();
    let user = get_current_user();
    let machine = get_machine_id().to_string();
    let cached = load_usage_cache();

    // A fresh cache — from a prior API fetch OR a prior offline approximation —
    // is reused as-is: no HTTP, no store re-scan. This bounds the work to at
    // most once per TTL even when the API keeps failing, so the usage TUI (which
    // re-runs this on every refresh) never hammers the endpoint or re-reads
    // every store.db each tick.
    if let Some(cache) = &cached
        && now.saturating_sub(cache.fetched_at) < CURSOR_USAGE_CACHE_TTL_SECS
    {
        return Ok(aggregate_events(&cache.events, time_range, &user, &machine));
    }

    // Cache stale or absent: make exactly one refresh attempt this call.
    let (events, from_api) = match fetch_all_events() {
        Ok(events) => (events, true),
        Err(err) => match cached {
            // Prefer stale real billing data over the rough approximation.
            Some(cache) if cache.from_api && !cache.events.is_empty() => {
                eprintln!("Warning: Cursor usage API unavailable ({err}); using cached usage.");
                (cache.events, true)
            }
            _ => {
                eprintln!(
                    "Warning: Cursor usage API unavailable ({err}); \
                     Cursor usage is an approximation from local context data."
                );
                (approximation_events(chats_dir, tracking_db)?, false)
            }
        },
    };

    // Persist the result (API, reused-real, or approximation) with a fresh
    // timestamp so the next refresh within the TTL is a pure cache read and does
    // not retry the network or re-scan the stores.
    let _ = save_usage_cache(&events, now, from_api);
    Ok(aggregate_events(&events, time_range, &user, &machine))
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
/// Never fails the whole scan for one bad store: a store that cannot be read is
/// logged to stderr and skipped. Returns `Ok` with whatever parsed.
pub fn read_cursor_analysis(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
    mode: ParseMode,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let conv_models = load_conversation_models(tracking_db);
    let user = get_current_user();
    let machine = get_machine_id().to_string();

    let mut out = Vec::new();
    for store_db in cursor_store_dbs(chats_dir) {
        match read_store_analysis(&store_db, &conv_models, time_range, mode, &user, &machine) {
            Ok(mut rows) => out.append(&mut rows),
            Err(err) => {
                eprintln!(
                    "Warning: Failed to read Cursor store {}: {err}",
                    store_db.display()
                );
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// usage: dashboard API
// ===========================================================================

/// One usage-events aggregation row, cached in `~/.vct` and keyed by
/// `(date, model)` so any time range can filter it locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEvent {
    date: String,
    model: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

/// The on-disk usage-events cache: the fetch time plus the aggregated rows.
#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageCache {
    fetched_at: i64,
    events: Vec<CachedEvent>,
    /// Whether `events` came from the API (real billing data) rather than the
    /// local approximation. Real data is reused verbatim when the API is down;
    /// an approximation is recomputed each refresh window so it picks up new
    /// turns. Defaults to `false` for older caches without the field.
    #[serde(default)]
    from_api: bool,
}

/// Path to the usage-events cache file (`~/.vct/cursor_usage_events.json`).
fn usage_cache_path() -> Option<PathBuf> {
    get_cache_dir()
        .ok()
        .map(|d| d.join("cursor_usage_events.json"))
}

/// Loads the cached usage events, or `None` when absent/unreadable.
fn load_usage_cache() -> Option<UsageCache> {
    let path = usage_cache_path()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persists the usage events with the current timestamp and their source.
fn save_usage_cache(events: &[CachedEvent], fetched_at: i64, from_api: bool) -> Result<()> {
    let path = usage_cache_path().ok_or_else(|| anyhow!("no cache dir"))?;
    let cache = UsageCache {
        fetched_at,
        events: events.to_vec(),
        from_api,
    };
    let body = serde_json::to_string(&cache)?;
    std::fs::write(path, body)?;
    Ok(())
}

/// Fetches every usage event from the dashboard API and aggregates them by
/// `(date, model)`.
///
/// Fetches the full history (`startDate = 0`) so a single cache serves every
/// time range; the aggregators filter by date locally.
fn fetch_all_events() -> Result<Vec<CachedEvent>> {
    let auth_path = get_cursor_auth_path()?;
    let body = std::fs::read_to_string(&auth_path)
        .with_context(|| format!("no Cursor credentials at {}", auth_path.display()))?;
    let session = read_cursor_session(&body)
        .ok_or_else(|| anyhow!("no usable Cursor session in {}", auth_path.display()))?;

    let now_ms = chrono::Local::now().timestamp_millis();
    // JWT `exp` is in seconds; bail so the caller can fall back rather than
    // spamming a request that will only 401.
    if session.exp > 0 && session.exp.saturating_mul(1000) <= now_ms {
        return Err(anyhow!(
            "Cursor session token expired (run: cursor-agent login)"
        ));
    }

    let client = Client::builder().timeout(CURSOR_HTTP_TIMEOUT).build()?;

    // (date, model) -> (input, output, cache_read, cache_write, cost_usd)
    let mut agg: HashMap<(String, String), EventAcc> = HashMap::new();
    let mut page = 1i64;
    loop {
        let payload = json!({
            "teamId": CURSOR_INDIVIDUAL_TEAM_ID,
            "startDate": 0,
            "endDate": now_ms,
            "page": page,
            "pageSize": CURSOR_EVENTS_PAGE_SIZE,
        });
        let resp = client
            .post(CURSOR_EVENTS_URL)
            .header(reqwest::header::COOKIE, session.cookie.as_str())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, cursor_ua())
            .header(reqwest::header::ORIGIN, "https://cursor.com")
            .header(
                reqwest::header::REFERER,
                "https://cursor.com/dashboard?tab=usage",
            )
            .json(&payload)
            .send()
            .context("Failed to send Cursor usage-events request")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "Cursor usage-events API returned {}",
                resp.status()
            ));
        }
        let body: Value = resp
            .json()
            .context("Failed to parse Cursor usage-events response")?;

        let events = body
            .get("usageEventsDisplay")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if events.is_empty() {
            break;
        }
        for ev in &events {
            let Some(ts) = ev.get("timestamp").and_then(as_i64_lenient) else {
                continue;
            };
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };
            let model = ev
                .get("model")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string();
            let tu = ev.get("tokenUsage");
            let bucket = |key: &str| {
                tu.and_then(|t| t.get(key))
                    .and_then(as_i64_lenient)
                    .unwrap_or(0)
            };
            let e_input = bucket("inputTokens");
            let e_output = bucket("outputTokens");
            let e_cache_read = bucket("cacheReadTokens");
            let e_cache_write = bucket("cacheWriteTokens");
            let e_cost = event_cost_usd(ev);
            // Skip empty events (e.g. errored / not-charged interactions): they
            // would otherwise create 0-token / $0 model rows and count their
            // date as an active Cursor day.
            if e_input == 0
                && e_output == 0
                && e_cache_read == 0
                && e_cache_write == 0
                && e_cost == 0.0
            {
                continue;
            }
            let acc = agg.entry((date, model)).or_default();
            acc.input += e_input;
            acc.output += e_output;
            acc.cache_read += e_cache_read;
            acc.cache_write += e_cache_write;
            acc.cost += e_cost;
        }

        let total = body
            .get("totalUsageEventsCount")
            .and_then(as_i64_lenient)
            .unwrap_or(0);
        if page.saturating_mul(CURSOR_EVENTS_PAGE_SIZE) >= total || page >= CURSOR_EVENTS_MAX_PAGES
        {
            break;
        }
        page += 1;
    }

    Ok(agg
        .into_iter()
        .map(|((date, model), acc)| CachedEvent {
            date,
            model,
            input: acc.input,
            output: acc.output,
            cache_read: acc.cache_read,
            cache_write: acc.cache_write,
            cost: acc.cost,
        })
        .collect())
}

/// Running per-`(date, model)` token/cost accumulator while paginating.
#[derive(Default)]
struct EventAcc {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

/// The billed cost of one usage event, in USD.
///
/// Prefers the token cost (`tokenUsage.totalCents`, comparable to the LiteLLM
/// pricing the other providers use) and falls back to the event-level
/// `chargedCents`.
fn event_cost_usd(ev: &Value) -> f64 {
    let cents = ev
        .get("tokenUsage")
        .and_then(|t| t.get("totalCents"))
        .and_then(as_f64_lenient)
        .or_else(|| ev.get("chargedCents").and_then(as_f64_lenient))
        .unwrap_or(0.0);
    cents / 100.0
}

/// Turns cached usage events into the `(date, CodeAnalysis, cost)` tuples the
/// usage aggregator consumes, filtered by `time_range`.
fn aggregate_events(
    events: &[CachedEvent],
    time_range: TimeRange,
    user: &str,
    machine: &str,
) -> Vec<(String, CodeAnalysis, f64)> {
    let cutoff = cutoff_string(time_range);
    let mut out = Vec::new();
    for e in events {
        if is_before_cutoff(&e.date, &cutoff) {
            continue;
        }
        let usage = cursor_usage_value(e.input, e.output, e.cache_read, e.cache_write);
        let mut map = FastHashMap::default();
        map.insert(e.model.clone(), usage);
        let record = SessionParseState::with_mode(ParseMode::UsageOnly).into_record(map);
        out.push((e.date.clone(), wrap_record(record, user, machine), e.cost));
    }
    out
}

// ===========================================================================
// usage: offline approximation
// ===========================================================================

/// Builds all-time usage-approximation events from the local context gauge, for
/// use when the dashboard API is unreachable.
///
/// Cursor stores only the running context-window size per assistant turn, not
/// billed tokens. Each turn re-sends (and prompt-cache-reads) the accumulated
/// context, so summing the gauge across a conversation's turns approximates the
/// **cache-read** token volume — reported in the cache-read bucket both because
/// that is the honest bucket and because it is then priced at the much cheaper
/// cache rate rather than a wildly-inflated full-input rate. Input/output are
/// unknown (`0`) and the stored cost is `0` (models Cursor prices itself, e.g.
/// `composer-*`, have no LiteLLM entry and stay `$0`). Deliberately rough — the
/// real numbers come from the API path. Returns all dates; the caller filters by
/// time range after caching.
fn approximation_events(chats_dir: &Path, tracking_db: &Path) -> Result<Vec<CachedEvent>> {
    let conv_models = load_conversation_models(tracking_db);

    // (date, model) -> summed context-window gauge
    let mut agg: HashMap<(String, String), i64> = HashMap::new();
    for store_db in cursor_store_dbs(chats_dir) {
        let conv_id = conversation_id_from_path(&store_db);
        let Ok((model, turns)) = read_store_context(&store_db, &conv_models, &conv_id) else {
            continue;
        };
        for (ts, ctx) in turns {
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };
            *agg.entry((date, model.clone())).or_insert(0) += ctx;
        }
    }

    Ok(agg
        .into_iter()
        .map(|((date, model), ctx)| CachedEvent {
            date,
            model,
            input: 0,
            output: 0,
            // Re-sent context is cache-read; priced at the cache rate (or $0 for
            // Cursor's own models), never the inflated full-input rate.
            cache_read: ctx,
            cache_write: 0,
            cost: 0.0,
        })
        .collect())
}

// ===========================================================================
// analysis: store.db parsing
// ===========================================================================

/// Enumerates every `chats/<projectHash>/<conversationId>/store.db` under the
/// chats root (exactly two directory levels deep).
fn cursor_store_dbs(chats_dir: &Path) -> Vec<PathBuf> {
    let mut dbs = Vec::new();
    let Ok(projects) = std::fs::read_dir(chats_dir) else {
        return dbs;
    };
    for project in projects.flatten() {
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(convs) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for conv in convs.flatten() {
            if !conv.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let db = conv.path().join("store.db");
            if db.is_file() {
                dbs.push(db);
            }
        }
    }
    dbs
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
/// when the DB is absent or unreadable — callers fall back to the store's
/// `lastUsedModel`.
fn load_conversation_models(tracking_db: &Path) -> FastHashMap<String, String> {
    let mut map = FastHashMap::default();
    if !tracking_db.exists() {
        return map;
    }
    let loaded = open_readonly(tracking_db, "ai_code_hashes", |conn| {
        let mut stmt = conn.prepare(
            "SELECT conversationId, model, COUNT(*) AS c FROM ai_code_hashes \
             WHERE conversationId IS NOT NULL AND conversationId != '' \
               AND model IS NOT NULL AND model != '' \
             GROUP BY conversationId, model ORDER BY c DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut m: FastHashMap<String, String> = FastHashMap::default();
        for row in rows.flatten() {
            // Rows are ordered by descending line count, so the first model seen
            // for a conversation is its dominant one.
            m.entry(row.0).or_insert(row.1);
        }
        Ok(m)
    });
    if let Ok(m) = loaded {
        map = m;
    }
    map
}

/// Parses one chat store into per-(date) analysis records for its model.
fn read_store_analysis(
    store_db: &Path,
    conv_models: &FastHashMap<String, String>,
    time_range: TimeRange,
    mode: ParseMode,
    user: &str,
    machine: &str,
) -> Result<Vec<(String, CodeAnalysis)>> {
    let conv_id = conversation_id_from_path(store_db);
    let cutoff = cutoff_string(time_range);

    open_readonly(store_db, "blobs", |conn| {
        let model = resolve_store_model(conn, conv_models, &conv_id);
        let blobs = load_blobs(conn)?;

        // Pass 1: index Read tool results by tool-call id so their file lines can
        // be attributed to the matching Read call in pass 2.
        let read_contents = collect_read_results(&blobs);

        // Pass 2: fold each assistant turn's tool calls into a per-date state.
        let mut per_date: HashMap<String, SessionParseState> = HashMap::new();
        for data in &blobs {
            let Some((msg_bytes, ts)) = assistant_node(data) else {
                continue;
            };
            let Ok(msg) = serde_json::from_slice::<Value>(msg_bytes) else {
                continue;
            };
            // Only assistant turns carry tool calls. Guard before creating a
            // date bucket so a non-assistant node never emits a zero-metric row
            // or inflates the Cursor active-day count.
            if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let Some(date) = ms_to_local_date(ts) else {
                continue;
            };
            if is_before_cutoff(&date, &cutoff) {
                continue;
            }
            let state = per_date.entry(date).or_insert_with(|| {
                let mut s = SessionParseState::with_mode(mode);
                s.task_id = conv_id.clone();
                s
            });
            state.last_ts = ts.max(state.last_ts);
            apply_assistant_tools(state, &msg, &read_contents, ts);
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
        Ok(out)
    })
}

/// Reads a store's per-turn context-occupancy gauge for the usage approximation.
///
/// Returns the conversation's model plus `(timestamp_ms, context_tokens)` for
/// every assistant turn that carries the gauge.
fn read_store_context(
    store_db: &Path,
    conv_models: &FastHashMap<String, String>,
    conv_id: &str,
) -> Result<(String, Vec<(i64, i64)>)> {
    open_readonly(store_db, "blobs", |conn| {
        let model = resolve_store_model(conn, conv_models, conv_id);
        let blobs = load_blobs(conn)?;
        let mut turns = Vec::new();
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
            if !message_is_assistant(msg_bytes) {
                continue;
            }
            if let Some(ctx) = context_tokens(ctx_msg) {
                turns.push((ts, ctx));
            }
        }
        Ok((model, turns))
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
    let mut stmt = conn.prepare("SELECT data FROM blobs")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    Ok(rows.flatten().collect())
}

/// Returns `(assistant message JSON bytes, timestamp_ms)` for a binary DAG node.
///
/// Binary nodes start with `field 1` (`0x0A`) and embed exactly one assistant
/// message in `field 4`; `field 26` is the epoch-ms timestamp. Non-node blobs
/// (JSON messages) return `None`, as do nodes missing the timestamp — an
/// undateable turn is skipped rather than mis-bucketed to the epoch (1970).
fn assistant_node(data: &[u8]) -> Option<(&[u8], i64)> {
    if data.first() != Some(&0x0A) {
        return None;
    }
    let node = walk_node(data);
    Some((node.msg?, node.ts?))
}

/// Whether a message JSON blob is an assistant turn.
fn message_is_assistant(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|m| {
            m.get("role")
                .and_then(|v| v.as_str())
                .map(|role| role == "assistant")
        })
        .unwrap_or(false)
}

/// Applies one assistant message's tool calls to `state`.
fn apply_assistant_tools(
    state: &mut SessionParseState,
    msg: &Value,
    read_contents: &HashMap<String, String>,
    ts: i64,
) {
    if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return;
    }
    let Some(content) = msg.get("content").and_then(|v| v.as_array()) else {
        return;
    };
    for c in content {
        if c.get("type").and_then(|v| v.as_str()) != Some("tool-call") {
            continue;
        }
        let tool = c.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
        let args = c.get("args");
        let arg = |key: &str| -> &str {
            args.and_then(|a| a.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
        };
        match tool {
            "Write" => state.add_write_detail(arg("path"), arg("contents"), ts),
            "StrReplace" => {
                state.add_edit_detail(arg("path"), arg("old_string"), arg("new_string"), ts)
            }
            "Read" => {
                let path = arg("path");
                let content = c
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .and_then(|id| read_contents.get(id))
                    .map(String::as_str)
                    .unwrap_or("");
                if !path.is_empty() && !content.is_empty() {
                    state.add_read_detail(path, content, ts);
                } else {
                    // A read whose result we could not recover still counts as a
                    // read invocation, matching the OpenCode reader.
                    state.tool_counts.read += 1;
                }
            }
            "Shell" => state.add_run_command(arg("command"), arg("description"), ts),
            "TodoWrite" => state.tool_counts.todo_write += 1,
            // Grep / Glob / Delete etc. are not part of the tracked tool set.
            _ => {}
        }
    }
}

/// Indexes `Read` tool results by tool-call id, with line-number prefixes
/// stripped so the recovered content is the file's own lines.
fn collect_read_results(blobs: &[Vec<u8>]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for data in blobs {
        if data.first() != Some(&b'{') {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(data) else {
            continue;
        };
        if msg.get("role").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        let Some(content) = msg.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in content {
            if c.get("type").and_then(|v| v.as_str()) != Some("tool-result") {
                continue;
            }
            if c.get("toolName").and_then(|v| v.as_str()) != Some("Read") {
                continue;
            }
            let (Some(id), Some(result)) = (
                c.get("toolCallId").and_then(|v| v.as_str()),
                c.get("result").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            map.insert(id.to_string(), strip_cursor_line_numbers(result));
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
fn cursor_usage_value(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_input_tokens": cache_read,
        "cache_creation_input_tokens": cache_write,
    })
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

/// Reads a JSON number *or* numeric string as `i64` (the dashboard API returns
/// token counts as strings on some endpoints).
fn as_i64_lenient(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

/// Reads a JSON number *or* numeric string as `f64`.
fn as_f64_lenient(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
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

/// Opens a Cursor SQLite DB read-only, falling back to a temp copy (with WAL
/// sidecars) if the read-only open cannot see the data.
///
/// `probe` is a table known to exist, used to confirm the connection works.
/// Mirrors the OpenCode reader's `with_connection`, generalized over the probe.
fn open_readonly<T>(
    db_path: &Path,
    probe: &str,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let probe_sql = format!("SELECT count(*) FROM {probe}");
    if let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        && conn.query_row(&probe_sql, [], |_| Ok(())).is_ok()
    {
        return f(&conn);
    }

    let copy = TempDbCopy::new(db_path)?;
    let conn = Connection::open_with_flags(&copy.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
            format!(
                "Failed to open Cursor DB copy at {}",
                copy.db_path.display()
            )
        })?;
    f(&conn)
}

/// A private temp-directory copy of a Cursor DB (plus WAL sidecars), removed on
/// drop. The temp dir has owner-only permissions so the chat data is never
/// exposed to other local users.
struct TempDbCopy {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
}

impl TempDbCopy {
    fn new(src: &Path) -> Result<Self> {
        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid Cursor DB path: {}", src.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("vct-cursor-")
            .tempdir()
            .context("Failed to create temp dir for Cursor DB copy")?;
        let db_path = dir.path().join(file_name);
        std::fs::copy(src, &db_path)
            .with_context(|| format!("Failed to copy Cursor DB from {}", src.display()))?;
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

    #[test]
    fn lenient_number_parsing() {
        assert_eq!(as_i64_lenient(&json!(42)), Some(42));
        assert_eq!(as_i64_lenient(&json!("42")), Some(42));
        assert_eq!(as_i64_lenient(&json!("  17 ")), Some(17));
        assert_eq!(as_i64_lenient(&json!("nope")), None);
        assert_eq!(as_f64_lenient(&json!("3.5")), Some(3.5));
    }

    #[test]
    fn event_cost_prefers_token_total_cents() {
        let ev = json!({"tokenUsage": {"totalCents": 250}, "chargedCents": 999});
        assert!((event_cost_usd(&ev) - 2.5).abs() < 1e-9);
        let ev2 = json!({"chargedCents": "120"});
        assert!((event_cost_usd(&ev2) - 1.2).abs() < 1e-9);
        assert_eq!(event_cost_usd(&json!({})), 0.0);
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
            ]
        });
        let mut reads = HashMap::new();
        reads.insert("e".to_string(), "r1\nr2\nr3".to_string());
        apply_assistant_tools(&mut state, &msg, &reads, 42);

        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.tool_counts.todo_write, 1);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_write_lines, 2);
        assert_eq!(state.total_edit_lines, 2);
        assert_eq!(state.total_read_lines, 3);
    }

    #[test]
    fn aggregate_events_filters_and_builds_records() {
        let events = vec![
            CachedEvent {
                date: "2999-01-01".to_string(),
                model: "claude-sonnet-4.6".to_string(),
                input: 100,
                output: 20,
                cache_read: 50,
                cache_write: 10,
                cost: 1.5,
            },
            CachedEvent {
                date: "2000-01-01".to_string(),
                model: "composer-2".to_string(),
                input: 5,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                cost: 0.0,
            },
        ];
        // Daily cutoff drops the ancient 2000 row but keeps the far-future one.
        let rows = aggregate_events(&events, TimeRange::Daily, "u", "m");
        assert_eq!(rows.len(), 1);
        let (date, analysis, cost) = &rows[0];
        assert_eq!(date, "2999-01-01");
        assert!((cost - 1.5).abs() < 1e-9);
        assert!(
            analysis.records[0]
                .conversation_usage
                .contains_key("claude-sonnet-4.6")
        );
    }

    /// Builds a temp `store.db` with the given binary nodes and JSON blobs.
    fn make_store_db(nodes: &[Vec<u8>], json_blobs: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let conn = Connection::open(&path).unwrap();
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
        (dir, path)
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
        let (_dir, path) = make_store_db(&[assistant, user_node], &[tool_result]);

        let conv_models = FastHashMap::default();
        let rows = read_store_analysis(
            &path,
            &conv_models,
            TimeRange::All,
            ParseMode::Full,
            "u",
            "m",
        )
        .unwrap();

        // Exactly one assistant turn -> one (date) record; the user node is dropped.
        assert_eq!(rows.len(), 1);
        let rec = &rows[0].1.records[0];
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
        let (model, mut turns) = read_store_context(&path, &conv_models, "conv").unwrap();
        turns.sort();
        assert_eq!(model, "unknown");
        assert_eq!(
            turns,
            vec![(1_700_000_000_000, 42_000), (1_700_000_500_000, 88_000)]
        );
    }
}
