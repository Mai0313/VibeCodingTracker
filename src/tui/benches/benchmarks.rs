use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use ratatui::{Terminal, backend::TestBackend};
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use vct_core::analysis::aggregate_sessions_by_model_from_paths_with_cache;
use vct_core::config::ProvidersConfig;
use vct_core::constants::FastHashMap;
use vct_core::ledger::SessionLedger;
use vct_core::models::usage::UsageResult;
use vct_core::models::{ExtensionType, PerProviderUsage, ProviderActiveDays, TimeRange};
use vct_core::pricing::{ModelPricing, ModelPricingMap, clear_pricing_cache, normalize_model_name};
use vct_core::scan::build_scan_pool;
use vct_core::session::{
    ParseMode, parse_session_file_typed_as, parse_session_file_typed_with_mode,
    read_cursor_analysis, read_cursor_usage, read_hermes_usage, read_opencode_analysis,
    read_opencode_usage,
};
use vct_core::usage::StoredCosts;
use vct_core::usage::aggregator::aggregate_usage_from_paths_with_cache;
use vct_core::usage::summary::{build_usage_summary, merge_rows_by_base_model};
use vct_core::utils::{HelperPaths, resolve_paths_from_home};
use vct_tui::bench_support::{LayoutFrameBenchmark, init_settings_ui, layout_ids};
use vct_tui::display::app::Tab;
use vct_tui::display::common::render_loading;

/// Returns the absolute path to a file under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Builds a synthetic LiteLLM-shaped pricing catalog with ~`target_entries` models across realistic vendor prefixes and dated variants.
fn build_synthetic_pricing_map(target_entries: usize) -> ModelPricingMap {
    const VENDORS: [&str; 10] = [
        "",
        "anthropic/",
        "openai/",
        "vertex_ai/",
        "bedrock/",
        "azure/",
        "openrouter/",
        "deepseek/",
        "xai/",
        "together_ai/",
    ];
    const FAMILIES: [&str; 10] = [
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
        "gpt-5.4",
        "gpt-5.4-codex",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "deepseek-reasoner",
        "grok-code-fast-1",
        "qwen3-coder",
    ];
    const DATES: [&str; 4] = ["", "-20250929", "-20260115", "-20260401"];

    let mut raw = HashMap::with_capacity(target_entries + 32);

    // Canonical bare and prefixed anchor models used by lookup and summary benchmarks.
    for (idx, family) in FAMILIES.iter().enumerate() {
        let base_in = 1.0e-6 * ((idx + 1) as f64);
        let pricing = ModelPricing {
            input_cost_per_token: base_in,
            output_cost_per_token: base_in * 4.0,
            cache_read_input_token_cost: base_in * 0.1,
            cache_creation_input_token_cost: base_in * 1.25,
            cache_creation_input_token_cost_above_1hr: base_in * 2.0,
            output_cost_per_reasoning_token: base_in * 4.0,
            ..ModelPricing::default()
        };
        raw.insert((*family).to_string(), pricing.clone());
        for vendor in &VENDORS[1..] {
            raw.insert(format!("{vendor}{family}"), pricing.clone());
        }
    }

    // Fill out the remaining catalog up to `target_entries` with distinct vendor/family/date/tier keys.
    let mut seq = 0usize;
    while raw.len() < target_entries {
        let vendor = VENDORS[seq % VENDORS.len()];
        let family = FAMILIES[(seq / VENDORS.len()) % FAMILIES.len()];
        let date = DATES[(seq / (VENDORS.len() * FAMILIES.len())) % DATES.len()];
        let variant = seq / (VENDORS.len() * FAMILIES.len() * DATES.len());
        let key = format!("{vendor}{family}-r{variant:02}{date}");
        let base_in = 0.5e-6 + ((seq % 20) as f64) * 0.25e-6;
        raw.entry(key).or_insert_with(|| ModelPricing {
            input_cost_per_token: base_in,
            output_cost_per_token: base_in * 3.5,
            cache_read_input_token_cost: base_in * 0.1,
            cache_creation_input_token_cost: base_in * 1.25,
            ..ModelPricing::default()
        });
        seq += 1;
    }

    assert!(raw.len() >= target_entries);
    ModelPricingMap::new(raw)
}

// ========== Pricing & String Operations ==========

fn benchmark_normalize_model_name(c: &mut Criterion) {
    c.bench_function("normalize_model_name simple", |b| {
        b.iter(|| normalize_model_name(black_box("claude-3-sonnet-20240229")))
    });

    c.bench_function("normalize_model_name with prefix", |b| {
        b.iter(|| normalize_model_name(black_box("bedrock/claude-3-opus-20240229")))
    });

    c.bench_function("normalize_model_name complex", |b| {
        b.iter(|| normalize_model_name(black_box("openai/gpt-4-turbo-20240409-v1.5")))
    });
}

