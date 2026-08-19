// Integration tests for usage aggregation.
//
// These drive `aggregate_usage_from_paths` against a `TempHome` (fixture session files
// under a temp directory) so the real aggregation runs hermetically: no
// process-global env is mutated, no machine files are read, and no external API
// is reached. The remaining tests are pure in-memory cost / JSON math.

use rusqlite::{Connection, params};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use vct_core::TimeRange;
use vct_core::config::ProvidersConfig;
use vct_core::models::{ExtensionType, PerProviderUsage, ProviderActiveDays};
use vct_core::summary_cache::SummaryScanCache;
use vct_core::usage::aggregator::{
    StoredCosts, UsageData, aggregate_usage_from_paths, aggregate_usage_from_paths_with_cache,
    aggregate_usage_from_paths_with_diagnostics, aggregate_usage_from_paths_with_providers,
};
use vct_test_support::{TempHome, append_cursor_json_blob, fixture_str};

/// A real Codex rollout file name: the archive move preserves it, and it is
/// what cross-root deduplication keys on.
const CODEX_ROLLOUT: &str =
    "rollout-2026-06-06T10-00-00-019e4b75-8a4e-7801-a9ed-9723e77e0497.jsonl";

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
    }
}

// Only the permission-based test needs this, and that one is Unix-only.
#[cfg(unix)]
fn codex_only() -> ProvidersConfig {
    ProvidersConfig {
        claude: false,
        codex: true,
        copilot: false,
        gemini: false,
        opencode: false,
        cursor: false,
        hermes: false,
        grok: false,
        dsh: false,
    }
}

fn opencode_only() -> ProvidersConfig {
    ProvidersConfig {
        claude: false,
        codex: false,
        copilot: false,
        gemini: false,
        opencode: true,
        cursor: false,
        hermes: false,
        grok: false,
        dsh: false,
    }
}

fn cursor_only() -> ProvidersConfig {
    ProvidersConfig {
        claude: false,
        codex: false,
        copilot: false,
        gemini: false,
        opencode: false,
        cursor: true,
        hermes: false,
        grok: false,
        dsh: false,
    }
}

fn seed_opencode_usage_db(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            r#"CREATE TABLE session (
                   id TEXT PRIMARY KEY,
                   directory TEXT NOT NULL,
                   time_updated INTEGER NOT NULL
               );
               CREATE TABLE message (
                   id TEXT PRIMARY KEY,
                   session_id TEXT NOT NULL,
                   data TEXT NOT NULL
               );
               CREATE TABLE part (
                   id TEXT PRIMARY KEY,
                   message_id TEXT NOT NULL,
                   session_id TEXT NOT NULL,
                   data TEXT NOT NULL
               );
               INSERT INTO session (id, directory, time_updated)
               VALUES ('open-session', '/repo', 1780757089000);
               INSERT INTO message (id, session_id, data)
               VALUES (
                   'open-message',
                   'open-session',
                   '{"role":"assistant","providerID":"openai","modelID":"open-model","cost":0.25,"tokens":{"input":31,"output":17,"reasoning":3,"cache":{"read":11,"write":5}},"time":{"created":1780757088000,"completed":1780757089000}}'
               );"#,
        )
        .unwrap();
}

fn seed_hermes_usage_db(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
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
             );
             INSERT INTO session_model_usage (
                 session_id, model, billing_provider, input_tokens, output_tokens,
                 cache_read_tokens, cache_write_tokens, reasoning_tokens,
                 estimated_cost_usd, actual_cost_usd, first_seen, last_seen
             ) VALUES (
                 'hermes-session', 'hermes-model', 'openai', 41, 23, 13, 7, 3,
                 0.5, 0.4, 1780757088, 1780757089
             );
             INSERT INTO sessions (
                 id, model, billing_provider, input_tokens, output_tokens,
                 cache_read_tokens, cache_write_tokens, reasoning_tokens,
                 estimated_cost_usd, actual_cost_usd, started_at, ended_at
             ) VALUES (
                 'hermes-session', 'hermes-model', 'openai', 41, 23, 13, 7, 3,
                 0.5, 0.4, 1780757088, 1780757089
             );",
        )
        .unwrap();
}

