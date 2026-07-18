use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ratatui::{Terminal, backend::TestBackend};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::config::ProvidersConfig;
use vibe_coding_tracker::constants::FastHashMap;
use vibe_coding_tracker::display::common::render_loading_frame;
use vibe_coding_tracker::display::usage::UsageFrameBenchmark;
use vibe_coding_tracker::models::ExtensionType;
use vibe_coding_tracker::pricing::{ModelPricingMap, normalize_model_name};
use vibe_coding_tracker::scan::build_scan_pool;
use vibe_coding_tracker::session::{
    ParseMode, parse_session_file_as, parse_session_file_typed_with_mode,
};
use vibe_coding_tracker::summary_cache::SummaryScanCache;
use vibe_coding_tracker::usage::calculator::get_usage_from_paths_with_cache;
use vibe_coding_tracker::utils::{HelperPaths, resolve_paths_from_home};

/// Absolute path to a file under the repo's `tests/fixtures/` directory.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
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
    use std::collections::HashMap;

    // Create a mock pricing map (ModelPricingMap requires std HashMap)
    let mut pricing_data = HashMap::new();
    pricing_data.insert(
        "claude-3-sonnet".to_string(),
        vibe_coding_tracker::pricing::ModelPricing::default(),
    );
    pricing_data.insert(
        "gpt-4-turbo".to_string(),
        vibe_coding_tracker::pricing::ModelPricing::default(),
    );
    pricing_data.insert(
        "gemini-pro".to_string(),
        vibe_coding_tracker::pricing::ModelPricing::default(),
    );
    pricing_data.insert(
        "copilot-gpt-4".to_string(),
        vibe_coding_tracker::pricing::ModelPricing::default(),
    );

    let pricing_map = ModelPricingMap::new(pricing_data);

    c.bench_function("pricing lookup exact match", |b| {
        b.iter(|| pricing_map.get(black_box("claude-3-sonnet")))
    });

    c.bench_function("pricing lookup normalized", |b| {
        b.iter(|| pricing_map.get(black_box("claude-3-sonnet-20240229")))
    });

    c.bench_function("pricing lookup fuzzy", |b| {
        b.iter(|| pricing_map.get(black_box("claude-sonnet-3")))
    });
}

fn benchmark_line_counting(c: &mut Criterion) {
    let short_text = "line1\nline2\nline3\n";
    let medium_text = (0..100).map(|i| format!("line{}\n", i)).collect::<String>();
    let long_text = (0..10000)
        .map(|i| format!("line{}\n", i))
        .collect::<String>();

    c.bench_function("count_lines short (3 lines)", |b| {
        b.iter(|| vibe_coding_tracker::utils::count_lines(black_box(short_text)))
    });

    c.bench_function("count_lines medium (100 lines)", |b| {
        b.iter(|| vibe_coding_tracker::utils::count_lines(black_box(&medium_text)))
    });

    c.bench_function("count_lines long (10k lines)", |b| {
        b.iter(|| vibe_coding_tracker::utils::count_lines(black_box(&long_text)))
    });
}

// ========== File Parsing Benchmarks ==========