fn benchmark_pricing_lookup(c: &mut Criterion) {
    let pricing_map = build_synthetic_pricing_map(2_000);
    assert!(!pricing_map.is_empty());

    // Warm lazy secondary indices once and assert each benchmark query hits its intended matching stage.
    let exact = pricing_map.get("claude-sonnet-4-6");
    assert!(exact.matched_model.is_none() && exact.pricing.input_cost_per_token > 0.0);
    let normalized = pricing_map.get("bedrock/claude-sonnet-4-6-20250929");
    assert!(normalized.matched_model.is_some() && normalized.pricing.input_cost_per_token > 0.0);
    let substring = pricing_map.get("claude-sonnet-4-6-thinking-preview");
    assert!(substring.matched_model.is_some() && substring.pricing.input_cost_per_token > 0.0);
    let miss = pricing_map.get("completely-unknown-synthetic-xyz-999");
    assert!(miss.matched_model.is_none() && miss.pricing.input_cost_per_token == 0.0);

    let mut group = c.benchmark_group("pricing_lookup_2000");
    group.sample_size(20);

    group.bench_function("exact", |b| {
        b.iter(|| black_box(pricing_map.get(black_box("claude-sonnet-4-6"))))
    });

    group.bench_function("normalized_uncached", |b| {
        b.iter_batched(
            clear_pricing_cache,
            |()| black_box(pricing_map.get(black_box("bedrock/claude-sonnet-4-6-20250929"))),
            BatchSize::PerIteration,
        )
    });

    group.bench_function("prefix_substring_uncached", |b| {
        b.iter_batched(
            clear_pricing_cache,
            |()| black_box(pricing_map.get(black_box("claude-sonnet-4-6-thinking-preview"))),
            BatchSize::PerIteration,
        )
    });

    group.bench_function("fuzzy_miss_uncached", |b| {
        b.iter_batched(
            clear_pricing_cache,
            |()| black_box(pricing_map.get(black_box("completely-unknown-synthetic-xyz-999"))),
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

fn benchmark_line_counting(c: &mut Criterion) {
    let short_text = "line1\nline2\nline3\n";
    let medium_text = (0..100).map(|i| format!("line{}\n", i)).collect::<String>();
    let long_text = (0..10000)
        .map(|i| format!("line{}\n", i))
        .collect::<String>();

    c.bench_function("count_lines short (3 lines)", |b| {
        b.iter(|| vct_core::utils::count_lines(black_box(short_text)))
    });

    c.bench_function("count_lines medium (100 lines)", |b| {
        b.iter(|| vct_core::utils::count_lines(black_box(&medium_text)))
    });

    c.bench_function("count_lines long (10k lines)", |b| {
        b.iter(|| vct_core::utils::count_lines(black_box(&long_text)))
    });
}

// ========== File Parsing Benchmarks ==========

fn benchmark_file_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_parsing");

    let test_files = vec![
        ("claude", "sessions/claude_code.jsonl"),
        ("codex", "sessions/codex.jsonl"),
        ("copilot", "sessions/copilot.jsonl"),
        ("gemini", "sessions/gemini.jsonl"),
        ("grok", "sessions/grok/signals.json"),
        ("dsh", "sessions/dsh/session.jsonl.zstd"),
    ];

    for (name, path) in test_files {
        let path_buf = fixture(path);
        if path_buf.exists() {
            group.bench_with_input(
                BenchmarkId::new("parse_session_file_to_value", name),
                &path_buf,
                |b, p| b.iter(|| vct_core::session::parse_session_file_to_value(black_box(p))),
            );
        }
    }

    group.finish();
}

fn benchmark_known_provider_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("known_provider_parse");
    let fixtures = [
        (
            "claude",
            "sessions/claude_code.jsonl",
            ExtensionType::ClaudeCode,
        ),
        ("codex", "sessions/codex.jsonl", ExtensionType::Codex),
        ("copilot", "sessions/copilot.jsonl", ExtensionType::Copilot),
        ("gemini", "sessions/gemini.jsonl", ExtensionType::Gemini),
        ("grok", "sessions/grok/signals.json", ExtensionType::Grok),
        (
            "dsh",
            "sessions/dsh/session.jsonl.zstd",
            ExtensionType::DeepSeek,
        ),
    ];

    for (name, path, provider) in fixtures {
        let path = fixture(path);
        if !path.exists() {
            continue;
        }
        for (mode_name, mode) in [
            ("usage_only", ParseMode::UsageOnly),
            ("full", ParseMode::Full),
        ] {
            group.bench_function(BenchmarkId::new(name, mode_name), |b| {
                b.iter(|| {
                    black_box(
                        parse_session_file_typed_as(black_box(&path), provider, mode)
                            .expect("parse known-provider benchmark fixture"),
                    )
                })
            });
        }
    }

    group.finish();
}

fn benchmark_long_preamble_detection(c: &mut Criterion) {
    let temp = TempDir::new().expect("create long-preamble benchmark directory");
    let path = temp.path().join("session.jsonl");
    let sentinel = r#"{"type":"file-history-snapshot","messageId":"m1","isSnapshotUpdate":false,"snapshot":{}}"#;
    let assistant = r#"{"type":"assistant","sessionId":"sess-1","parentUuid":"pu","timestamp":"2026-04-23T00:00:00.000Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50},"content":[]}}"#;
    let mut body = String::with_capacity((sentinel.len() + 1) * 10_000 + assistant.len() + 1);
    for _ in 0..10_000 {
        body.push_str(sentinel);
        body.push('\n');
    }
    body.push_str(assistant);
    body.push('\n');
    std::fs::write(&path, body).expect("write long-preamble benchmark fixture");

    let mut group = c.benchmark_group("single_file_detection");
    group.sample_size(10);
    group.bench_function("claude_after_10000_metadata_records", |b| {
        b.iter(|| {
            black_box(
                parse_session_file_typed_with_mode(black_box(&path), ParseMode::UsageOnly)
                    .expect("parse long-preamble benchmark fixture"),
            )
        })
    });
    group.finish();
}

fn claude_only() -> ProvidersConfig {
    ProvidersConfig {
        claude: true,
        codex: false,
        copilot: false,
        gemini: false,
        opencode: false,
        cursor: false,
        hermes: false,
        grok: false,
        dsh: false,
        antigravity: false,
    }
}

fn build_scan_corpus(source_count: usize, fixture: &str) -> (TempDir, HelperPaths, PathBuf) {
    let temp = TempDir::new().expect("create scan benchmark directory");
    let paths = resolve_paths_from_home(temp.path());
    let project = paths.claude_session_dir.join("benchmark");
    std::fs::create_dir_all(&project).expect("create scan benchmark project");
    for index in 0..source_count {
        std::fs::write(project.join(format!("session-{index}.jsonl")), fixture)
            .expect("write scan benchmark fixture");
    }
    let changed = project.join("session-0.jsonl");
    (temp, paths, changed)
}

fn run_cached_scan(pool: &rayon::ThreadPool, paths: &HelperPaths, cache: &mut SessionLedger) {
    let result = pool.install(|| {
        aggregate_usage_from_paths_with_cache(paths, TimeRange::All, claude_only(), cache)
    });
    black_box(result.expect("scan benchmark corpus"));
}

fn benchmark_summary_scan_cache(c: &mut Criterion) {
    let fixture = std::fs::read_to_string(fixture("sessions/claude_code.jsonl"))
        .expect("read scan benchmark fixture");
    let (_small_temp, small_paths, changed_path) = build_scan_corpus(100, &fixture);
    let (_large_temp, large_paths, _) = build_scan_corpus(1_000, &fixture);
    let pool = build_scan_pool(2).expect("build scan benchmark pool");

    let mut group = c.benchmark_group("summary_scan_cache");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function(BenchmarkId::new("cold", 100), |b| {
        b.iter_batched(
            SessionLedger::new,
            |mut cache| run_cached_scan(&pool, &small_paths, &mut cache),
            BatchSize::SmallInput,
        );
    });
    group.bench_function(BenchmarkId::new("cold", 1_000), |b| {
        b.iter_batched(
            SessionLedger::new,
            |mut cache| run_cached_scan(&pool, &large_paths, &mut cache),
            BatchSize::SmallInput,
        );
    });

    let mut warm_cache = SessionLedger::new();
    run_cached_scan(&pool, &small_paths, &mut warm_cache);
    group.bench_function(BenchmarkId::new("unchanged", 100), |b| {
        b.iter(|| {
            run_cached_scan(&pool, &small_paths, &mut warm_cache);
            debug_assert_eq!(warm_cache.stats().parsed_sources, 0);
        });
    });

    group.bench_function(BenchmarkId::new("single_changed", 100), |b| {
        b.iter_custom(|iterations| {
            std::fs::write(&changed_path, &fixture).expect("reset changed benchmark fixture");
            let mut cache = SessionLedger::new();
            run_cached_scan(&pool, &small_paths, &mut cache);
            let fixture_with_newline = format!("{fixture}\n");
            let mut elapsed = Duration::ZERO;
            for iteration in 0..iterations {
                let content = if iteration % 2 == 0 {
                    fixture_with_newline.as_str()
                } else {
                    fixture.as_str()
                };
                std::fs::write(&changed_path, content).expect("mutate changed benchmark fixture");
                let started = Instant::now();
                run_cached_scan(&pool, &small_paths, &mut cache);
                elapsed += started.elapsed();
                debug_assert_eq!(cache.stats().parsed_sources, 1);
            }
            elapsed
        });
    });

    group.finish();
}

// ========== SQLite Provider Benchmarks ==========

fn build_synthetic_opencode_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("opencode.db");
    let conn = Connection::open(&db_path).expect("create synthetic OpenCode db");
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
    .expect("create synthetic OpenCode schema");

    const MODELS: [(&str, &str); 4] = [
        ("anthropic", "claude-sonnet-4-6"),
        ("openai", "gpt-5.4"),
        ("deepseek", "deepseek-reasoner"),
        ("google", "gemini-2.5-pro"),
    ];
    let base_ts: i64 = 1_780_757_089_000;

    let tx = conn.unchecked_transaction().expect("begin OpenCode tx");
    for s in 0..40usize {
        let session_id = format!("sess-{s:03}");
        let ts = base_ts + (s as i64) * 60_000;
        tx.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES (?1, '/repo', ?2)",
            rusqlite::params![session_id, ts],
        )
        .expect("insert OpenCode session");

        for m in 0..5usize {
            let msg_id = format!("msg-{s:03}-{m:02}");
            let (provider, model) = MODELS[(s + m) % MODELS.len()];
            let msg_ts = ts + (m as i64) * 5_000;
            let msg_json = json!({
                "role": "assistant",
                "providerID": provider,
                "modelID": model,
                "cost": 0.0125,
                "tokens": {
                    "input": 2_400 + m * 100,
                    "output": 450 + m * 25,
                    "reasoning": 120,
                    "cache": { "read": 12_000, "write": 600 },
                },
                "time": { "created": msg_ts - 1_000, "completed": msg_ts },
            })
            .to_string();
            tx.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![msg_id, session_id, msg_ts - 1_000, msg_ts, msg_json],
            )
            .expect("insert OpenCode message");

            for p in 0..2usize {
                let part_id = format!("part-{s:03}-{m:02}-{p}");
                let part_json = match (m + p) % 4 {
                    0 => json!({
                        "type": "tool",
                        "tool": "read",
                        "state": {
                            "status": "completed",
                            "input": { "filePath": "/repo/src/lib.rs" },
                            "output": "<content>\n1: pub fn sample() {}\n2: pub fn other() {}\n</content>",
                        }
                    }),
                    1 => json!({
                        "type": "tool",
                        "tool": "edit",
                        "state": {
                            "status": "completed",
                            "input": {
                                "filePath": "/repo/src/lib.rs",
                                "oldString": "pub fn sample() {}",
                                "newString": "pub fn sample() -> usize {\n    42\n}",
                            }
                        }
                    }),
                    2 => json!({
                        "type": "tool",
                        "tool": "write",
                        "state": {
                            "status": "completed",
                            "input": {
                                "filePath": "/repo/src/generated.rs",
                                "content": "pub const COUNT: usize = 10;\npub fn value() -> usize { COUNT }\n",
                            }
                        }
                    }),
                    _ => json!({
                        "type": "tool",
                        "tool": "bash",
                        "state": {
                            "status": "completed",
                            "input": {
                                "command": "cargo test --workspace",
                                "description": "run unit tests",
                            }
                        }
                    }),
                }
                .to_string();
                tx.execute(
                    "INSERT INTO part (id, message_id, session_id, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![part_id, msg_id, session_id, msg_ts, part_json],
                )
                .expect("insert OpenCode part");
            }
        }
    }
    tx.commit().expect("commit OpenCode db");
    db_path
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn encode_cursor_dag_node(message: &str, context_tokens: i64, timestamp_ms: i64) -> Vec<u8> {
    let mut node = Vec::with_capacity(message.len() + 32);
    // field 1 (wire 2): parent/root header
    encode_varint((1 << 3) | 2, &mut node);
    encode_varint(1, &mut node);
    node.push(0);
    // field 4 (wire 2): assistant message JSON
    encode_varint((4 << 3) | 2, &mut node);
    encode_varint(message.len() as u64, &mut node);
    node.extend_from_slice(message.as_bytes());
    // field 5 (wire 2): running context-window gauge
    let mut context = Vec::new();
    encode_varint(1 << 3, &mut context);
    encode_varint(context_tokens as u64, &mut context);
    encode_varint((5 << 3) | 2, &mut node);
    encode_varint(context.len() as u64, &mut node);
    node.extend_from_slice(&context);
    // field 26 (wire 0): epoch-ms timestamp
    encode_varint(26 << 3, &mut node);
    encode_varint(timestamp_ms as u64, &mut node);
    node
}

