use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::path::PathBuf;
use vibe_coding_tracker::constants::FastHashMap;
use vibe_coding_tracker::pricing::{ModelPricingMap, normalize_model_name};

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
        ("claude", "examples/test_conversation.jsonl"),
        ("codex", "examples/test_conversation_oai.jsonl"),
        ("copilot", "examples/test_conversation_copilot.json"),
        ("gemini", "examples/test_conversation_gemini.json"),
    ];

    for (name, path) in test_files {
        let path_buf = PathBuf::from(path);
        if path_buf.exists() {
            group.bench_with_input(
                BenchmarkId::new("analyze_jsonl_file", name),
                &path_buf,
                |b, p| b.iter(|| vibe_coding_tracker::analysis::analyze_jsonl_file(black_box(p))),
            );
        }
    }

    group.finish();
}

// ========== Format Detection Benchmarks ==========

fn benchmark_format_detection(c: &mut Criterion) {
    use serde_json::json;
    use vibe_coding_tracker::analysis::detector::detect_extension_type;

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
    use std::path::PathBuf;
    use vibe_coding_tracker::cache::global_cache;

    let test_path = PathBuf::from("examples/test_conversation.jsonl");

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
    use vibe_coding_tracker::models::usage::DateUsageResult;

    c.bench_function("aggregate usage 100 dates", |b| {
        b.iter(|| {
            let mut result = DateUsageResult::new();
            for i in 0..100 {
                let date = format!("2025-01-{:02}", (i % 28) + 1);
                let model = format!("model-{}", i % 5);

                let usage = json!({
                    "input_tokens": 1000,
                    "output_tokens": 500,
                    "cache_read_input_tokens": 2000,
                    "cache_creation_input_tokens": 300,
                    "cost_usd": 0.01,
                    "matched_model": format!("matched-model-{}", i % 5)
                });

                let date_entry = result.entry(date).or_default();
                date_entry.insert(model, usage);
            }
            black_box(result);
        })
    });
}

// ========== Batch Analysis Benchmarks ==========

fn benchmark_batch_analysis(c: &mut Criterion) {
    use std::path::PathBuf;

    // Only run if example files exist
    let claude_path = PathBuf::from("examples/test_conversation.jsonl");
    let codex_path = PathBuf::from("examples/test_conversation_oai.jsonl");
    let copilot_path = PathBuf::from("examples/test_conversation_copilot.json");
    let gemini_path = PathBuf::from("examples/test_conversation_gemini.json");

    if !claude_path.exists()
        || !codex_path.exists()
        || !copilot_path.exists()
        || !gemini_path.exists()
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
            ];

            // Simulate batch processing
            for (path, _name) in paths {
                let _ = vibe_coding_tracker::analysis::analyze_jsonl_file(black_box(&path));
            }
        })
    });
}

// ========== JSON Serialization Benchmarks ==========

fn benchmark_json_serialization(c: &mut Criterion) {
    use serde_json::json;
    use vibe_coding_tracker::models::usage::DateUsageResult;

    // Create sample data
    let mut result = DateUsageResult::new();
    for i in 0..50 {
        let date = format!("2025-01-{:02}", (i % 28) + 1);
        let model = format!("claude-sonnet-{}", i % 3);

        let usage = json!({
            "input_tokens": 1000 + i * 10,
            "output_tokens": 500 + i * 5,
            "cache_read_input_tokens": 2000 + i * 20,
            "cache_creation_input_tokens": 300 + i * 3,
            "cost_usd": 0.01 * (i as f64),
            "matched_model": "claude-sonnet"
        });

        let date_entry = result.entry(date).or_default();
        date_entry.insert(model, usage);
    }

    c.bench_function("serialize DateUsageResult", |b| {
        b.iter(|| serde_json::to_string(black_box(&result)))
    });

    c.bench_function("serialize DateUsageResult pretty", |b| {
        b.iter(|| serde_json::to_string_pretty(black_box(&result)))
    });
}

criterion_group!(
    benches,
    benchmark_normalize_model_name,
    benchmark_pricing_lookup,
    benchmark_line_counting,
    benchmark_file_parsing,
    benchmark_format_detection,
    benchmark_cache_operations,
    benchmark_hashmap_performance,
    benchmark_usage_aggregation,
    benchmark_batch_analysis,
    benchmark_json_serialization,
);
criterion_main!(benches);
