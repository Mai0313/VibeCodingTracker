use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::collections::HashMap;
use vibe_coding_tracker::pricing::{ModelPricingMap, normalize_model_name};

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
    // Create a mock pricing map
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

criterion_group!(
    benches,
    benchmark_normalize_model_name,
    benchmark_pricing_lookup,
    benchmark_line_counting
);
criterion_main!(benches);