fn build_synthetic_cursor_stores(dir: &Path) -> (PathBuf, PathBuf) {
    let chats_dir = dir.join("chats");
    let tracking_db = dir.join("ai-code-tracking.db");
    let tracking = Connection::open(&tracking_db).expect("create synthetic Cursor tracking db");
    tracking
        .execute_batch("CREATE TABLE ai_code_hashes (conversationId TEXT, model TEXT);")
        .expect("create Cursor tracking schema");

    const MODELS: [&str; 4] = [
        "claude-sonnet-4-6",
        "gpt-5.4",
        "gemini-2.5-pro",
        "grok-code-fast-1",
    ];
    let assistant_msg = r#"{"role":"assistant","content":[{"type":"tool-call","toolName":"Write","toolCallId":"tc-w","args":{"path":"/repo/out.rs","contents":"line1\nline2\nline3"}},{"type":"tool-call","toolName":"StrReplace","toolCallId":"tc-e","args":{"path":"/repo/out.rs","old_string":"line1","new_string":"updated1\nupdated2"}},{"type":"tool-call","toolName":"Shell","toolCallId":"tc-s","args":{"command":"cargo check"}}]}"#;
    let base_ts: i64 = 1_780_757_089_000;

    for idx in 0..20usize {
        let conv_id = format!("conv-{idx:03}");
        let model = MODELS[idx % MODELS.len()];
        tracking
            .execute(
                "INSERT INTO ai_code_hashes (conversationId, model) VALUES (?1, ?2)",
                rusqlite::params![conv_id, model],
            )
            .expect("insert Cursor model mapping");

        let store_path = chats_dir
            .join(format!("proj-{:02}", idx % 4))
            .join(&conv_id)
            .join("store.db");
        std::fs::create_dir_all(store_path.parent().expect("Cursor store parent"))
            .expect("create Cursor store dir");
        let conn = Connection::open(&store_path).expect("create synthetic Cursor store");
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
        )
        .expect("create Cursor store schema");

        let tx = conn.unchecked_transaction().expect("begin Cursor store tx");
        for turn in 0..10usize {
            let node = encode_cursor_dag_node(
                assistant_msg,
                8_000 + (turn as i64) * 2_500,
                base_ts + (idx as i64) * 60_000 + (turn as i64) * 2_000,
            );
            tx.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("node-{turn:02}"), node],
            )
            .expect("insert Cursor DAG blob");
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('metadata', ?1)",
            rusqlite::params![json!({ "lastUsedModel": model }).to_string()],
        )
        .expect("insert Cursor meta");
        tx.commit().expect("commit Cursor store");
    }

    (chats_dir, tracking_db)
}