/// Compares every bucket of two usage results.
///
/// Every struct here is destructured rather than having its fields named one at
/// a time: a bucket added to any of them then fails to compile, instead of
/// silently going uncompared the way DeepSeek Harness did.
fn assert_usage_data_eq(actual: &UsageData, expected: &UsageData) {
    let UsageData {
        models,
        per_provider,
        provider_days,
        stored_costs,
    } = actual;
    assert_eq!(models, &expected.models);

    let PerProviderUsage {
        claude,
        codex,
        copilot,
        gemini,
        opencode,
        cursor,
        hermes,
        grok,
        deepseek,
    } = per_provider;
    let other = &expected.per_provider;
    for (provider, actual_usage, expected_usage) in [
        ("Claude", claude, &other.claude),
        ("Codex", codex, &other.codex),
        ("Copilot", copilot, &other.copilot),
        ("Gemini", gemini, &other.gemini),
        ("OpenCode", opencode, &other.opencode),
        ("Cursor", cursor, &other.cursor),
        ("Hermes", hermes, &other.hermes),
        ("Grok", grok, &other.grok),
        ("DeepSeek", deepseek, &other.deepseek),
    ] {
        assert_eq!(actual_usage, expected_usage, "{provider} usage differs");
    }

    let ProviderActiveDays {
        claude,
        codex,
        copilot,
        gemini,
        opencode,
        cursor,
        hermes,
        grok,
        deepseek,
        total,
    } = provider_days;
    let other = &expected.provider_days;
    for (provider, actual_days, expected_days) in [
        ("Claude", claude, &other.claude),
        ("Codex", codex, &other.codex),
        ("Copilot", copilot, &other.copilot),
        ("Gemini", gemini, &other.gemini),
        ("OpenCode", opencode, &other.opencode),
        ("Cursor", cursor, &other.cursor),
        ("Hermes", hermes, &other.hermes),
        ("Grok", grok, &other.grok),
        ("DeepSeek", deepseek, &other.deepseek),
        ("All providers", total, &other.total),
    ] {
        assert_eq!(actual_days, expected_days, "{provider} active days differ");
    }

    let StoredCosts { opencode, hermes } = stored_costs;
    let other = &expected.stored_costs;
    for (provider, actual_costs, expected_costs) in [
        ("OpenCode", opencode, &other.opencode),
        ("Hermes", hermes, &other.hermes),
    ] {
        assert_eq!(
            actual_costs, expected_costs,
            "{provider} stored costs differ"
        );
    }
}

#[test]
fn empty_home_yields_no_usage() {
    let home = TempHome::new();
    let data =
        aggregate_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate empty home");
    assert!(data.models.is_empty(), "empty home has no models");
    assert_eq!(data.provider_days.total, 0);
}