fn benchmark_file_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_parsing");

    // Test files paths
    let test_files = vec![
        ("claude", "sessions/claude_code.jsonl"),
        ("codex", "sessions/codex.jsonl"),
        ("copilot", "sessions/copilot.jsonl"),
        ("gemini", "sessions/gemini.jsonl"),
        ("grok", "sessions/grok/signals.json"),
    ];

    for (name, path) in test_files {
        let path_buf = fixture(path);
        if path_buf.exists() {
            group.bench_with_input(
                BenchmarkId::new("parse_session_file", name),
                &path_buf,
                |b, p| b.iter(|| vibe_coding_tracker::session::parse_session_file(black_box(p))),
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
                        parse_session_file_as(black_box(&path), provider, mode)
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

fn run_cached_scan(pool: &rayon::ThreadPool, paths: &HelperPaths, cache: &mut SummaryScanCache) {
    let result = pool
        .install(|| get_usage_from_paths_with_cache(paths, TimeRange::All, claude_only(), cache));
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
            SummaryScanCache::new,
            |mut cache| run_cached_scan(&pool, &small_paths, &mut cache),
            criterion::BatchSize::SmallInput,
        );
    });
    group.bench_function(BenchmarkId::new("cold", 1_000), |b| {
        b.iter_batched(
            SummaryScanCache::new,
            |mut cache| run_cached_scan(&pool, &large_paths, &mut cache),
            criterion::BatchSize::SmallInput,
        );
    });

    let mut warm_cache = SummaryScanCache::new();
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
            let mut cache = SummaryScanCache::new();
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

// ========== Format Detection Benchmarks ==========

fn benchmark_format_detection(c: &mut Criterion) {
    use serde_json::json;
    use vibe_coding_tracker::session::detector::detect_extension_type;

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
    use vibe_coding_tracker::cache::global_cache;

    let test_path = fixture("sessions/claude_code.jsonl");

    if !test_path.exists() {
        return;
    }

    // Warm up cache
    let _ = global_cache().get_or_parse(&test_path);

    c.bench_function("cache hit (warm)", |b| {
        b.iter(|| global_cache().get_or_parse(black_box(&test_path)))
    });

    c.bench_function("cache miss (cold)", |b| {
        b.iter_batched(
            || {
                // Clear cache before each iteration
                global_cache().invalidate(&test_path);
                test_path.clone()
            },
            |path| global_cache().get_or_parse(&path),
            criterion::BatchSize::SmallInput,
        )
    });

    c.bench_function("cache stats", |b| b.iter(|| global_cache().stats()));
}

// ========== HashMap Performance Benchmarks ==========

fn benchmark_hashmap_performance(c: &mut Criterion) {
    use std::collections::HashMap;

    let mut group = c.benchmark_group("hashmap");

    // Prepare test data
    let keys: Vec<String> = (0..1000).map(|i| format!("key_{}", i)).collect();
    let values: Vec<i32> = (0..1000).collect();

    // ahash::AHashMap (FastHashMap)
    group.bench_function("FastHashMap insert 1000", |b| {
        b.iter(|| {
            let mut map = FastHashMap::default();
            for (k, v) in keys.iter().zip(values.iter()) {
                map.insert(k.clone(), *v);
            }
            black_box(map);
        })
    });

    // std::collections::HashMap
    group.bench_function("std HashMap insert 1000", |b| {
        b.iter(|| {
            let mut map = HashMap::new();
            for (k, v) in keys.iter().zip(values.iter()) {
                map.insert(k.clone(), *v);
            }
            black_box(map);
        })
    });

    // Lookup benchmark
    let mut fast_map = FastHashMap::default();
    let mut std_map = HashMap::new();
    for (k, v) in keys.iter().zip(values.iter()) {
        fast_map.insert(k.clone(), *v);
        std_map.insert(k.clone(), *v);
    }

    group.bench_function("FastHashMap lookup 1000", |b| {
        b.iter(|| {
            for key in &keys {
                black_box(fast_map.get(key));
            }
        })
    });

    group.bench_function("std HashMap lookup 1000", |b| {
        b.iter(|| {
            for key in &keys {
                black_box(std_map.get(key));
            }
        })
    });

    group.finish();
}

// ========== Usage Aggregation Benchmarks ==========

fn benchmark_usage_aggregation(c: &mut Criterion) {
    use serde_json::json;
    use vibe_coding_tracker::models::usage::UsageResult;

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
    // Only run if fixture files exist
    let claude_path = fixture("sessions/claude_code.jsonl");
    let codex_path = fixture("sessions/codex.jsonl");
    let copilot_path = fixture("sessions/copilot.jsonl");
    let gemini_path = fixture("sessions/gemini.jsonl");
    let grok_path = fixture("sessions/grok/signals.json");

    if !claude_path.exists()
        || !codex_path.exists()
        || !copilot_path.exists()
        || !gemini_path.exists()
        || !grok_path.exists()
    {
        return;
    }

    c.bench_function("batch analyze all formats", |b| {
        b.iter(|| {
            // Create temporary directory paths for testing
            let paths = vec![
                (claude_path.clone(), "claude"),
                (codex_path.clone(), "codex"),
                (copilot_path.clone(), "copilot"),
                (gemini_path.clone(), "gemini"),
                (grok_path.clone(), "grok"),
            ];

            // Simulate batch processing
            for (path, _name) in paths {
                let _ = vibe_coding_tracker::session::parse_session_file(black_box(&path));
            }
        })
    });
}

// ========== JSON Serialization Benchmarks ==========

fn benchmark_json_serialization(c: &mut Criterion) {
    use serde_json::json;
    use vibe_coding_tracker::models::usage::UsageResult;

    // Create sample data
    let mut result = UsageResult::default();
    for i in 0..50 {
        let model = format!("claude-sonnet-{}", i % 3);

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

    c.bench_function("serialize UsageResult", |b| {
        b.iter(|| serde_json::to_string(black_box(&result)))
    });

    c.bench_function("serialize UsageResult pretty", |b| {
        b.iter(|| serde_json::to_string_pretty(black_box(&result)))
    });
}

// ========== TUI Frame Rendering Benchmarks ==========

fn benchmark_tui_frame_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("tui_frame_render");

    let mut loading =
        Terminal::new(TestBackend::new(160, 42)).expect("create loading frame benchmark terminal");
    let mut spinner_index = 0usize;
    group.bench_function("loading", |b| {
        b.iter(|| {
            render_loading_frame(&mut loading, black_box(spinner_index))
                .expect("render loading benchmark frame");
            spinner_index = spinner_index.wrapping_add(1);
        })
    });

    let mut ready =
        UsageFrameBenchmark::new(160, 42).expect("create ready frame benchmark fixture");
    group.bench_function("ready", |b| {
        b.iter(|| {
            ready
                .render(black_box(None))
                .expect("render ready benchmark frame")
        })
    });

    let mut refreshing =
        UsageFrameBenchmark::new(160, 42).expect("create refreshing frame benchmark fixture");
    group.bench_function("refreshing", |b| {
        b.iter(|| {
            refreshing
                .render(black_box(Some("Refreshing...")))
                .expect("render refreshing benchmark frame")
        })
    });

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
    benchmark_format_detection,
    benchmark_cache_operations,
    benchmark_hashmap_performance,
    benchmark_usage_aggregation,
    benchmark_batch_analysis,
    benchmark_json_serialization,
    benchmark_tui_frame_render,
);
criterion_main!(benches);