fn build_synthetic_hermes_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("state.db");
    let conn = Connection::open(&db_path).expect("create synthetic Hermes db");
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
    .expect("create Hermes schema");

    const MODELS: [&str; 4] = [
        "qwen3-coder",
        "claude-sonnet-4-6",
        "gpt-5.4",
        "deepseek-reasoner",
    ];
    let base_ts = 1_780_757_089.0_f64;

    let tx = conn.unchecked_transaction().expect("begin Hermes tx");
    for s in 0..80usize {
        let session_id = format!("hermes-sess-{s:03}");
        let ts = base_ts + (s as f64) * 120.0;
        let primary_model = MODELS[s % MODELS.len()];
        // Include a small residual above per-model sums so reconciliation exercises both paths.
        tx.execute(
            "INSERT INTO sessions (id, model, billing_provider, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, estimated_cost_usd, actual_cost_usd, started_at, ended_at)
             VALUES (?1, ?2, 'openrouter', 20000, 5000, 30000, 2000, 1000, 0.12, 0.11, ?3, ?3)",
            rusqlite::params![session_id, primary_model, ts],
        )
        .expect("insert Hermes session");

        for m in 0..3usize {
            let model = MODELS[(s + m) % MODELS.len()];
            tx.execute(
                "INSERT INTO session_model_usage (session_id, model, billing_provider, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, estimated_cost_usd, actual_cost_usd, first_seen, last_seen)
                 VALUES (?1, ?2, 'openrouter', 6000, 1500, 9000, 600, 300, 0.035, 0.032, ?3, ?3)",
                rusqlite::params![session_id, model, ts],
            )
            .expect("insert Hermes model usage");
        }
    }
    tx.commit().expect("commit Hermes db");
    db_path
}