#[test]
fn cached_usage_matches_uncached_for_every_provider_source() {
    let home = TempHome::new();
    home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_codex_session(
        "2026/06/06/rollout.jsonl",
        &fixture_str("sessions/codex.jsonl"),
    );
    home.put(
        ".copilot/session-state/copilot-session/events.jsonl",
        &fixture_str("sessions/copilot.jsonl"),
    );
    home.put_gemini_session(
        "project-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );
    home.put_grok_fixture_session("workspace", "grok-session");
    seed_opencode_usage_db(&home.paths.opencode_db);
    home.put_cursor_session(
        "cursor-project",
        "cursor-conversation",
        "cursor-model",
        1_780_757_089_000,
        1_234,
    );
    seed_hermes_usage_db(&home.paths.hermes_db);
    home.put_dsh_fixture_session("dsh-project", "dsh-session");

    let providers = ProvidersConfig::default();
    let uncached =
        aggregate_usage_from_paths_with_providers(&home.paths, TimeRange::All, providers).unwrap();
    let mut cache = SummaryScanCache::new();
    let cold =
        aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
            .unwrap();

    assert_eq!(cold.diagnostics.candidates, 9);
    assert_eq!(cold.diagnostics.parsed, 9);
    assert!(cold.diagnostics.failures.is_empty());
    assert_eq!(cache.stats().parsed_sources, 9);
    assert_usage_data_eq(&cold.data, &uncached);
    // Destructured so that a provider added later cannot be left unseeded: the
    // new field has to be named here, and naming it without using it below is
    // an unused-variable warning, which CI denies.
    let PerProviderUsage {
        claude,
        codex,
        copilot,
        gemini,
        opencode,
        cursor,
        hermes,
        grok,
        deepseek,
    } = &cold.data.per_provider;
    for (provider, usage) in [
        ("Claude", claude),
        ("Codex", codex),
        ("Copilot", copilot),
        ("Gemini", gemini),
        ("Grok", grok),
        ("DeepSeek", deepseek),
        ("OpenCode", opencode),
        ("Cursor", cursor),
        ("Hermes", hermes),
    ] {
        assert!(
            !usage.is_empty(),
            "{provider} fixture must contribute usage"
        );
    }

    let warm =
        aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
            .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_usage_data_eq(&warm.data, &uncached);
}

#[cfg(unix)]
#[test]
fn usage_cache_preserves_entries_after_partial_directory_discovery() {
    let home = TempHome::new();
    home.put_claude_session(
        "visible",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let hidden_source = home.put_claude_session(
        "hidden",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let hidden_dir = hidden_source.parent().unwrap();
    let original_permissions = std::fs::metadata(hidden_dir).unwrap().permissions();
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        claude_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().entries, 2);
    assert_eq!(cache.stats().parsed_sources, 2);

    std::fs::set_permissions(hidden_dir, std::fs::Permissions::from_mode(0o0)).unwrap();
    let partial = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        claude_only(),
        &mut cache,
    );
    std::fs::set_permissions(hidden_dir, original_permissions).unwrap();
    let partial = partial.unwrap();

    assert_eq!(partial.diagnostics.candidates, 2);
    assert_eq!(partial.diagnostics.parsed, 1);
    assert_eq!(partial.diagnostics.failures.len(), 1);
    assert!(
        partial.diagnostics.failures[0]
            .source
            .starts_with(hidden_dir)
    );
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 2);

    let restored = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        claude_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 2);
    assert!(restored.diagnostics.failures.is_empty());
    assert_usage_data_eq(&restored.data, &cold.data);
}

#[test]
fn cursor_usage_cache_invalidates_only_changed_stores() {
    let home = TempHome::new();
    let first = home.put_cursor_session("project", "first", "cursor-first", 1_780_757_089_000, 100);
    let second =
        home.put_cursor_session("project", "second", "cursor-second", 1_780_757_090_000, 200);
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        cursor_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 2);
    assert_eq!(cache.stats().entries, 2);
    assert_eq!(cold.diagnostics.candidates, 2);
    assert_eq!(cold.diagnostics.parsed, 2);

    let warm = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        cursor_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_usage_data_eq(&warm.data, &cold.data);

    append_cursor_json_blob(&first, "mutation");
    let changed = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        cursor_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(cache.stats().entries, 2);
    assert_usage_data_eq(&changed.data, &cold.data);

    home.put_cursor_session("project", "third", "cursor-third", 1_780_757_091_000, 300);
    let added = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        cursor_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(cache.stats().entries, 3);
    assert!(added.data.per_provider.cursor.contains_key("cursor-third"));

    std::fs::remove_file(second).unwrap();
    let deleted = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        cursor_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 2);
    assert_eq!(deleted.diagnostics.candidates, 2);
    assert_eq!(deleted.diagnostics.parsed, 2);
    assert!(
        !deleted
            .data
            .per_provider
            .cursor
            .contains_key("cursor-second")
    );
}