fn benchmark_sqlite_providers(c: &mut Criterion) {
    let temp = TempDir::new().expect("create SQLite benchmark directory");
    let opencode_db = build_synthetic_opencode_db(temp.path());
    let (cursor_chats, cursor_tracking) = build_synthetic_cursor_stores(temp.path());
    let hermes_db = build_synthetic_hermes_db(temp.path());

    // Sanity-check synthetic fixtures decode non-empty rows before timing.
    assert!(
        !read_opencode_usage(&opencode_db, TimeRange::All)
            .expect("verify synthetic OpenCode usage")
            .is_empty()
    );
    assert!(
        !read_opencode_analysis(&opencode_db, TimeRange::All, ParseMode::Full)
            .expect("verify synthetic OpenCode analysis")
            .is_empty()
    );
    assert!(
        !read_cursor_usage(&cursor_chats, &cursor_tracking, TimeRange::All)
            .expect("verify synthetic Cursor usage")
            .is_empty()
    );
    assert!(
        !read_cursor_analysis(
            &cursor_chats,
            &cursor_tracking,
            TimeRange::All,
            ParseMode::Full
        )
        .expect("verify synthetic Cursor analysis")
        .is_empty()
    );
    assert!(
        !read_hermes_usage(&hermes_db, TimeRange::All)
            .expect("verify synthetic Hermes usage")
            .is_empty()
    );

    let mut group = c.benchmark_group("sqlite_providers");
    group.sample_size(15);

    group.bench_function("opencode_usage", |b| {
        b.iter(|| {
            black_box(
                read_opencode_usage(black_box(&opencode_db), TimeRange::All)
                    .expect("read OpenCode usage"),
            )
        })
    });

    group.bench_function("opencode_analysis_usage_only", |b| {
        b.iter(|| {
            black_box(
                read_opencode_analysis(
                    black_box(&opencode_db),
                    TimeRange::All,
                    ParseMode::UsageOnly,
                )
                .expect("read OpenCode analysis (usage_only)"),
            )
        })
    });

    group.bench_function("opencode_analysis_full", |b| {
        b.iter(|| {
            black_box(
                read_opencode_analysis(black_box(&opencode_db), TimeRange::All, ParseMode::Full)
                    .expect("read OpenCode analysis (full)"),
            )
        })
    });

    group.bench_function("cursor_usage", |b| {
        b.iter(|| {
            black_box(
                read_cursor_usage(
                    black_box(&cursor_chats),
                    black_box(&cursor_tracking),
                    TimeRange::All,
                )
                .expect("read Cursor usage"),
            )
        })
    });

    group.bench_function("cursor_analysis_full", |b| {
        b.iter(|| {
            black_box(
                read_cursor_analysis(
                    black_box(&cursor_chats),
                    black_box(&cursor_tracking),
                    TimeRange::All,
                    ParseMode::Full,
                )
                .expect("read Cursor analysis"),
            )
        })
    });

    group.bench_function("hermes_usage", |b| {
        b.iter(|| {
            black_box(
                read_hermes_usage(black_box(&hermes_db), TimeRange::All)
                    .expect("read Hermes usage"),
            )
        })
    });

    group.finish();
}

// ========== Session Ledger Lifecycle Benchmarks ==========

fn build_diverse_claude_corpus(source_count: usize) -> (TempDir, HelperPaths, PathBuf) {
    const MODELS: [&str; 5] = [
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
        "claude-sonnet-4-5-20250929",
        "claude-opus-4-5-20251101",
    ];
    let temp = TempDir::new().expect("create ledger lifecycle benchmark directory");
    let paths = resolve_paths_from_home(temp.path());
    let project = paths.claude_session_dir.join("ledger-bench");
    std::fs::create_dir_all(&project).expect("create ledger benchmark project");

    for index in 0..source_count {
        let model = MODELS[index % MODELS.len()];
        let day = (index % 28) + 1;
        let line = format!(
            r#"{{"type":"assistant","sessionId":"ledger-sess-{index:04}","parentUuid":"p-{index}","timestamp":"2026-04-{day:02}T12:00:00.000Z","message":{{"model":"{model}","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":{cache_create}}},"content":[{{"type":"tool_use","id":"tu-{index}","name":"Edit","input":{{"file_path":"/repo/src/mod_{index}.rs","old_string":"old","new_string":"new\nline"}}}}]}}}}"#,
            input = 1_500 + (index % 10) * 200,
            output = 350 + (index % 7) * 50,
            cache_read = 12_000 + (index % 5) * 1_000,
            cache_create = 400 + (index % 4) * 100,
        );
        std::fs::write(project.join(format!("session-{index:04}.jsonl")), line)
            .expect("write ledger benchmark session");
    }

    let first_session = project.join("session-0000.jsonl");
    (temp, paths, first_session)
}

fn benchmark_ledger_lifecycle(c: &mut Criterion) {
    let (_temp, paths, first_session) = build_diverse_claude_corpus(300);
    let pool = build_scan_pool(2).expect("build ledger lifecycle pool");

    // Populate both usage and analysis halves on disk so `open`, `save`, and folds reflect real state.
    let mut seed_ledger = SessionLedger::open(&paths.cache_dir);
    pool.install(|| {
        aggregate_usage_from_paths_with_cache(
            &paths,
            TimeRange::All,
            claude_only(),
            &mut seed_ledger,
        )
        .expect("seed usage ledger");
        aggregate_sessions_by_model_from_paths_with_cache(
            &paths,
            TimeRange::All,
            claude_only(),
            &mut seed_ledger,
        )
        .expect("seed analysis ledger");
    });
    seed_ledger.save().expect("save seeded benchmark ledger");
    assert_eq!(seed_ledger.stats().entries, 300);

    let mut group = c.benchmark_group("ledger_lifecycle_300");
    group.sample_size(15);

    group.bench_function("open", |b| {
        b.iter(|| {
            let ledger = SessionLedger::open(black_box(&paths.cache_dir));
            debug_assert_eq!(ledger.stats().entries, 300);
            black_box(ledger)
        })
    });

    let base_first =
        std::fs::read_to_string(&first_session).expect("read first ledger benchmark session");
    let alt_first = format!("{base_first}\n");
    let mut flip = false;
    group.bench_function("save", |b| {
        b.iter_batched(
            || {
                flip = !flip;
                let body = if flip {
                    alt_first.as_str()
                } else {
                    base_first.as_str()
                };
                std::fs::write(&first_session, body).expect("dirty one ledger source");
                let mut ledger = SessionLedger::open(&paths.cache_dir);
                pool.install(|| {
                    aggregate_usage_from_paths_with_cache(
                        &paths,
                        TimeRange::All,
                        claude_only(),
                        &mut ledger,
                    )
                    .expect("scan dirty ledger source");
                });
                debug_assert_eq!(ledger.stats().parsed_sources, 1);
                ledger
            },
            |mut ledger| {
                ledger.save().expect("save dirty benchmark ledger");
                black_box(ledger)
            },
            BatchSize::PerIteration,
        )
    });

    let mut warm_ledger = SessionLedger::open(&paths.cache_dir);
    pool.install(|| {
        aggregate_usage_from_paths_with_cache(
            &paths,
            TimeRange::All,
            claude_only(),
            &mut warm_ledger,
        )
        .expect("prime warm usage ledger");
        aggregate_sessions_by_model_from_paths_with_cache(
            &paths,
            TimeRange::All,
            claude_only(),
            &mut warm_ledger,
        )
        .expect("prime warm analysis ledger");
    });

    group.bench_function("fold_usage", |b| {
        b.iter(|| {
            let data = pool.install(|| {
                aggregate_usage_from_paths_with_cache(
                    black_box(&paths),
                    TimeRange::All,
                    claude_only(),
                    &mut warm_ledger,
                )
                .expect("fold warm usage ledger")
            });
            debug_assert_eq!(warm_ledger.stats().parsed_sources, 0);
            black_box(data)
        })
    });

    group.bench_function("fold_analysis", |b| {
        b.iter(|| {
            let data = pool.install(|| {
                aggregate_sessions_by_model_from_paths_with_cache(
                    black_box(&paths),
                    TimeRange::All,
                    claude_only(),
                    &mut warm_ledger,
                )
                .expect("fold warm analysis ledger")
            });
            debug_assert_eq!(warm_ledger.stats().parsed_sources, 0);
            black_box(data)
        })
    });

    group.finish();
}