#[test]
fn incremental_cache_reuses_unchanged_sources_and_tracks_mutations() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        claude_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);

    let warm = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        claude_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(
        serde_json::to_value(&cold.data.models).unwrap(),
        serde_json::to_value(&warm.data.models).unwrap()
    );

    let mut changed = fixture_str("sessions/claude_code.jsonl");
    changed.push('\n');
    std::fs::write(&source, changed).unwrap();
    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, claude_only(), &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);

    home.put_claude_session(
        "proj",
        "second.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, claude_only(), &mut cache)
        .unwrap();
    assert_eq!(
        cache.stats().parsed_sources,
        1,
        "only the added source parses"
    );
    assert_eq!(cache.stats().entries, 2);

    std::fs::remove_file(source).unwrap();
    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, claude_only(), &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 1, "deleted source is evicted");

    let disabled = ProvidersConfig {
        claude: false,
        ..claude_only()
    };
    let result =
        aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, disabled, &mut cache)
            .unwrap();
    assert!(result.data.models.is_empty());
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 0);
}

/// Each assistant step reports its usage twice — once as an early
/// `assistant/chunk` sample and again on the assembled `assistant/message`.
/// Counting both would double every bucket, so this pins the totals the fixture
/// was built to produce.
#[test]
fn dsh_usage_counts_each_step_once() {
    let home = TempHome::new();
    home.put_dsh_fixture_session("project", "session-fixture");

    let data = aggregate_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate usage");
    let usage = data
        .per_provider
        .deepseek
        .get("demo-model")
        .expect("DeepSeek usage row");

    // The fixture's one `demo-model` step reports 10 / 100 / 200 / 50 twice.
    assert_eq!(usage["input_tokens"], 10);
    assert_eq!(usage["output_tokens"], 100);
    assert_eq!(usage["cache_read_input_tokens"], 200);
    assert_eq!(usage["cache_creation_input_tokens"], 50);

    let reasoner = data
        .per_provider
        .deepseek
        .get("demo-reasoner")
        .expect("DeepSeek reasoning row");
    // Reasoning is a subset of output upstream, so it is subtracted back out.
    assert_eq!(reasoner["reasoning_output_tokens"], 30);
    // 80 - 30 from the message, plus the 2 of the step that died after its
    // usage chunk — that one names no model, so it is attributed to the route
    // the harness last logged.
    assert_eq!(reasoner["output_tokens"], 52);
    assert_eq!(reasoner["input_tokens"], 6);
    assert_eq!(data.provider_days.deepseek, 1);
}

#[test]
fn grok_sidecars_invalidate_the_compact_cache() {
    let home = TempHome::new();
    let signals = home.put_grok_fixture_session("workspace", "session");
    let providers = ProvidersConfig {
        grok: true,
        claude: false,
        codex: false,
        copilot: false,
        gemini: false,
        opencode: false,
        cursor: false,
        hermes: false,
        dsh: false,
    };
    let mut cache = SummaryScanCache::new();

    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);

    let updates = signals.with_file_name("updates.jsonl");
    let mut content = std::fs::read_to_string(&updates).unwrap();
    content.push('\n');
    std::fs::write(updates, content).unwrap();
    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
}

#[test]
fn usage_diagnostics_cover_empty_blank_all_failed_and_partial_scans() {
    let empty = TempHome::new();
    let no_candidates =
        aggregate_usage_from_paths_with_diagnostics(&empty.paths, TimeRange::All, claude_only())
            .unwrap();
    assert_eq!(no_candidates.diagnostics.candidates, 0);
    assert!(!no_candidates.diagnostics.all_failed());

    let blank = TempHome::new();
    blank.put_claude_session("proj", "blank.jsonl", "\n");
    let blank_result =
        aggregate_usage_from_paths_with_diagnostics(&blank.paths, TimeRange::All, claude_only())
            .unwrap();
    assert_eq!(blank_result.diagnostics.candidates, 1);
    assert_eq!(blank_result.diagnostics.parsed, 1);
    assert!(!blank_result.diagnostics.has_failures());

    let failed = TempHome::new();
    failed.put_claude_session("proj", "invalid.jsonl", "not json\n");
    let failed_result =
        aggregate_usage_from_paths_with_diagnostics(&failed.paths, TimeRange::All, claude_only())
            .unwrap();
    assert!(failed_result.diagnostics.all_failed());
    assert_eq!(failed_result.diagnostics.failures.len(), 1);

    failed.put_claude_session(
        "proj",
        "valid.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let partial =
        aggregate_usage_from_paths_with_diagnostics(&failed.paths, TimeRange::All, claude_only())
            .unwrap();
    assert!(partial.diagnostics.partially_failed());
    assert!(!partial.data.models.is_empty());
}

#[test]
fn zero_usage_metadata_does_not_increment_active_days() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "metadata.jsonl",
        r#"{"type":"permission-mode","parentUuid":"root","timestamp":"2026-07-12T00:00:00Z"}"#,
    );

    let legacy =
        aggregate_usage_from_paths_with_providers(&home.paths, TimeRange::All, claude_only())
            .unwrap();
    assert_eq!(legacy.provider_days.claude, 0);

    let cached =
        aggregate_usage_from_paths_with_diagnostics(&home.paths, TimeRange::All, claude_only())
            .unwrap();
    assert_eq!(cached.data.provider_days.claude, 0);
}

#[test]
fn usage_database_failures_preserve_all_failed_and_partial_diagnostics() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    std::fs::write(&home.paths.opencode_db, "not a SQLite database").unwrap();

    let failed =
        aggregate_usage_from_paths_with_diagnostics(&home.paths, TimeRange::All, opencode_only())
            .unwrap();
    assert!(failed.data.models.is_empty());
    assert_eq!(failed.diagnostics.candidates, 1);
    assert_eq!(failed.diagnostics.parsed, 0);
    assert!(failed.diagnostics.all_failed());
    assert_eq!(failed.diagnostics.failures.len(), 1);
    assert_eq!(
        failed.diagnostics.failures[0].provider,
        ExtensionType::OpenCode
    );
    assert_eq!(
        failed.diagnostics.failures[0].source,
        home.paths.opencode_db
    );

    home.put_claude_session(
        "proj",
        "valid.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = ProvidersConfig {
        opencode: true,
        ..claude_only()
    };
    let partial =
        aggregate_usage_from_paths_with_diagnostics(&home.paths, TimeRange::All, providers)
            .unwrap();
    assert_eq!(partial.diagnostics.candidates, 2);
    assert_eq!(partial.diagnostics.parsed, 1);
    assert!(partial.diagnostics.partially_failed());
    assert!(!partial.data.models.is_empty());
}

#[test]
fn opencode_usage_schema_drift_is_diagnostic_and_fingerprint_cached() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER); \
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT); \
             INSERT INTO session VALUES ('s1', '/repo', 1780757089000); \
             INSERT INTO message VALUES ('bad', 's1', \
                 '{\"role\":\"assistant\",\"futureUsage\":{\"input\":10}}');",
        )
        .unwrap();
    drop(connection);

    let mut cache = SummaryScanCache::new();
    let failed = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert!(failed.diagnostics.all_failed());
    assert_eq!(failed.diagnostics.candidates, 1);
    assert_eq!(failed.diagnostics.parsed, 0);
    assert_eq!(failed.diagnostics.failures.len(), 1);
    assert_eq!(cache.stats().parsed_sources, 1);

    let warm = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(warm.diagnostics, failed.diagnostics);

    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('good', 's1', ?1)",
            [r#"{"role":"assistant","modelID":"known","tokens":{"input":3}}"#],
        )
        .unwrap();
    drop(connection);

    let partial = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert!(partial.diagnostics.partially_failed());
    assert_eq!(partial.diagnostics.candidates, 1);
    assert_eq!(partial.diagnostics.parsed, 1);
    assert_eq!(partial.data.models["known"]["input_tokens"], 3);
}