// ========== Large-Scale Parsing & Summary Pipeline Benchmarks ==========

fn build_multi_mb_claude_jsonl(dir: &Path, turns: usize) -> PathBuf {
    const MODELS: [&str; 4] = [
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
        "claude-sonnet-4-5-20250929",
    ];
    let path = dir.join("large_session.jsonl");
    let filler = "x".repeat(480);
    let mut out = String::with_capacity(turns * 950);

    for idx in 0..turns {
        let model = MODELS[idx % MODELS.len()];
        let line = match idx % 3 {
            0 => format!(
                r#"{{"type":"assistant","sessionId":"large-sess","parentUuid":"p-{idx}","timestamp":"2026-04-20T10:00:00.000Z","message":{{"model":"{model}","usage":{{"input_tokens":{in_tok},"output_tokens":{out_tok},"cache_read_input_tokens":{cr},"cache_creation_input_tokens":{cc}}},"content":[{{"type":"text","text":"{filler}"}},{{"type":"tool_use","id":"tu-{idx}","name":"Edit","input":{{"file_path":"/repo/src/file_{f}.rs","old_string":"let a = 1;","new_string":"let a = 2;\nlet b = 3;"}}}}]}}}}"#,
                in_tok = 1_200 + (idx % 500),
                out_tok = 300 + (idx % 200),
                cr = 40_000 + (idx % 20_000),
                cc = 1_000 + (idx % 400),
                f = idx % 32,
            ),
            1 => format!(
                r#"{{"type":"assistant","sessionId":"large-sess","parentUuid":"p-{idx}","timestamp":"2026-04-20T10:01:00.000Z","message":{{"model":"{model}","usage":{{"input_tokens":{in_tok},"output_tokens":{out_tok},"cache_read_input_tokens":{cr},"cache_creation_input_tokens":0}},"content":[{{"type":"tool_use","id":"tu-{idx}","name":"Write","input":{{"file_path":"/repo/src/gen_{f}.rs","content":"// {filler}\npub fn generated() -> usize {{ 1 }}\n"}}}}]}}}}"#,
                in_tok = 900 + (idx % 400),
                out_tok = 250 + (idx % 150),
                cr = 35_000 + (idx % 15_000),
                f = idx % 32,
            ),
            _ => format!(
                r#"{{"type":"assistant","sessionId":"large-sess","parentUuid":"p-{idx}","timestamp":"2026-04-20T10:02:00.000Z","message":{{"model":"{model}","usage":{{"input_tokens":{in_tok},"output_tokens":{out_tok},"cache_read_input_tokens":{cr},"cache_creation_input_tokens":200}},"content":[{{"type":"tool_use","id":"tu-{idx}","name":"Bash","input":{{"command":"cargo test -p vct-core -- {f}","description":"{filler}"}}}}]}}}}"#,
                in_tok = 800 + (idx % 300),
                out_tok = 180 + (idx % 100),
                cr = 25_000 + (idx % 10_000),
                f = idx % 16,
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }

    assert!(
        out.len() >= 2 * 1024 * 1024,
        "synthetic multi-MB JSONL should be at least 2 MiB (got {} bytes)",
        out.len()
    );
    std::fs::write(&path, out).expect("write multi-MB Claude JSONL");
    path
}

fn build_synthetic_usage_pipeline_input(
    model_count: usize,
) -> (
    UsageResult,
    PerProviderUsage,
    ProviderActiveDays,
    StoredCosts,
) {
    const BASE_MODELS: [&str; 10] = [
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
        "gpt-5.4",
        "gpt-5.4-codex",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "deepseek-reasoner",
        "grok-code-fast-1",
        "qwen3-coder",
    ];
    const PREFIXES: [&str; 10] = [
        "",
        "openai/",
        "copilot/",
        "vertex_ai/",
        "xai/",
        "deepseek/",
        "antigravity/",
        "opencode/",
        "cursor/",
        "hermes/",
    ];

    let mut usage_data = UsageResult::default();
    let mut per_provider = PerProviderUsage::default();
    let mut stored_costs = StoredCosts::default();

    for idx in 0..model_count {
        let slot = idx % PREFIXES.len();
        let group_idx = idx / PREFIXES.len();
        let base = BASE_MODELS[group_idx % BASE_MODELS.len()];
        let revision = group_idx / BASE_MODELS.len();
        let model = if revision == 0 {
            format!("{}{base}", PREFIXES[slot])
        } else {
            format!("{}{base}-r{revision:02}", PREFIXES[slot])
        };
        let scale = (idx as i64) + 1;
        let payload = json!({
            "input_tokens": 12_000 * scale,
            "output_tokens": 2_400 * scale,
            "reasoning_tokens": if idx % 3 == 0 { 600 * scale } else { 0 },
            "cache_read_input_tokens": 48_000 * scale,
            "cache_creation_input_tokens": 1_200 * scale,
        });

        usage_data.insert(model.clone(), payload.clone());
        match slot {
            0 => {
                per_provider.claude.insert(model, payload);
            }
            1 => {
                per_provider.codex.insert(model, payload);
            }
            2 => {
                per_provider.copilot.insert(model, payload);
            }
            3 => {
                per_provider.gemini.insert(model, payload);
            }
            4 => {
                per_provider.grok.insert(model, payload);
            }
            5 => {
                per_provider.deepseek.insert(model, payload);
            }
            6 => {
                per_provider.antigravity.insert(model, payload);
            }
            7 => {
                stored_costs
                    .opencode
                    .insert(model.clone(), (scale as f64) * 0.02);
                per_provider.opencode.insert(model, payload);
            }
            8 => {
                per_provider.cursor.insert(model, payload);
            }
            _ => {
                stored_costs
                    .hermes
                    .insert(model.clone(), (scale as f64) * 0.015);
                per_provider.hermes.insert(model, payload);
            }
        }
    }

    let provider_days = ProviderActiveDays {
        claude: 14,
        codex: 14,
        copilot: 10,
        gemini: 12,
        grok: 8,
        deepseek: 9,
        antigravity: 7,
        opencode: 11,
        cursor: 6,
        hermes: 5,
        total: 14,
    };

    assert_eq!(usage_data.len(), model_count);

    (usage_data, per_provider, provider_days, stored_costs)
}

fn benchmark_large_scale_pipeline(c: &mut Criterion) {
    let temp = TempDir::new().expect("create large-scale pipeline benchmark directory");
    let large_jsonl = build_multi_mb_claude_jsonl(temp.path(), 3_000);
    let pricing_map = build_synthetic_pricing_map(2_000);
    let (usage_data, per_provider, provider_days, stored_costs) =
        build_synthetic_usage_pipeline_input(150);

    let baseline_summary = build_usage_summary(
        &usage_data,
        &per_provider,
        &provider_days,
        &pricing_map,
        &stored_costs,
    );
    assert_eq!(baseline_summary.rows.len(), 150);
    let merged_baseline = merge_rows_by_base_model(&baseline_summary.rows);
    assert!(merged_baseline.len() < baseline_summary.rows.len());

    let mut group = c.benchmark_group("large_scale_pipeline");
    group.sample_size(10);

    group.bench_function("parse_multi_mb_jsonl_usage_only", |b| {
        b.iter(|| {
            black_box(
                parse_session_file_typed_as(
                    black_box(&large_jsonl),
                    ExtensionType::ClaudeCode,
                    ParseMode::UsageOnly,
                )
                .expect("parse multi-MB Claude JSONL (usage_only)"),
            )
        })
    });

    group.bench_function("parse_multi_mb_jsonl_full", |b| {
        b.iter(|| {
            black_box(
                parse_session_file_typed_as(
                    black_box(&large_jsonl),
                    ExtensionType::ClaudeCode,
                    ParseMode::Full,
                )
                .expect("parse multi-MB Claude JSONL (full)"),
            )
        })
    });

    group.bench_function("build_usage_summary_150_models_2000_catalog", |b| {
        b.iter_batched(
            clear_pricing_cache,
            |()| {
                black_box(build_usage_summary(
                    black_box(&usage_data),
                    black_box(&per_provider),
                    black_box(&provider_days),
                    black_box(&pricing_map),
                    black_box(&stored_costs),
                ))
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("merge_rows_by_base_model_150_models", |b| {
        b.iter(|| black_box(merge_rows_by_base_model(black_box(&baseline_summary.rows))))
    });

    group.finish();
}

// ========== Format Detection Benchmarks ==========

fn benchmark_format_detection(c: &mut Criterion) {
    use vct_core::session::detector::detect_extension_type;

    let claude_data = vec![
        json!({"parentUuid": null, "type": "user", "message": {"role": "user"}}),
        json!({"parentUuid": "abc", "type": "assistant", "message": {"role": "assistant"}}),
    ];

    let codex_data = vec![
        json!({"completion_response": {"usage": {}}, "total_token_usage": {}}),
        json!({"completion_response": {"usage": {}}}),
    ];

    let copilot_data = vec![json!({"sessionId": "test", "startTime": 123, "timeline": []})];

    let gemini_data = vec![json!({"sessionId": "test", "projectHash": "abc", "messages": []})];

    c.bench_function("detect_format claude", |b| {
        b.iter(|| detect_extension_type(black_box(&claude_data)))
    });

    c.bench_function("detect_format codex", |b| {
        b.iter(|| detect_extension_type(black_box(&codex_data)))
    });

    c.bench_function("detect_format copilot", |b| {
        b.iter(|| detect_extension_type(black_box(&copilot_data)))
    });

    c.bench_function("detect_format gemini", |b| {
        b.iter(|| detect_extension_type(black_box(&gemini_data)))
    });
}

// ========== Cache Performance Benchmarks ==========

fn benchmark_cache_operations(c: &mut Criterion) {
    use vct_core::cache::global_cache;

    let test_path = fixture("sessions/claude_code.jsonl");

    if !test_path.exists() {
        return;
    }

    let _ = global_cache().get_or_parse(&test_path);

    c.bench_function("cache hit (warm)", |b| {
        b.iter(|| global_cache().get_or_parse(black_box(&test_path)))
    });

    c.bench_function("cache miss (cold)", |b| {
        b.iter_batched(
            || {
                global_cache().invalidate(&test_path);
                test_path.clone()
            },
            |path| global_cache().get_or_parse(&path),
            BatchSize::PerIteration,
        )
    });

    c.bench_function("cache stats", |b| b.iter(|| global_cache().stats()));
}

// ========== HashMap Performance Benchmarks ==========

fn benchmark_hashmap_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap");

    let keys: Vec<String> = (0..1000).map(|i| format!("key_{i}")).collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let values: Vec<i32> = (0..1000).collect();

    group.bench_function("FastHashMap insert 1000", |b| {
        b.iter(|| {
            let mut map = FastHashMap::with_capacity(key_refs.len());
            for (&k, &v) in key_refs.iter().zip(values.iter()) {
                map.insert(k, v);
            }
            black_box(map);
        })
    });

    group.bench_function("std HashMap insert 1000", |b| {
        b.iter(|| {
            let mut map = HashMap::with_capacity(key_refs.len());
            for (&k, &v) in key_refs.iter().zip(values.iter()) {
                map.insert(k, v);
            }
            black_box(map);
        })
    });

    let mut fast_map = FastHashMap::with_capacity(key_refs.len());
    let mut std_map = HashMap::with_capacity(key_refs.len());
    for (&k, &v) in key_refs.iter().zip(values.iter()) {
        fast_map.insert(k, v);
        std_map.insert(k, v);
    }

    group.bench_function("FastHashMap lookup 1000", |b| {
        b.iter(|| {
            for &key in &key_refs {
                black_box(fast_map.get(key));
            }
        })
    });

    group.bench_function("std HashMap lookup 1000", |b| {
        b.iter(|| {
            for &key in &key_refs {
                black_box(std_map.get(key));
            }
        })
    });

    group.finish();
}

// ========== Usage Aggregation Benchmarks ==========

fn benchmark_usage_aggregation(c: &mut Criterion) {
    c.bench_function("aggregate usage 100 models", |b| {
        b.iter(|| {
            let mut result = UsageResult::default();
            for i in 0..100 {
                let model = format!("model-{}", i % 5);

                let usage = json!({
                    "input_tokens": 1000,
                    "output_tokens": 500,
                    "cache_read_input_tokens": 2000,
                    "cache_creation_input_tokens": 300,
                    "cost_usd": 0.01,
                    "matched_model": format!("matched-model-{}", i % 5)
                });

                result.insert(model, usage);
            }
            black_box(result);
        })
    });
}

// ========== Batch Analysis Benchmarks ==========

fn benchmark_batch_analysis(c: &mut Criterion) {
    let paths = [
        fixture("sessions/claude_code.jsonl"),
        fixture("sessions/codex.jsonl"),
        fixture("sessions/copilot.jsonl"),
        fixture("sessions/gemini.jsonl"),
        fixture("sessions/grok/signals.json"),
        fixture("sessions/dsh/session.jsonl.zstd"),
    ];

    if paths.iter().any(|path| !path.exists()) {
        return;
    }

    c.bench_function("batch analyze all formats", |b| {
        b.iter(|| {
            for path in &paths {
                black_box(
                    vct_core::session::parse_session_file_to_value(black_box(path))
                        .expect("parse batch benchmark fixture"),
                );
            }
        })
    });
}

// ========== JSON Serialization Benchmarks ==========

fn benchmark_json_serialization(c: &mut Criterion) {
    let mut result = UsageResult::default();
    for i in 0..50 {
        let model = format!("claude-sonnet-{i}");

        let usage = json!({
            "input_tokens": 1000 + i * 10,
            "output_tokens": 500 + i * 5,
            "cache_read_input_tokens": 2000 + i * 20,
            "cache_creation_input_tokens": 300 + i * 3,
            "cost_usd": 0.01 * (i as f64),
            "matched_model": "claude-sonnet"
        });

        result.insert(model, usage);
    }
    assert_eq!(result.len(), 50);

    c.bench_function("serialize UsageResult", |b| {
        b.iter(|| black_box(serde_json::to_string(black_box(&result)).expect("serialize json")))
    });

    c.bench_function("serialize UsageResult pretty", |b| {
        b.iter(|| {
            black_box(serde_json::to_string_pretty(black_box(&result)).expect("serialize pretty"))
        })
    });
}

// ========== TUI Frame & Layout Rendering Benchmarks ==========

fn benchmark_tui_frame_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("tui_frame_render");

    let mut loading =
        Terminal::new(TestBackend::new(160, 42)).expect("create loading frame benchmark terminal");
    let mut spinner_index = 0usize;
    group.bench_function("loading", |b| {
        b.iter(|| {
            loading
                .draw(|frame| {
                    render_loading(
                        frame,
                        frame.area(),
                        black_box(spinner_index),
                        "Loading sessions...",
                    )
                })
                .expect("render loading benchmark frame");
            spinner_index = spinner_index.wrapping_add(1);
        })
    });

    group.bench_function("settings_init", |b| {
        b.iter(|| black_box(init_settings_ui()))
    });

    let mut settings_fixture =
        LayoutFrameBenchmark::new("classic", 190, 42, 120).expect("create settings benchmark");
    group.bench_function("settings_frame", |b| {
        b.iter(|| {
            settings_fixture
                .render_tab(Tab::Settings, true, None)
                .expect("render settings benchmark frame")
        })
    });

    group.finish();
}

fn benchmark_tui_layouts(c: &mut Criterion) {
    let ids = layout_ids();
    assert!(
        !ids.is_empty(),
        "expected at least one layout plugin to be registered"
    );

    let mut group = c.benchmark_group("tui_layouts_120_rows");
    group.sample_size(20);

    for &layout_id in ids {
        let mut usage_fixture =
            LayoutFrameBenchmark::new(layout_id, 190, 42, 120).expect("create usage layout bench");
        group.bench_function(BenchmarkId::new("usage", layout_id), |b| {
            b.iter(|| {
                usage_fixture
                    .render_tab(Tab::Usage, true, black_box(None))
                    .expect("render usage layout benchmark frame")
            })
        });

        let mut analysis_fixture = LayoutFrameBenchmark::new(layout_id, 190, 42, 120)
            .expect("create analysis layout bench");
        group.bench_function(BenchmarkId::new("analysis", layout_id), |b| {
            b.iter(|| {
                analysis_fixture
                    .render_tab(Tab::Analysis, true, black_box(None))
                    .expect("render analysis layout benchmark frame")
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    benchmark_normalize_model_name,
    benchmark_pricing_lookup,
    benchmark_line_counting,
    benchmark_file_parsing,
    benchmark_known_provider_parsing,
    benchmark_long_preamble_detection,
    benchmark_summary_scan_cache,
    benchmark_sqlite_providers,
    benchmark_ledger_lifecycle,
    benchmark_large_scale_pipeline,
    benchmark_format_detection,
    benchmark_cache_operations,
    benchmark_hashmap_performance,
    benchmark_usage_aggregation,
    benchmark_batch_analysis,
    benchmark_json_serialization,
    benchmark_tui_frame_render,
    benchmark_tui_layouts,
);
criterion_main!(benches);