#[test]
fn deterministic_sqlite_schema_failure_is_cached() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    Connection::open(&home.paths.opencode_db)
        .unwrap()
        .execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY);")
        .unwrap();

    let mut cache = SummaryScanCache::new();
    let cold = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert!(cold.diagnostics.all_failed());
    assert_eq!(cache.stats().parsed_sources, 1);

    let warm = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert!(warm.diagnostics.all_failed());
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_eq!(cache.stats().parsed_sources, 0);
}

#[test]
fn cursor_tracking_failure_does_not_create_a_source_candidate() {
    let home = TempHome::new();
    std::fs::create_dir_all(&home.paths.cursor_chats_dir).unwrap();
    std::fs::create_dir_all(home.paths.cursor_tracking_db.parent().unwrap()).unwrap();
    std::fs::write(&home.paths.cursor_tracking_db, "not SQLite").unwrap();

    let result =
        aggregate_usage_from_paths_with_diagnostics(&home.paths, TimeRange::All, cursor_only())
            .unwrap();
    assert_eq!(result.diagnostics.candidates, 0);
    assert_eq!(result.diagnostics.parsed, 0);
    assert_eq!(result.diagnostics.failures.len(), 1);
    assert!(!result.diagnostics.all_failed());
}

#[test]
fn opencode_wal_change_invalidates_the_compact_cache() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute_batch(
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
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session (id, model, directory, time_updated, tokens_input)
             VALUES (?1, ?2, '/repo', ?3, ?4)",
            params!["s1", r#"{"id":"wal-model"}"#, 1_780_757_089_000_i64, 10],
        )
        .unwrap();

    let mut cache = SummaryScanCache::new();
    let cold = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(cold.data.models["wal-model"]["input_tokens"], 10);

    aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, opencode_only(), &mut cache)
        .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);

    connection
        .execute(
            "INSERT INTO session (id, model, directory, time_updated, tokens_input)
             VALUES (?1, ?2, '/repo', ?3, ?4)",
            params!["s2", r#"{"id":"wal-model"}"#, 1_780_757_090_000_i64, 20],
        )
        .unwrap();
    let changed = aggregate_usage_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        opencode_only(),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(changed.data.models["wal-model"]["input_tokens"], 30);
}

#[test]
fn aggregates_claude_session_from_paths() {
    let home = TempHome::new();
    home.put_claude_session(
        "test-project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );

    let data = aggregate_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate claude");

    assert!(
        data.models.contains_key("claude-sonnet-4-20250514"),
        "the Claude fixture's model should appear in the merged table, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert!(
        data.per_provider
            .claude
            .contains_key("claude-sonnet-4-20250514"),
        "and be attributed to the Claude provider bucket"
    );
    assert!(
        data.provider_days.claude >= 1,
        "at least one active Claude day"
    );
}

#[test]
fn merges_multiple_providers_from_paths() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );

    let data = aggregate_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate multi");

    assert!(data.models.contains_key("claude-sonnet-4-20250514"));
    assert!(
        data.models.keys().any(|m| m.starts_with("gemini-3")),
        "a Gemini model should be present, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert!(data.provider_days.claude >= 1);
    assert!(data.provider_days.gemini >= 1);
}

#[test]
fn aggregates_grok_context_estimate_without_model_or_compaction_duplication() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");

    let data = aggregate_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate Grok");
    let usage = data.models.get("grok-test").expect("resolved Grok model");

    assert_eq!(usage["input_tokens"], 0);
    assert_eq!(usage["cache_read_input_tokens"], 12_345);
    assert!(
        !data.models.contains_key("grok-secondary"),
        "session aggregates must not be copied to every model in modelsUsed"
    );
    assert_eq!(data.per_provider.grok.get("grok-test"), Some(usage));
    assert_eq!(data.provider_days.grok, 1);
    assert_eq!(data.provider_days.total, 1);
}

#[test]
fn disabled_grok_provider_is_not_scanned() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");
    let providers = ProvidersConfig {
        grok: false,
        ..ProvidersConfig::default()
    };

    let data = aggregate_usage_from_paths_with_providers(&home.paths, TimeRange::All, providers)
        .expect("aggregate with Grok disabled");

    assert!(data.models.is_empty());
    assert!(data.per_provider.grok.is_empty());
    assert_eq!(data.provider_days.grok, 0);
}

#[test]
fn disabled_provider_is_dropped_from_usage_rollup() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );

    // Turn Gemini off in `[providers]`: it must be skipped entirely.
    let providers = ProvidersConfig {
        gemini: false,
        ..ProvidersConfig::default()
    };
    let data = aggregate_usage_from_paths_with_providers(&home.paths, TimeRange::All, providers)
        .expect("aggregate with gemini disabled");

    assert!(
        data.models.contains_key("claude-sonnet-4-20250514"),
        "the enabled Claude provider is still aggregated"
    );
    assert!(
        !data.models.keys().any(|m| m.starts_with("gemini-3")),
        "the disabled Gemini provider must not appear, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert_eq!(data.provider_days.gemini, 0, "no active Gemini days");
}

#[test]
fn test_usage_data_serialization() {
    use serde_json::json;
    use vct_core::models::usage::UsageResult;

    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 1000,
            "cost_usd": 0.05,
            "matched_model": "claude-sonnet-4"
        }),
    );

    let json = serde_json::to_string(&usage).unwrap();
    assert!(
        json.contains("claude-sonnet-4"),
        "Should contain model name"
    );

    let deserialized: UsageResult = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.len(), usage.len());
    assert!(deserialized.contains_key("claude-sonnet-4"));
}

#[test]
fn test_usage_calculation_cost_accuracy() {
    use vct_core::pricing::{ModelPricing, calculate_cost};

    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
        ..Default::default()
    };

    // 2000 cache_creation tokens, all default (5 minute) TTL, no reasoning.
    let counts = vct_core::utils::TokenCounts {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read: 10000,
        cache_creation: 2000,
        cache_creation_5m: 2000,
        ..Default::default()
    };
    let cost = calculate_cost(&counts, &pricing);

    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation (5m): 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021, "Cost calculation should be accurate");
}

#[test]
fn test_usage_with_multiple_models() {
    use serde_json::json;
    use vct_core::models::usage::UsageResult;

    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.05
        }),
    );
    usage.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "input_tokens": 2000,
            "output_tokens": 1000,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.10
        }),
    );

    assert_eq!(usage.len(), 2, "Should have two models");

    let total_cost: f64 = usage.values().filter_map(|v| v["cost_usd"].as_f64()).sum();
    assert!(
        (total_cost - 0.15).abs() < 0.001,
        "Total cost should be sum of individual costs"
    );
}

#[test]
fn test_usage_json_output_format() {
    use serde_json::{Value, json};
    use vct_core::models::usage::UsageResult;

    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 1000,
            "cost_usd": 0.05123456789,
            "matched_model": "claude-sonnet-4"
        }),
    );

    let json = serde_json::to_string_pretty(&usage).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();

    assert!(parsed.is_object(), "Root should be an object");

    let model_value = &parsed["claude-sonnet-4"];
    assert!(
        model_value["input_tokens"].is_number(),
        "input_tokens should be number"
    );
    assert!(
        model_value["output_tokens"].is_number(),
        "output_tokens should be number"
    );
    assert!(
        model_value["cost_usd"].is_number(),
        "cost_usd should be number"
    );
}

#[test]
fn test_usage_handles_missing_cache_tokens() {
    use serde_json::json;

    let usage_value = json!({
        "model": "test-model",
        "input_tokens": 1000,
        "output_tokens": 500,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cost_usd": 0.05
    });

    assert_eq!(usage_value["input_tokens"].as_i64().unwrap(), 1000);
    assert_eq!(usage_value["cache_read_input_tokens"].as_i64().unwrap(), 0);
    assert_eq!(
        usage_value["cache_creation_input_tokens"].as_i64().unwrap(),
        0
    );
}

#[test]
fn codex_archived_sessions_contribute_usage_exactly_once() {
    let rollout = fixture_str("sessions/codex.jsonl");

    let active = TempHome::new();
    active.put_codex_session(&format!("2026/06/06/{CODEX_ROLLOUT}"), &rollout);
    let expected = aggregate_usage_from_paths(&active.paths, TimeRange::All).unwrap();
    assert!(
        !expected.per_provider.codex.is_empty(),
        "the Codex fixture must contribute usage from the active root"
    );

    // Archiving moves the rollout log into the flat archived root, so a scan
    // that only reads the active one would silently lose this session.
    let archived = TempHome::new();
    archived.put_codex_archived_session(CODEX_ROLLOUT, &rollout);
    let archived_only = aggregate_usage_from_paths(&archived.paths, TimeRange::All).unwrap();
    assert_usage_data_eq(&archived_only, &expected);

    // A scan racing the archive move can see the same log under both roots.
    let both = TempHome::new();
    both.put_codex_session(&format!("2026/06/06/{CODEX_ROLLOUT}"), &rollout);
    both.put_codex_archived_session(CODEX_ROLLOUT, &rollout);
    let deduplicated = aggregate_usage_from_paths(&both.paths, TimeRange::All).unwrap();
    assert_usage_data_eq(&deduplicated, &expected);
}

#[test]
fn moving_a_codex_session_between_roots_keeps_cached_usage_stable() {
    let home = TempHome::new();
    let active = home.put_codex_session(
        &format!("2026/06/06/{CODEX_ROLLOUT}"),
        &fixture_str("sessions/codex.jsonl"),
    );
    let archived = home.paths.codex_archived_session_dir.join(CODEX_ROLLOUT);
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    let providers = ProvidersConfig::default();
    let mut cache = SummaryScanCache::new();
    let before =
        aggregate_usage_from_paths_with_cache(&home.paths, TimeRange::All, providers, &mut cache)
            .unwrap();
    assert_eq!(before.diagnostics.parsed, 1);
    assert_eq!(cache.stats().entries, 1);

    // Archiving, then un-archiving: the cache is keyed by path, so both moves
    // must re-key the entry rather than drop or duplicate the session.
    for (from, to) in [(&active, &archived), (&archived, &active)] {
        std::fs::rename(from, to).unwrap();
        let after = aggregate_usage_from_paths_with_cache(
            &home.paths,
            TimeRange::All,
            providers,
            &mut cache,
        )
        .unwrap();
        assert_eq!(after.diagnostics, before.diagnostics);
        assert_usage_data_eq(&after.data, &before.data);
        // The vacated path must be evicted, not left behind as a stale entry.
        assert_eq!(cache.stats().entries, 1);
    }
}

#[cfg(unix)]
#[test]
fn an_unreadable_codex_root_does_not_sink_the_other_one() {
    let home = TempHome::new();
    home.put_codex_session(
        &format!("2026/06/06/{CODEX_ROLLOUT}"),
        &fixture_str("sessions/codex.jsonl"),
    );
    let archived = &home.paths.codex_archived_session_dir;
    std::fs::create_dir_all(archived).unwrap();
    let original_permissions = std::fs::metadata(archived).unwrap().permissions();

    std::fs::set_permissions(archived, std::fs::Permissions::from_mode(0o0)).unwrap();
    let partial =
        aggregate_usage_from_paths_with_diagnostics(&home.paths, TimeRange::All, codex_only());
    std::fs::set_permissions(archived, original_permissions).unwrap();
    let partial = partial.unwrap();

    // One root failing is a partial failure, not an all-failed scan: the active
    // root's session still counts, and the unreadable root is reported.
    assert_eq!(partial.diagnostics.parsed, 1);
    assert_eq!(partial.diagnostics.failures.len(), 1);
    assert_eq!(partial.diagnostics.failures[0].source, *archived);
    assert!(!partial.data.per_provider.codex.is_empty());
}
