// Integration tests for analysis functionality.
//
// Single-file parsing reads the in-repo fixtures via `common::fixture` (an
// absolute, machine-stable path). Batch aggregation drives
// `aggregate_sessions_by_model_from_paths` against a `TempHome`, so it reads no
// real machine session directories and mutates no environment.

mod common;

use chrono::{Duration, Local, SecondsFormat};
use common::{TempHome, append_cursor_json_blob, fixture, fixture_str};
use rusqlite::{Connection, params};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;
use vibe_coding_tracker::analysis::aggregator::{
    AnalysisData, aggregate_sessions_by_model_from_paths,
    aggregate_sessions_by_model_from_paths_with, aggregate_sessions_by_model_from_paths_with_cache,
    aggregate_sessions_by_model_from_paths_with_diagnostics,
    collect_analysis_sessions_from_paths_with, project_code_analysis, project_session_file,
};
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::config::ProvidersConfig;
use vibe_coding_tracker::models::ExtensionType;
use vibe_coding_tracker::session::parser::{
    parse_session_file, parse_session_file_as, parse_session_file_typed,
    parse_session_file_typed_with_mode_and_diagnostics,
};
use vibe_coding_tracker::session::state::ParseMode;
use vibe_coding_tracker::session::{
    cursor::read_cursor_analysis, opencode::read_opencode_analysis,
};
use vibe_coding_tracker::summary_cache::SummaryScanCache;

fn providers_only(provider: ExtensionType) -> ProvidersConfig {
    ProvidersConfig {
        claude: provider == ExtensionType::ClaudeCode,
        codex: provider == ExtensionType::Codex,
        copilot: provider == ExtensionType::Copilot,
        gemini: provider == ExtensionType::Gemini,
        grok: provider == ExtensionType::Grok,
        opencode: provider == ExtensionType::OpenCode,
        cursor: provider == ExtensionType::Cursor,
        hermes: provider == ExtensionType::Hermes,
    }
}

fn seed_opencode_tie_breaker_db(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (
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
         VALUES ('shared-session', '/repo', 1780757089000);",
    )
    .unwrap();

    let tx = conn.transaction().unwrap();
    for input_tokens in [91, 7, 42, 3, 88, 19, 63, 5, 77, 31, 54, 11] {
        let id = format!("message-{input_tokens}");
        let data = json!({
            "role": "assistant",
            "modelID": "shared-model",
            "cost": 0,
            "tokens": {
                "input": input_tokens,
                "output": 0,
                "reasoning": 0,
                "cache": { "read": 0, "write": 0 }
            },
            "time": {
                "created": 1780757088000_i64,
                "completed": 1780757089000_i64
            }
        })
        .to_string();
        tx.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, 'shared-session', ?2)",
            params![id, data],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn seed_opencode_analysis_db(path: &Path, timestamp_ms: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
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
             );",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('session', '/repo', ?1)",
            [timestamp_ms],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message (id, session_id, data) VALUES ('message', 'session', ?1)",
            [json!({
                "role": "assistant",
                "modelID": "opencode-model",
                "cost": 0,
                "tokens": {
                    "input": 1,
                    "output": 1,
                    "reasoning": 0,
                    "cache": { "read": 0, "write": 0 }
                },
                "time": { "created": timestamp_ms, "completed": timestamp_ms }
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES ('write', 'message', 'session', ?1)",
            [json!({
                "type": "tool",
                "tool": "write",
                "state": {
                    "status": "completed",
                    "time": { "start": timestamp_ms, "end": timestamp_ms },
                    "input": { "filePath": "/repo/open.rs", "content": "one\ntwo" }
                }
            })
            .to_string()],
        )
        .unwrap();
}

fn assert_effect_fields_equal(
    actual: &vibe_coding_tracker::models::CodeAnalysis,
    expected: &vibe_coding_tracker::models::CodeAnalysis,
) {
    let actual = &actual.records[0];
    let expected = &expected.records[0];
    assert_eq!(actual.total_unique_files, expected.total_unique_files);
    assert_eq!(
        actual.total_write_characters,
        expected.total_write_characters
    );
    assert_eq!(actual.total_read_characters, expected.total_read_characters);
    assert_eq!(actual.total_edit_characters, expected.total_edit_characters);
    assert_eq!(
        serde_json::to_value(&actual.write_file_details).unwrap(),
        serde_json::to_value(&expected.write_file_details).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.read_file_details).unwrap(),
        serde_json::to_value(&expected.read_file_details).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.edit_file_details).unwrap(),
        serde_json::to_value(&expected.edit_file_details).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.run_command_details).unwrap(),
        serde_json::to_value(&expected.run_command_details).unwrap()
    );
}

fn claude_read_lifecycle(
    message_id: &str,
    tool_id: &str,
    timestamp: &str,
    path: &str,
    content: &str,
    input_tokens: i64,
) -> String {
    let assistant = json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "assistant",
        "timestamp": timestamp,
        "isSidechain": true,
        "message": {
            "id": message_id,
            "model": "claude-test-model",
            "usage": { "input_tokens": input_tokens, "output_tokens": 0 },
            "content": [{
                "type": "tool_use",
                "id": tool_id,
                "name": "Read",
                "input": { "file_path": path }
            }]
        }
    });
    let result = json!({
        "parentUuid": "assistant",
        "sessionId": "session",
        "type": "user",
        "timestamp": timestamp,
        "isSidechain": true,
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": content
            }]
        }
    });
    format!("{assistant}\n{result}\n")
}

fn codex_patch_lifecycle(call_id: &str, timestamp: &str, path: &str) -> String {
    let patch = format!("*** Begin Patch\n*** Update File: {path}\n@@\n-old\n+new\n*** End Patch");
    let invocation = json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "name": "apply_patch",
            "input": patch,
            "call_id": call_id
        }
    });
    let outcome = json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": "Done!"
        }
    });
    format!("{invocation}\n{outcome}\n")
}

fn assert_analysis_data_eq(actual: &AnalysisData, expected: &AnalysisData) {
    assert_eq!(
        serde_json::to_value(&actual.rows).unwrap(),
        serde_json::to_value(&expected.rows).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.claude).unwrap(),
        serde_json::to_value(&expected.per_provider.claude).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.codex).unwrap(),
        serde_json::to_value(&expected.per_provider.codex).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.copilot).unwrap(),
        serde_json::to_value(&expected.per_provider.copilot).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.gemini).unwrap(),
        serde_json::to_value(&expected.per_provider.gemini).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.grok).unwrap(),
        serde_json::to_value(&expected.per_provider.grok).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.opencode).unwrap(),
        serde_json::to_value(&expected.per_provider.opencode).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.per_provider.cursor).unwrap(),
        serde_json::to_value(&expected.per_provider.cursor).unwrap()
    );
    assert_eq!(
        (
            actual.provider_days.claude,
            actual.provider_days.codex,
            actual.provider_days.copilot,
            actual.provider_days.gemini,
            actual.provider_days.grok,
            actual.provider_days.opencode,
            actual.provider_days.cursor,
            actual.provider_days.hermes,
            actual.provider_days.total,
        ),
        (
            expected.provider_days.claude,
            expected.provider_days.codex,
            expected.provider_days.copilot,
            expected.provider_days.gemini,
            expected.provider_days.grok,
            expected.provider_days.opencode,
            expected.provider_days.cursor,
            expected.provider_days.hermes,
            expected.provider_days.total,
        )
    );
}

#[test]
fn test_single_file_analysis_claude() {
    let analysis = parse_session_file(fixture("sessions/claude_code.jsonl"))
        .expect("should successfully analyze Claude file");

    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert!(analysis["records"].is_array(), "Should have records array");
}

#[test]
fn test_single_file_analysis_codex() {
    let analysis = parse_session_file(fixture("sessions/codex.jsonl"))
        .expect("should successfully analyze Codex file");
    assert_eq!(analysis["extensionName"], "Codex");
}

#[test]
fn test_single_file_analysis_copilot() {
    let analysis = parse_session_file(fixture("sessions/copilot.jsonl"))
        .expect("should successfully analyze Copilot file");
    assert_eq!(analysis["extensionName"], "Copilot-CLI");
}

#[test]
fn test_single_file_analysis_gemini() {
    let analysis = parse_session_file(fixture("sessions/gemini.jsonl"))
        .expect("should successfully analyze Gemini file");
    assert_eq!(analysis["extensionName"], "Gemini");
}

#[test]
fn test_single_file_analysis_grok() {
    let analysis = parse_session_file(fixture("sessions/grok/signals.json"))
        .expect("should successfully analyze Grok file");
    assert_eq!(analysis["extensionName"], "Grok");
    let record = &analysis["records"][0];
    assert_eq!(record["conversationUsage"]["grok-test"]["input_tokens"], 0);
    assert_eq!(
        record["conversationUsage"]["grok-test"]["cache_read_input_tokens"],
        12_345
    );
    // Tool counts represent invocations, including the failed grep and write.
    // Failed calls do not contribute file effects or line totals.
    assert_eq!(record["toolCallCounts"]["Read"], 3);
    assert_eq!(record["toolCallCounts"]["Write"], 2);
    assert_eq!(record["toolCallCounts"]["Edit"], 1);
    assert_eq!(record["toolCallCounts"]["Bash"], 1);
    assert_eq!(record["toolCallCounts"]["TodoWrite"], 1);
    assert!(record["conversationUsage"].get("grok-secondary").is_none());
}

#[test]
fn batch_analysis_attributes_grok_tools_to_the_grok_provider() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");

    let data = aggregate_sessions_by_model_from_paths(&home.paths, TimeRange::All)
        .expect("aggregate Grok analysis");
    let row = data
        .rows
        .iter()
        .find(|row| row.model == "grok-test")
        .expect("Grok model row");
    assert_eq!(row.read_count, 3);
    assert_eq!(row.write_count, 2);
    assert_eq!(row.edit_count, 1);
    assert_eq!(row.bash_count, 1);
    assert_eq!(row.todo_write_count, 1);
    assert_eq!(data.per_provider.grok.len(), 1);
    assert_eq!(data.provider_days.grok, 1);
}

#[test]
fn disabled_grok_provider_is_not_scanned_for_analysis() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");
    let providers = ProvidersConfig {
        grok: false,
        ..ProvidersConfig::default()
    };

    let data = aggregate_sessions_by_model_from_paths_with(&home.paths, TimeRange::All, providers)
        .expect("aggregate with Grok disabled");
    assert!(data.rows.is_empty());
    assert!(data.per_provider.grok.is_empty());
    assert_eq!(data.provider_days.grok, 0);
}

#[test]
fn test_analysis_record_structure() {
    let analysis = parse_session_file(fixture("sessions/claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records
        .as_array()
        .and_then(|arr| arr.first())
        .expect("fixture has at least one record");

    assert!(
        first_record["conversationUsage"].is_object(),
        "Should have conversationUsage"
    );
    assert!(
        first_record["toolCallCounts"].is_object(),
        "Should have toolCallCounts"
    );
    assert!(first_record["taskId"].is_string(), "Should have taskId");
    assert!(
        first_record["timestamp"].is_number(),
        "Should have timestamp"
    );
}

#[test]
fn test_analysis_conversation_usage() {
    let analysis = parse_session_file(fixture("sessions/claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();
    let usage = &first_record["conversationUsage"];

    assert!(
        usage.as_object().map(|o| !o.is_empty()).unwrap_or(false),
        "Should have at least one model in conversationUsage"
    );

    for (model_name, model_usage) in usage.as_object().unwrap() {
        assert!(!model_name.is_empty(), "Model name should not be empty");
        assert!(
            model_usage["input_tokens"].is_number(),
            "Should have input_tokens"
        );
        assert!(
            model_usage["output_tokens"].is_number(),
            "Should have output_tokens"
        );
    }
}

#[test]
fn test_analysis_tool_call_counts() {
    let analysis = parse_session_file(fixture("sessions/claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();
    let counts = &first_record["toolCallCounts"];

    assert!(counts.is_object(), "toolCallCounts should be an object");
    for (_tool, count) in counts.as_object().unwrap() {
        assert!(count.is_number(), "Tool count should be a number");
    }
}

#[test]
fn test_analysis_file_operations() {
    let analysis = parse_session_file(fixture("sessions/claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();

    assert!(
        first_record["editFileDetails"].is_array() || first_record["editFileDetails"].is_null()
    );
    assert!(
        first_record["readFileDetails"].is_array() || first_record["readFileDetails"].is_null()
    );
    assert!(
        first_record["writeFileDetails"].is_array() || first_record["writeFileDetails"].is_null()
    );
    assert!(
        first_record["runCommandDetails"].is_array() || first_record["runCommandDetails"].is_null()
    );

    assert!(first_record["totalEditLines"].is_number());
    assert!(first_record["totalReadLines"].is_number());
    assert!(first_record["totalWriteLines"].is_number());
}

#[test]
fn disabled_provider_is_dropped_from_analysis_rollup() {
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
    let data = aggregate_sessions_by_model_from_paths_with(&home.paths, TimeRange::All, providers)
        .expect("aggregate with gemini disabled");

    assert!(
        data.rows
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514"),
        "the enabled Claude provider is still aggregated"
    );
    assert!(
        !data.rows.iter().any(|r| r.model.starts_with("gemini-3")),
        "the disabled Gemini provider must not appear, got: {:?}",
        data.rows.iter().map(|r| &r.model).collect::<Vec<_>>()
    );
}

#[test]
fn batch_analysis_from_paths_groups_by_model() {
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

    let data = aggregate_sessions_by_model_from_paths(&home.paths, TimeRange::All)
        .expect("batch aggregation should succeed");

    // Every row has a non-empty model name and rows are sorted.
    for row in &data.rows {
        assert!(!row.model.is_empty(), "Model should not be empty");
    }
    for i in 1..data.rows.len() {
        assert!(
            data.rows[i - 1].model <= data.rows[i].model,
            "Models should be sorted alphabetically"
        );
    }

    // The Claude fixture's model is grouped and attributed to the Claude bucket.
    assert!(
        data.rows
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514"),
        "Claude fixture model should have a row, got: {:?}",
        data.rows.iter().map(|r| &r.model).collect::<Vec<_>>()
    );
    assert!(
        data.per_provider
            .claude
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514")
    );
    assert!(
        data.per_provider
            .gemini
            .iter()
            .any(|r| r.model.starts_with("gemini-3"))
    );

    let max_provider_days = data
        .provider_days
        .claude
        .max(data.provider_days.codex)
        .max(data.provider_days.copilot)
        .max(data.provider_days.gemini);
    assert!(data.provider_days.total >= max_provider_days);
    assert!(data.provider_days.claude >= 1 && data.provider_days.gemini >= 1);
}

#[test]
fn cached_analysis_matches_uncached_and_reuses_unchanged_sources() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
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
        "proj-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );
    home.put_grok_fixture_session("workspace", "grok-session");
    seed_opencode_tie_breaker_db(&home.paths.opencode_db);
    home.put_cursor_session(
        "cursor-project",
        "cursor-conversation",
        "cursor-model",
        1_780_757_089_000,
        1_234,
    );
    let providers = ProvidersConfig::default();
    let uncached = aggregate_sessions_by_model_from_paths_with_diagnostics(
        &home.paths,
        TimeRange::All,
        providers,
    )
    .unwrap();

    let mut cache = SummaryScanCache::new();
    let cold = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 7);
    assert_eq!(cold.diagnostics.candidates, 7);
    assert_eq!(cold.diagnostics.parsed, 7);
    assert!(cold.diagnostics.failures.is_empty());
    assert_eq!(cold.diagnostics, uncached.diagnostics);
    assert_analysis_data_eq(&cold.data, &uncached.data);
    for (provider, rows) in [
        ("Claude", &cold.data.per_provider.claude),
        ("Codex", &cold.data.per_provider.codex),
        ("Copilot", &cold.data.per_provider.copilot),
        ("Gemini", &cold.data.per_provider.gemini),
        ("Grok", &cold.data.per_provider.grok),
        ("OpenCode", &cold.data.per_provider.opencode),
        ("Cursor", &cold.data.per_provider.cursor),
    ] {
        assert!(!rows.is_empty(), "{provider} fixture must contribute a row");
    }
    for rows in [
        &cold.data.rows,
        &cold.data.per_provider.claude,
        &cold.data.per_provider.codex,
        &cold.data.per_provider.copilot,
        &cold.data.per_provider.gemini,
        &cold.data.per_provider.grok,
        &cold.data.per_provider.opencode,
        &cold.data.per_provider.cursor,
    ] {
        assert!(
            rows.windows(2).all(|pair| pair[0].model <= pair[1].model),
            "cached rows must retain deterministic model ordering"
        );
    }

    let warm = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_analysis_data_eq(&warm.data, &uncached.data);
}

#[cfg(unix)]
#[test]
fn analysis_cache_preserves_entries_after_partial_directory_discovery() {
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
    let providers = providers_only(ExtensionType::ClaudeCode);
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().entries, 2);
    assert_eq!(cache.stats().parsed_sources, 2);

    std::fs::set_permissions(hidden_dir, std::fs::Permissions::from_mode(0o0)).unwrap();
    let partial = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
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

    let restored = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_eq!(cache.stats().entries, 2);
    assert!(restored.diagnostics.failures.is_empty());
    assert_analysis_data_eq(&restored.data, &cold.data);
}

#[test]
fn deterministic_analysis_sqlite_schema_failure_is_cached() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    Connection::open(&home.paths.opencode_db)
        .unwrap()
        .execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY);")
        .unwrap();
    let providers = providers_only(ExtensionType::OpenCode);
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert!(cold.diagnostics.all_failed());
    assert_eq!(cache.stats().parsed_sources, 1);

    let warm = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers,
        &mut cache,
    )
    .unwrap();
    assert!(warm.diagnostics.all_failed());
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_eq!(cache.stats().parsed_sources, 0);
}

#[test]
fn cursor_tracking_failure_is_not_an_analysis_candidate() {
    let home = TempHome::new();
    std::fs::create_dir_all(&home.paths.cursor_chats_dir).unwrap();
    std::fs::create_dir_all(home.paths.cursor_tracking_db.parent().unwrap()).unwrap();
    std::fs::write(&home.paths.cursor_tracking_db, "not SQLite").unwrap();

    let result = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        &mut SummaryScanCache::new(),
    )
    .unwrap();
    assert_eq!(result.diagnostics.candidates, 0);
    assert_eq!(result.diagnostics.parsed, 0);
    assert_eq!(result.diagnostics.failures.len(), 1);
    assert!(!result.diagnostics.all_failed());
}

#[test]
fn cursor_analysis_cache_invalidates_only_changed_stores() {
    let home = TempHome::new();
    let first = home.put_cursor_session("project", "first", "cursor-first", 1_780_757_089_000, 100);
    let second =
        home.put_cursor_session("project", "second", "cursor-second", 1_780_757_090_000, 200);
    let mut cache = SummaryScanCache::new();

    let cold = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 2);
    assert_eq!(cache.stats().entries, 2);
    assert_eq!(cold.diagnostics.candidates, 2);
    assert_eq!(cold.diagnostics.parsed, 2);

    let warm = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 0);
    assert_analysis_data_eq(&warm.data, &cold.data);

    append_cursor_json_blob(&first, "mutation");
    let changed = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(cache.stats().entries, 2);
    assert_analysis_data_eq(&changed.data, &cold.data);

    home.put_cursor_session("project", "third", "cursor-third", 1_780_757_091_000, 300);
    let added = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cache.stats().parsed_sources, 1);
    assert_eq!(cache.stats().entries, 3);
    assert!(
        added
            .data
            .per_provider
            .cursor
            .iter()
            .any(|row| row.model == "cursor-third")
    );

    std::fs::remove_file(second).unwrap();
    let deleted = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
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
            .iter()
            .any(|row| row.model == "cursor-second")
    );
}

#[test]
fn canonical_dataset_serializes_as_full_code_analysis_objects_in_provider_order() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_codex_session(
        "2026/04/23/rollout.jsonl",
        &fixture_str("sessions/codex.jsonl"),
    );
    home.put(
        ".copilot/session-state/test-session/events.jsonl",
        &fixture_str("sessions/copilot.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );
    home.put_grok_fixture_session("workspace", "grok-session");

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        ProvidersConfig::default(),
        ParseMode::Full,
    )
    .expect("collect canonical analysis dataset");

    let extensions: Vec<&str> = dataset
        .sessions
        .iter()
        .map(|session| session.analysis.extension_name.as_str())
        .collect();
    assert_eq!(
        extensions,
        ["Claude-Code", "Codex", "Copilot-CLI", "Gemini", "Grok"]
    );
    assert_eq!(dataset.diagnostics.candidates, 5);
    assert_eq!(dataset.diagnostics.parsed, 5);
    assert!(!dataset.diagnostics.has_failures());

    let serialized = serde_json::to_value(&dataset).expect("serialize canonical dataset");
    let expected = serde_json::Value::Array(
        dataset
            .sessions
            .iter()
            .map(|session| serde_json::to_value(&session.analysis).unwrap())
            .collect(),
    );
    assert_eq!(serialized, expected);

    let sessions = serialized.as_array().unwrap();
    assert!(sessions.iter().all(|session| session["records"].is_array()));
    assert!(
        sessions
            .iter()
            .all(|session| session.get("provider").is_none() && session.get("date").is_none())
    );
    assert!(sessions.iter().any(|session| {
        session["records"].as_array().is_some_and(|records| {
            records.iter().any(|record| {
                record["writeFileDetails"]
                    .as_array()
                    .is_some_and(|details| !details.is_empty())
            })
        })
    }));

    let second = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        ProvidersConfig::default(),
        ParseMode::Full,
    )
    .expect("collect deterministic dataset again");
    assert_eq!(
        serde_json::to_string(&second).unwrap(),
        serde_json::to_string(&dataset).unwrap(),
        "repeated collection should be byte-for-byte deterministic"
    );
}

#[test]
fn all_time_batch_json_preserves_non_claude_full_effects() {
    let home = TempHome::new();
    let codex = home.put_codex_session(
        "2026/04/23/rollout.jsonl",
        &fixture_str("sessions/codex.jsonl"),
    );
    let copilot = home.put(
        ".copilot/session-state/test-session/events.jsonl",
        &fixture_str("sessions/copilot.jsonl"),
    );
    let gemini = home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("sessions/gemini.jsonl"),
    );
    let grok = home.put_grok_fixture_session("workspace", "grok-session");

    for (provider, path) in [
        (ExtensionType::Codex, codex),
        (ExtensionType::Copilot, copilot),
        (ExtensionType::Gemini, gemini),
        (ExtensionType::Grok, grok),
    ] {
        let expected = parse_session_file_as(&path, provider, ParseMode::Full).unwrap();
        let dataset = collect_analysis_sessions_from_paths_with(
            &home.paths,
            TimeRange::All,
            providers_only(provider),
            ParseMode::Full,
        )
        .unwrap();
        assert_eq!(dataset.len(), 1, "provider={provider}");
        assert_effect_fields_equal(&dataset.sessions[0].analysis, &expected);
        let compact = collect_analysis_sessions_from_paths_with(
            &home.paths,
            TimeRange::All,
            providers_only(provider),
            ParseMode::UsageOnly,
        )
        .unwrap();
        assert_eq!(
            compact.sessions[0].analysis.records[0].total_unique_files,
            expected.records[0].total_unique_files,
            "UsageOnly unique files drifted for provider={provider}"
        );
    }

    let timestamp_ms = Local::now().timestamp_millis();
    seed_opencode_analysis_db(&home.paths.opencode_db, timestamp_ms);
    let expected = read_opencode_analysis(&home.paths.opencode_db, TimeRange::All, ParseMode::Full)
        .unwrap()
        .remove(0)
        .1;
    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::OpenCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    assert_effect_fields_equal(&dataset.sessions[0].analysis, &expected);
    let compact = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::OpenCode),
        ParseMode::UsageOnly,
    )
    .unwrap();
    assert_eq!(
        compact.sessions[0].analysis.records[0].total_unique_files,
        expected.records[0].total_unique_files
    );

    let cursor_store = home.put_cursor_session(
        "cursor-project",
        "cursor-conversation",
        "cursor-model",
        timestamp_ms,
        123,
    );
    let cursor_result = json!({
        "role": "tool",
        "content": [{
            "type": "tool-result",
            "toolName": "Write",
            "toolCallId": "write",
            "result": "ok"
        }]
    })
    .to_string()
    .into_bytes();
    Connection::open(cursor_store)
        .unwrap()
        .execute(
            "INSERT INTO blobs (id, data) VALUES ('result', ?1)",
            params![cursor_result],
        )
        .unwrap();
    let expected = read_cursor_analysis(
        &home.paths.cursor_chats_dir,
        &home.paths.cursor_tracking_db,
        TimeRange::All,
        ParseMode::Full,
    )
    .unwrap()
    .remove(0)
    .1;
    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    assert_effect_fields_equal(&dataset.sessions[0].analysis, &expected);
    let compact = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        ParseMode::UsageOnly,
    )
    .unwrap();
    assert_eq!(
        compact.sessions[0].analysis.records[0].total_unique_files,
        expected.records[0].total_unique_files
    );
}

#[test]
fn batch_json_filters_claude_usage_and_tools_by_event_time_within_one_source() {
    let home = TempHome::new();
    let old = (Local::now() - Duration::days(10)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let today = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let contents = format!(
        "{}{}",
        claude_read_lifecycle(
            "old-message",
            "old-tool",
            &old,
            "/repo/old.rs",
            "old\ncontent",
            10,
        ),
        claude_read_lifecycle(
            "today-message",
            "today-tool",
            &today,
            "/repo/today.rs",
            "one\ntwo\nthree",
            20,
        ),
    );
    let source = home.put_claude_session("project", "resume.jsonl", &contents);
    std::fs::File::open(source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::Daily,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    let record = &dataset.sessions[0].analysis.records[0];
    assert_eq!(record.tool_call_counts.read, 1);
    assert_eq!(record.total_read_lines, 3);
    assert_eq!(record.total_unique_files, 1);
    assert_eq!(record.read_file_details.len(), 1);
    assert_eq!(record.read_file_details[0].base.file_path, "/repo/today.rs");
    assert_eq!(
        record.conversation_usage["claude-test-model"]["input_tokens"],
        20
    );
}

#[test]
fn batch_record_timestamp_uses_latest_lifecycle_observation() {
    let home = TempHome::new();
    let invocation =
        (Local::now() - Duration::minutes(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let outcome =
        (Local::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let assistant = json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "assistant",
        "timestamp": &invocation,
        "message": {
            "id": "message",
            "model": "claude-test",
            "usage": { "input_tokens": 1, "output_tokens": 0 },
            "content": [{
                "type": "tool_use",
                "id": "read",
                "name": "Read",
                "input": { "file_path": "/tmp/a" }
            }]
        }
    });
    let result = json!({
        "parentUuid": "assistant",
        "sessionId": "session",
        "type": "user",
        "timestamp": &outcome,
        "isSidechain": true,
        "message": { "content": [{
            "type": "tool_result",
            "tool_use_id": "read",
            "content": "one"
        }] }
    });
    let source = home.put_claude_session(
        "project",
        "lifecycle.jsonl",
        &format!("{assistant}\n{result}\n"),
    );
    let direct =
        parse_session_file_as(&source, ExtensionType::ClaudeCode, ParseMode::Full).unwrap();

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();

    assert_eq!(
        dataset.sessions[0].analysis.records[0].timestamp,
        direct.records[0].timestamp
    );
    assert_eq!(
        dataset.sessions[0].analysis.records[0].timestamp,
        vibe_coding_tracker::utils::parse_iso_timestamp(&outcome)
    );
}

#[test]
fn batch_json_filters_codex_full_patch_effects_by_invocation_time() {
    let home = TempHome::new();
    let old = (Local::now() - Duration::days(10)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let today = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let turn_context = json!({
        "timestamp": &old,
        "type": "turn_context",
        "payload": { "model": "gpt-test" }
    });
    let contents = format!(
        "{turn_context}\n{}{}",
        codex_patch_lifecycle("old-patch", &old, "src/old.rs"),
        codex_patch_lifecycle("today-patch", &today, "src/today.rs"),
    );
    home.put_codex_session("2026/07/16/rollout.jsonl", &contents);

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::Daily,
        providers_only(ExtensionType::Codex),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    let record = &dataset.sessions[0].analysis.records[0];
    assert_eq!(record.tool_call_counts.edit, 1);
    assert_eq!(record.total_edit_lines, 1);
    assert_eq!(record.total_edit_characters, 3);
    assert_eq!(record.total_unique_files, 1);
    assert_eq!(record.edit_file_details.len(), 1);
    assert_eq!(record.edit_file_details[0].base.file_path, "src/today.rs");
    assert_eq!(record.edit_file_details[0].old_string, "old");
    assert_eq!(record.edit_file_details[0].new_string, "new");
}

#[test]
fn unnormalizable_codex_fact_does_not_bypass_cross_source_deduplication() {
    let home = TempHome::new();
    let timestamp = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let context = json!({
        "timestamp": &timestamp,
        "type": "turn_context",
        "payload": { "model": "gpt-test" }
    });
    let patch_event = |call_id: &str, change_type: &str, path: &str| {
        json!({
            "timestamp": &timestamp,
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "call_id": call_id,
                "success": true,
                "status": "completed",
                "stdout": "Success. Updated the following files:",
                "stderr": "",
                "changes": {
                    (path): {
                        "type": change_type,
                        "unified_diff": "@@ -1 +1 @@\n-old\n+new\n",
                        "move_path": null
                    }
                }
            }
        })
    };
    let duplicate = patch_event("duplicate", "update", "/tmp/duplicate.rs");
    home.put_codex_session(
        "2026/07/12/a-canonical.jsonl",
        &format!("{context}\n{duplicate}\n"),
    );
    let unsupported = patch_event("unsupported", "future_change", "/tmp/future.rs");
    home.put_codex_session(
        "2026/07/12/b-contains-fallback.jsonl",
        &format!("{context}\n{unsupported}\n{duplicate}\n"),
    );

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Codex),
        ParseMode::Full,
    )
    .unwrap();
    let edit_count: usize = dataset
        .sessions
        .iter()
        .map(|session| session.analysis.records[0].tool_call_counts.edit)
        .sum();
    assert_eq!(edit_count, 2);
}

#[test]
fn batch_json_deduplicates_split_claude_message_and_tool_lifecycles_across_sources() {
    let home = TempHome::new();
    let invocation_time =
        (Local::now() - Duration::minutes(2)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let outcome_time =
        (Local::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let invocation = json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "assistant",
        "timestamp": invocation_time,
        "message": {
            "id": "shared-message",
            "model": "claude-test-model",
            "usage": { "input_tokens": 10, "output_tokens": 0 },
            "content": [{
                "type": "tool_use",
                "id": "shared-tool",
                "name": "Read",
                "input": { "file_path": "/repo/shared.rs" }
            }]
        }
    });
    home.put_claude_session("project", "a-invocation.jsonl", &format!("{invocation}\n"));

    let usage_snapshot = json!({
        "parentUuid": null,
        "sessionId": "session",
        "type": "assistant",
        "timestamp": &outcome_time,
        "message": {
            "id": "shared-message",
            "model": "claude-test-model",
            "usage": { "input_tokens": 20, "output_tokens": 0 },
            "content": []
        }
    });
    let outcome = json!({
        "parentUuid": "assistant",
        "sessionId": "session",
        "type": "user",
        "timestamp": &outcome_time,
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": "shared-tool",
                "content": "one\ntwo"
            }]
        },
        "toolUseResult": {
            "type": "text",
            "file": { "filePath": "/repo/shared.rs", "content": "one\ntwo" }
        }
    });
    home.put_claude_session(
        "project",
        "b-outcome.jsonl",
        &format!("{usage_snapshot}\n{outcome}\n"),
    );

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 2);
    let invocation_record = &dataset.sessions[0].analysis.records[0];
    assert_eq!(invocation_record.tool_call_counts.read, 1);
    assert_eq!(invocation_record.total_read_lines, 2);
    assert_eq!(invocation_record.total_read_characters, 7);
    assert_eq!(invocation_record.total_unique_files, 1);
    assert_eq!(invocation_record.read_file_details.len(), 1);
    assert_eq!(
        dataset
            .sessions
            .iter()
            .map(|session| session.analysis.records[0].tool_call_counts.read)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        dataset
            .sessions
            .iter()
            .map(|session| session.analysis.records[0].read_file_details.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(
        dataset
            .sessions
            .iter()
            .filter_map(|session| {
                session.analysis.records[0]
                    .conversation_usage
                    .get("claude-test-model")
                    .and_then(|usage| usage["input_tokens"].as_i64())
            })
            .sum::<i64>(),
        20
    );
    let summarized = dataset.summarize();
    assert_eq!(summarized.rows.len(), 1);
    assert_eq!(summarized.rows[0].model, "claude-test-model");
    assert_eq!(summarized.rows[0].read_count, 1);
    assert_eq!(summarized.rows[0].read_lines, 2);
}

#[test]
fn gemini_revised_snapshots_keep_each_tool_independent_of_file_order() {
    let timestamp = Local::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let meta = json!({
        "sessionId": "session",
        "projectHash": "project",
        "startTime": &timestamp,
        "lastUpdated": &timestamp
    });
    let revision = |tools: serde_json::Value| {
        json!({
            "id": "shared-message",
            "timestamp": &timestamp,
            "type": "gemini",
            "model": "gemini-test",
            "tokens": { "input": 1, "output": 0, "cached": 0, "thoughts": 0, "tool": 0, "total": 1 },
            "toolCalls": tools
        })
    };
    let first = revision(json!([{
        "id": "write-a",
        "name": "write_file",
        "status": "success",
        "timestamp": &timestamp,
        "args": { "file_path": "/tmp/a", "content": "a" }
    }]));
    let latest = revision(json!([
        {
            "id": "write-a",
            "name": "write_file",
            "status": "success",
            "timestamp": &timestamp,
            "args": { "file_path": "/tmp/a", "content": "a" }
        },
        {
            "id": "write-b",
            "name": "write_file",
            "status": "success",
            "timestamp": &timestamp,
            "args": { "file_path": "/tmp/b", "content": "b" }
        }
    ]));

    for swap in [false, true] {
        let home = TempHome::new();
        let (a, b) = if swap {
            (&latest, &first)
        } else {
            (&first, &latest)
        };
        home.put_gemini_session("project", "a.jsonl", &format!("{meta}\n{a}\n"));
        home.put_gemini_session("project", "b.jsonl", &format!("{meta}\n{b}\n"));

        let dataset = collect_analysis_sessions_from_paths_with(
            &home.paths,
            TimeRange::All,
            providers_only(ExtensionType::Gemini),
            ParseMode::Full,
        )
        .unwrap();
        let write_count: usize = dataset
            .sessions
            .iter()
            .map(|session| session.analysis.records[0].tool_call_counts.write)
            .sum();
        let unique_files: usize = dataset
            .sessions
            .iter()
            .map(|session| session.analysis.records[0].total_unique_files)
            .sum();
        assert_eq!(write_count, 2, "swap={swap}");
        assert_eq!(unique_files, 2, "swap={swap}");
        let row = aggregate_sessions_by_model_from_paths_with_diagnostics(
            &home.paths,
            TimeRange::All,
            providers_only(ExtensionType::Gemini),
        )
        .unwrap()
        .data
        .rows
        .into_iter()
        .find(|row| row.model == "gemini-test")
        .unwrap();
        assert_eq!(row.write_count, 2, "swap={swap}");
    }
}

#[test]
fn gemini_tool_timestamp_controls_bounded_analysis_range() {
    let home = TempHome::new();
    let old = (Local::now() - Duration::days(10)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let recent = Local::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let meta = json!({
        "sessionId": "session",
        "projectHash": "project",
        "startTime": &old,
        "lastUpdated": &recent
    });
    let message = json!({
        "id": "message",
        "timestamp": &old,
        "type": "gemini",
        "model": "gemini-test",
        "toolCalls": [{
            "id": "recent-write",
            "name": "write_file",
            "status": "success",
            "timestamp": &recent,
            "args": { "file_path": "/tmp/recent", "content": "recent" }
        }]
    });
    home.put_gemini_session("project", "chat.jsonl", &format!("{meta}\n{message}\n"));

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::Daily,
        providers_only(ExtensionType::Gemini),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    assert_eq!(
        dataset.sessions[0].analysis.records[0]
            .tool_call_counts
            .write,
        1
    );
}

#[test]
fn conversation_usage_keys_have_a_stable_serialization_order() {
    let mut first = parse_session_file_as(
        fixture("sessions/claude_code.jsonl"),
        ExtensionType::ClaudeCode,
        ParseMode::Full,
    )
    .unwrap();
    let mut second = first.clone();

    first.records[0].conversation_usage.clear();
    first.records[0]
        .conversation_usage
        .insert("zeta".to_string(), json!(3));
    first.records[0]
        .conversation_usage
        .insert("alpha".to_string(), json!(1));
    first.records[0]
        .conversation_usage
        .insert("middle".to_string(), json!(2));

    second.records[0].conversation_usage.clear();
    second.records[0]
        .conversation_usage
        .insert("middle".to_string(), json!(2));
    second.records[0]
        .conversation_usage
        .insert("zeta".to_string(), json!(3));
    second.records[0]
        .conversation_usage
        .insert("alpha".to_string(), json!(1));

    let first_json = serde_json::to_string(&first).unwrap();
    let second_json = serde_json::to_string(&second).unwrap();
    assert_eq!(first_json, second_json);
    assert!(first_json.contains(r#""conversationUsage":{"alpha":1,"middle":2,"zeta":3}"#));
}

#[test]
fn opencode_sessions_use_stable_source_identity_for_ties() {
    let home = TempHome::new();
    seed_opencode_tie_breaker_db(&home.paths.opencode_db);
    let providers = providers_only(ExtensionType::OpenCode);

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers,
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.sessions.len(), 12);
    assert_eq!(dataset.diagnostics.candidates, 1);
    assert_eq!(dataset.diagnostics.parsed, 1);

    let input_tokens: Vec<i64> = dataset
        .sessions
        .iter()
        .map(|session| {
            session.analysis.records[0].conversation_usage["shared-model"]["input_tokens"]
                .as_i64()
                .unwrap()
        })
        .collect();
    assert_eq!(input_tokens, [11, 19, 3, 31, 42, 5, 54, 63, 7, 77, 88, 91]);

    let canonical = serde_json::to_string(&dataset).unwrap();
    for _ in 0..5 {
        let next = collect_analysis_sessions_from_paths_with(
            &home.paths,
            TimeRange::All,
            providers,
            ParseMode::Full,
        )
        .unwrap();
        assert_eq!(serde_json::to_string(&next).unwrap(), canonical);
    }
}

#[test]
fn full_and_usage_only_modes_have_identical_scalar_analysis() {
    let fixtures = [
        ("sessions/claude_code.jsonl", ExtensionType::ClaudeCode),
        ("sessions/codex.jsonl", ExtensionType::Codex),
        ("sessions/copilot.jsonl", ExtensionType::Copilot),
        ("sessions/gemini.jsonl", ExtensionType::Gemini),
        ("sessions/grok/signals.json", ExtensionType::Grok),
    ];

    for (name, provider) in fixtures {
        let path = fixture(name);
        let full = parse_session_file_as(&path, provider, ParseMode::Full).unwrap();
        let summary = parse_session_file_as(&path, provider, ParseMode::UsageOnly).unwrap();
        assert_eq!(full.records.len(), summary.records.len(), "fixture={name}");

        for (full, summary) in full.records.iter().zip(&summary.records) {
            assert_eq!(
                full.total_unique_files, summary.total_unique_files,
                "totalUniqueFiles fixture={name}"
            );
            assert_eq!(full.total_write_lines, summary.total_write_lines, "{name}");
            assert_eq!(full.total_read_lines, summary.total_read_lines, "{name}");
            assert_eq!(full.total_edit_lines, summary.total_edit_lines, "{name}");
            assert_eq!(
                full.total_write_characters, summary.total_write_characters,
                "{name}"
            );
            assert_eq!(
                full.total_read_characters, summary.total_read_characters,
                "{name}"
            );
            assert_eq!(
                full.total_edit_characters, summary.total_edit_characters,
                "{name}"
            );
            assert_eq!(
                full.tool_call_counts.read, summary.tool_call_counts.read,
                "{name}"
            );
            assert_eq!(
                full.tool_call_counts.write, summary.tool_call_counts.write,
                "{name}"
            );
            assert_eq!(
                full.tool_call_counts.edit, summary.tool_call_counts.edit,
                "{name}"
            );
            assert_eq!(
                full.tool_call_counts.todo_write, summary.tool_call_counts.todo_write,
                "{name}"
            );
            assert_eq!(
                full.tool_call_counts.bash, summary.tool_call_counts.bash,
                "{name}"
            );
            assert_eq!(
                full.conversation_usage, summary.conversation_usage,
                "{name}"
            );
            assert_eq!(full.task_id, summary.task_id, "{name}");
            assert_eq!(full.timestamp, summary.timestamp, "{name}");
            assert_eq!(full.folder_path, summary.folder_path, "{name}");
            assert_eq!(full.git_remote_url, summary.git_remote_url, "{name}");
            assert!(summary.write_file_details.is_empty(), "{name}");
            assert!(summary.read_file_details.is_empty(), "{name}");
            assert!(summary.edit_file_details.is_empty(), "{name}");
            assert!(summary.run_command_details.is_empty(), "{name}");
        }
    }
}

#[test]
fn single_code_analysis_uses_the_batch_projection() {
    let analysis = parse_session_file_as(
        fixture("sessions/claude_code.jsonl"),
        ExtensionType::ClaudeCode,
        ParseMode::Full,
    )
    .unwrap();

    let projected = project_code_analysis(&analysis);
    assert!(
        projected
            .rows
            .iter()
            .any(|row| row.model == "claude-sonnet-4-20250514")
    );
    assert_eq!(projected.provider_days.claude, 1);
    assert_eq!(projected.provider_days.total, 1);
    assert_eq!(projected.per_provider.claude.len(), projected.rows.len());
    for (provider, overall) in projected.per_provider.claude.iter().zip(&projected.rows) {
        assert_eq!(provider.model, overall.model);
        assert_eq!(provider.read_lines, overall.read_lines);
        assert_eq!(provider.write_lines, overall.write_lines);
        assert_eq!(provider.edit_lines, overall.edit_lines);
    }
}

#[test]
fn batch_analysis_from_empty_paths_is_empty() {
    let home = TempHome::new();
    let data = aggregate_sessions_by_model_from_paths(&home.paths, TimeRange::All).unwrap();
    assert!(data.rows.is_empty(), "no sessions -> no rows");
    assert_eq!(data.provider_days.total, 0);
}

#[test]
fn collection_diagnostics_distinguish_all_failed_from_no_candidates() {
    let home = TempHome::new();
    let invalid = home.put_claude_session("proj", "invalid.jsonl", "not valid json\n");
    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();

    assert!(dataset.is_empty());
    assert_eq!(dataset.diagnostics.candidates, 1);
    assert_eq!(dataset.diagnostics.parsed, 0);
    assert!(dataset.diagnostics.all_failed());
    assert!(!dataset.diagnostics.partially_failed());
    assert_eq!(dataset.diagnostics.failures.len(), 1);
    assert_eq!(dataset.diagnostics.failures[0].source, invalid);
    assert_eq!(
        dataset.diagnostics.failures[0].provider,
        ExtensionType::ClaudeCode
    );

    let empty = TempHome::new();
    let no_candidates = collect_analysis_sessions_from_paths_with(
        &empty.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(no_candidates.diagnostics.candidates, 0);
    assert!(!no_candidates.diagnostics.all_failed());
}

#[test]
fn collection_diagnostics_reject_completely_unknown_provider_schema() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "future.jsonl",
        r#"{"type":"future.claude.event","timestamp":"2026-07-12T00:00:00Z"}"#,
    );

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert!(dataset.is_empty());
    assert!(dataset.diagnostics.all_failed());
    assert_eq!(dataset.diagnostics.failures.len(), 1);
    assert!(
        dataset.diagnostics.failures[0]
            .error
            .contains("no recognized provider records")
    );
}

#[test]
fn collection_diagnostics_warn_after_a_recognized_header_and_future_event() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "future.jsonl",
        concat!(
            r#"{"type":"permission-mode","parentUuid":"root","timestamp":"2026-07-12T00:00:00Z"}"#,
            "\n",
            r#"{"type":"future.claude.event","timestamp":"2026-07-12T00:00:01Z"}"#,
            "\n"
        ),
    );

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    assert_eq!(dataset.diagnostics.parsed, 1);
    assert!(dataset.diagnostics.partially_failed());
    assert_eq!(dataset.diagnostics.failures.len(), 1);
    assert!(dataset.diagnostics.failures[0].error.contains("skipped 1"));
}

#[test]
fn collection_diagnostics_accept_blank_in_progress_session() {
    let home = TempHome::new();
    home.put_claude_session("proj", "empty.jsonl", "\n");

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert!(dataset.is_empty());
    assert_eq!(dataset.diagnostics.candidates, 1);
    assert_eq!(dataset.diagnostics.parsed, 1);
    assert!(!dataset.diagnostics.has_failures());
}

#[test]
fn metadata_only_file_sessions_remain_in_canonical_batch_json() {
    let home = TempHome::new();
    let claude = home.put_claude_session(
        "proj",
        "metadata.jsonl",
        r#"{"type":"permission-mode","parentUuid":"root","timestamp":"2026-07-12T00:00:00Z"}"#,
    );
    let codex = home.put_codex_session(
        "2026/07/12/metadata.jsonl",
        r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"session","cwd":"/repo"}}"#,
    );
    let copilot = home.put(
        ".copilot/session-state/session/events.jsonl",
        r#"{"type":"session.start","data":{"sessionId":"session","producer":"copilot-cli"},"timestamp":"2026-07-12T00:00:00Z"}"#,
    );
    let gemini = home.put_gemini_session(
        "proj",
        "metadata.jsonl",
        r#"{"sessionId":"session","projectHash":"proj","startTime":"2026-07-12T00:00:00Z"}"#,
    );

    for (provider, path) in [
        (ExtensionType::ClaudeCode, claude),
        (ExtensionType::Codex, codex),
        (ExtensionType::Copilot, copilot),
        (ExtensionType::Gemini, gemini),
    ] {
        let single = parse_session_file_typed(&path).unwrap();
        let dataset = collect_analysis_sessions_from_paths_with(
            &home.paths,
            TimeRange::All,
            providers_only(provider),
            ParseMode::Full,
        )
        .unwrap();
        assert_eq!(dataset.len(), 1, "metadata-only {provider} was omitted");
        assert_eq!(
            serde_json::to_value(&dataset.sessions[0].analysis).unwrap(),
            serde_json::to_value(single).unwrap(),
            "batch and single-file contracts diverged for {provider}"
        );
        assert_eq!(dataset.diagnostics.parsed, 1);
        assert!(!dataset.diagnostics.has_failures());
    }
}

#[test]
fn analyzer_payload_schema_drift_fails_for_every_file_provider() {
    let temp = TempDir::new().unwrap();
    let cases = [
        (
            "claude.jsonl",
            concat!(
                r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-07-12T00:00:00Z","message":{"model":"claude-sonnet","usage":"future","content":[]}}"#,
                "\n"
            ),
        ),
        (
            "codex.jsonl",
            concat!(
                r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"session"}}"#,
                "\n",
                r#"{"timestamp":"2026-07-12T00:00:01Z","type":"response_item","payload":{"type":42}}"#,
                "\n"
            ),
        ),
        (
            "copilot.jsonl",
            concat!(
                r#"{"type":"session.start","data":{"sessionId":"session","producer":"copilot-cli"},"timestamp":"2026-07-12T00:00:00Z"}"#,
                "\n",
                r#"{"type":"session.shutdown","data":{"modelMetrics":"future"},"timestamp":"2026-07-12T00:00:01Z"}"#,
                "\n"
            ),
        ),
        (
            "gemini.jsonl",
            concat!(
                r#"{"sessionId":"session","projectHash":"proj","startTime":"2026-07-12T00:00:00Z"}"#,
                "\n",
                r#"{"id":"message","timestamp":"2026-07-12T00:00:01Z","type":"gemini","model":"gemini","tokens":"future"}"#,
                "\n"
            ),
        ),
    ];

    for (name, contents) in cases {
        let path = temp.path().join(name);
        std::fs::write(&path, contents).unwrap();
        let error = parse_session_file_typed(&path).unwrap_err();
        assert!(
            error.to_string().contains("none used a supported schema"),
            "unexpected error for {name}: {error}"
        );
    }
}

#[test]
fn copilot_tracked_tool_argument_drift_keeps_only_the_invocation() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("copilot-tool.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"session.start","data":{"sessionId":"session","producer":"copilot-cli"},"timestamp":"2026-07-12T00:00:00Z"}"#,
            "\n",
            r#"{"type":"tool.execution_start","data":{"toolCallId":"call","toolName":"show_file","arguments":{"futurePath":"/repo/a"}},"timestamp":"2026-07-12T00:00:01Z"}"#,
            "\n",
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"call","success":true,"result":{"content":"text"}},"timestamp":"2026-07-12T00:00:02Z"}"#,
            "\n"
        ),
    )
    .unwrap();

    let analysis = parse_session_file_typed(path).unwrap();
    let record = &analysis.records[0];
    assert_eq!(record.tool_call_counts.read, 1);
    assert_eq!(record.total_read_lines, 0);
    assert_eq!(record.total_unique_files, 0);
    assert!(record.read_file_details.is_empty());
}

#[test]
fn single_file_and_batch_summaries_keep_tool_invocation_model() {
    let home = TempHome::new();
    let path = home.put(
        ".copilot/session-state/model-switch/events.jsonl",
        concat!(
            r#"{"type":"session.start","data":{"sessionId":"session","producer":"copilot-cli"},"timestamp":"2026-07-12T00:00:00Z"}"#,
            "\n",
            r#"{"type":"session.model_change","data":{"newModel":"model-a"},"timestamp":"2026-07-12T00:00:01Z"}"#,
            "\n",
            r#"{"type":"tool.execution_start","data":{"toolCallId":"read-a","toolName":"show_file","arguments":{"path":"/tmp/a.txt"}},"timestamp":"2026-07-12T00:00:02Z"}"#,
            "\n",
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"read-a","success":true,"result":{"content":"one\ntwo"}},"timestamp":"2026-07-12T00:00:03Z"}"#,
            "\n",
            r#"{"type":"session.model_change","data":{"newModel":"model-b"},"timestamp":"2026-07-12T00:00:04Z"}"#,
            "\n",
            r#"{"type":"tool.execution_start","data":{"toolCallId":"bash-b","toolName":"write_bash","arguments":{"command":"pwd"}},"timestamp":"2026-07-12T00:00:05Z"}"#,
            "\n",
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"bash-b","success":true,"result":{"content":"/tmp"}},"timestamp":"2026-07-12T00:00:06Z"}"#,
            "\n"
        ),
    );

    let (_, single, _) = project_session_file(&path, ParseMode::UsageOnly).unwrap();
    let model_a = single
        .rows
        .iter()
        .find(|row| row.model == "model-a")
        .unwrap();
    let model_b = single
        .rows
        .iter()
        .find(|row| row.model == "model-b")
        .unwrap();
    assert_eq!(model_a.read_count, 1);
    assert_eq!(model_a.read_lines, 2);
    assert_eq!(model_a.bash_count, 0);
    assert_eq!(model_b.read_count, 0);
    assert_eq!(model_b.bash_count, 1);

    let summary = aggregate_sessions_by_model_from_paths_with_diagnostics(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Copilot),
    )
    .unwrap()
    .data;
    let model_a = summary
        .rows
        .iter()
        .find(|row| row.model == "model-a")
        .unwrap();
    let model_b = summary
        .rows
        .iter()
        .find(|row| row.model == "model-b")
        .unwrap();
    assert_eq!(model_a.read_count, 1);
    assert_eq!(model_a.bash_count, 0);
    assert_eq!(model_b.read_count, 0);
    assert_eq!(model_b.bash_count, 1);

    let mut dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Copilot),
        ParseMode::Full,
    )
    .unwrap();
    let public_summary = dataset.summarize();
    assert_eq!(public_summary.rows.len(), 1);
    assert_eq!(public_summary.rows[0].model, "unknown");
    assert_eq!(public_summary.rows[0].read_count, 1);
    assert_eq!(public_summary.rows[0].bash_count, 1);
    dataset.sessions.clear();
    assert!(dataset.summarize().rows.is_empty());
}

#[test]
fn usage_presence_replay_uses_the_canonical_event_time() {
    let home = TempHome::new();
    let old = (Local::now() - Duration::days(10)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let recent = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let message = |timestamp: &str, input_tokens: i64| {
        json!({
            "parentUuid": null,
            "sessionId": "session",
            "type": "assistant",
            "timestamp": timestamp,
            "message": {
                "id": "shared-message",
                "model": "claude-replayed-model",
                "usage": { "input_tokens": input_tokens, "output_tokens": 1 },
                "content": []
            }
        })
    };
    home.put_claude_session("project", "old.jsonl", &message(&old, 1).to_string());
    home.put_claude_session("project", "recent.jsonl", &message(&recent, 2).to_string());

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::Daily,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert!(dataset.sessions.is_empty());
    assert!(dataset.summarize().rows.is_empty());
    assert_eq!(dataset.summarize().provider_days.claude, 0);

    let mut cache = SummaryScanCache::default();
    for _ in 0..2 {
        let summary = aggregate_sessions_by_model_from_paths_with_cache(
            &home.paths,
            TimeRange::Daily,
            providers_only(ExtensionType::ClaudeCode),
            &mut cache,
        )
        .unwrap();
        assert!(summary.data.rows.is_empty());
        assert_eq!(summary.data.provider_days.claude, 0);
    }
}

#[test]
fn advisor_usage_does_not_create_an_analysis_model_row() {
    let home = TempHome::new();
    let timestamp = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let source = home.put_claude_session(
        "project",
        "advisor.jsonl",
        &json!({
            "parentUuid": null,
            "sessionId": "session",
            "type": "assistant",
            "timestamp": timestamp,
            "message": {
                "id": "message",
                "model": "claude-main",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1,
                    "iterations": [
                        { "type": "message", "input_tokens": 1, "output_tokens": 1 },
                        { "type": "advisor_message", "model": "claude-advisor",
                          "input_tokens": 10, "output_tokens": 2 }
                    ]
                },
                "content": []
            }
        })
        .to_string(),
    );

    let (_, direct, _) = project_session_file(&source, ParseMode::UsageOnly).unwrap();
    assert_eq!(direct.rows.len(), 1);
    assert_eq!(direct.rows[0].model, "claude-main");

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.summarize().rows.len(), 1);
    assert_eq!(dataset.summarize().rows[0].model, "claude-main");
}

#[test]
fn single_file_without_timestamp_keeps_one_synthetic_active_day() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "project",
        "missing-timestamp.jsonl",
        &json!({
            "parentUuid": null,
            "sessionId": "session",
            "type": "assistant",
            "message": {
                "id": "message",
                "model": "claude-no-timestamp",
                "usage": { "input_tokens": 1, "output_tokens": 1 },
                "content": []
            }
        })
        .to_string(),
    );

    let (_, summary, _) = project_session_file(&source, ParseMode::UsageOnly).unwrap();
    assert_eq!(summary.rows.len(), 1);
    assert_eq!(summary.rows[0].model, "claude-no-timestamp");
    assert_eq!(summary.provider_days.claude, 1);
    assert_eq!(summary.provider_days.total, 1);
}

#[test]
fn zero_tool_model_and_active_day_survive_every_analysis_projection() {
    let home = TempHome::new();
    let timestamp = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let source = home.put_claude_session(
        "project",
        "usage-only.jsonl",
        &json!({
            "parentUuid": null,
            "sessionId": "session",
            "type": "assistant",
            "timestamp": &timestamp,
            "message": {
                "id": "message",
                "model": "claude-zero-tool",
                "usage": { "input_tokens": 1, "output_tokens": 1 },
                "content": []
            }
        })
        .to_string(),
    );

    let (_, direct, _) = project_session_file(&source, ParseMode::UsageOnly).unwrap();
    assert_eq!(direct.rows.len(), 1);
    assert_eq!(direct.rows[0].model, "claude-zero-tool");
    assert_eq!(direct.provider_days.claude, 1);

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        ParseMode::Full,
    )
    .unwrap();
    let summarized = dataset.summarize();
    assert_eq!(summarized.rows.len(), 1);
    assert_eq!(summarized.rows[0].model, "claude-zero-tool");
    assert_eq!(summarized.provider_days.claude, 1);

    let mut cache = SummaryScanCache::default();
    let cold = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        &mut cache,
    )
    .unwrap();
    assert_eq!(cold.data.rows.len(), 1);
    assert_eq!(cold.data.rows[0].model, "claude-zero-tool");
    assert_eq!(cold.data.provider_days.claude, 1);
    let warm = aggregate_sessions_by_model_from_paths_with_cache(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::ClaudeCode),
        &mut cache,
    )
    .unwrap();
    assert_eq!(warm.data.rows.len(), 1);
    assert_eq!(warm.data.rows[0].model, "claude-zero-tool");
    assert_eq!(warm.data.provider_days.claude, 1);
    assert_eq!(cache.stats().parsed_sources, 0);
}

#[test]
fn claude_tracked_tool_argument_drift_is_not_a_false_success() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("claude-tool.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"parentUuid":null,"sessionId":"session","type":"assistant","timestamp":"2026-07-12T00:00:00Z","message":{"content":[{"type":"tool_use","id":"read","name":"Read","input":{"futurePath":"/repo/a"}},{"type":"tool_use","id":"bash","name":"Bash","input":{"futureCommand":"pwd"}}]}}"#,
            "\n",
            r#"{"parentUuid":"root","sessionId":"session","type":"user","isSidechain":true,"timestamp":"2026-07-12T00:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"read","content":"one\ntwo"},{"type":"tool_result","tool_use_id":"bash","content":"ok"}]}}"#,
            "\n"
        ),
    )
    .unwrap();

    let error = parse_session_file_typed(path).unwrap_err();
    assert!(error.to_string().contains("none used a supported schema"));
}

#[test]
fn codex_custom_apply_patch_header_drift_is_not_a_false_success() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("codex-patch.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"session"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-12T00:00:01Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Future File: /repo/a\n+one\n*** End Patch","call_id":"call"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-12T00:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call","output":"Done!"}}"#,
            "\n"
        ),
    )
    .unwrap();

    let error = parse_session_file_typed(path).unwrap_err();
    assert!(error.to_string().contains("none used a supported schema"));
}

#[test]
fn codex_unknown_apply_patch_output_surfaces_source_diagnostics() {
    let home = TempHome::new();
    let path = home.put_codex_session(
        "2026/07/12/unknown-patch-output.jsonl",
        concat!(
            r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"session"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-12T00:00:01Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: /repo/a\n@@\n-old\n+new\n*** End Patch","call_id":"call"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-12T00:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call","output":{"success":true,"updated_files":["/repo/a"]}}}"#,
            "\n"
        ),
    );

    let (analysis, diagnostics) =
        parse_session_file_typed_with_mode_and_diagnostics(&path, ParseMode::Full).unwrap();
    assert_eq!(analysis.records[0].tool_call_counts.edit, 1);
    assert!(analysis.records[0].edit_file_details.is_empty());
    assert_eq!(diagnostics.skipped_records(), 1);

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Codex),
        ParseMode::Full,
    )
    .unwrap();
    assert_eq!(dataset.len(), 1);
    assert_eq!(dataset.diagnostics.failures.len(), 1);
    assert_eq!(dataset.diagnostics.failures[0].source, path);
    assert!(dataset.diagnostics.failures[0].error.contains("skipped 1"));
}

#[test]
fn collection_diagnostics_reject_unknown_opencode_assistant_schema() {
    let home = TempHome::new();
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    let conn = Connection::open(&home.paths.opencode_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER); \
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT); \
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT); \
         INSERT INTO session VALUES ('s1', '/repo', 1780757089000); \
         INSERT INTO message VALUES ('m1', 's1', \
             '{\"role\":\"assistant\",\"futureUsage\":{\"input\":10}}');",
    )
    .unwrap();
    drop(conn);

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::OpenCode),
        ParseMode::Full,
    )
    .unwrap();
    assert!(dataset.is_empty());
    assert_eq!(dataset.diagnostics.candidates, 1);
    assert_eq!(dataset.diagnostics.parsed, 0);
    assert!(dataset.diagnostics.all_failed());
}

#[test]
fn streaming_aggregation_retains_partial_failure_diagnostics() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "valid.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    std::fs::create_dir_all(home.paths.opencode_db.parent().unwrap()).unwrap();
    std::fs::write(&home.paths.opencode_db, "not a SQLite database").unwrap();

    let providers = ProvidersConfig {
        opencode: true,
        ..providers_only(ExtensionType::ClaudeCode)
    };

    let result = aggregate_sessions_by_model_from_paths_with_diagnostics(
        &home.paths,
        TimeRange::All,
        providers,
    )
    .unwrap();

    assert!(
        result
            .data
            .rows
            .iter()
            .any(|row| row.model == "claude-sonnet-4-20250514")
    );
    assert_eq!(result.diagnostics.candidates, 2);
    assert_eq!(result.diagnostics.parsed, 1);
    assert!(!result.diagnostics.all_failed());
    assert!(result.diagnostics.partially_failed());
    assert_eq!(result.diagnostics.failures.len(), 1);
    assert_eq!(
        result.diagnostics.failures[0].provider,
        ExtensionType::OpenCode
    );
    assert_eq!(
        result.diagnostics.failures[0].source,
        home.paths.opencode_db
    );
}

#[test]
fn cursor_all_store_failure_is_not_reported_as_parsed() {
    let home = TempHome::new();
    home.put(
        ".cursor/chats/project/conversation/store.db",
        "not a SQLite database",
    );

    let dataset = collect_analysis_sessions_from_paths_with(
        &home.paths,
        TimeRange::All,
        providers_only(ExtensionType::Cursor),
        ParseMode::Full,
    )
    .unwrap();

    assert!(dataset.is_empty());
    assert_eq!(dataset.diagnostics.candidates, 1);
    assert_eq!(dataset.diagnostics.parsed, 0);
    assert!(dataset.diagnostics.all_failed());
    assert_eq!(dataset.diagnostics.failures.len(), 1);
    assert_eq!(
        dataset.diagnostics.failures[0].provider,
        ExtensionType::Cursor
    );
}

#[test]
fn test_batch_analysis_serialization() {
    use vibe_coding_tracker::analysis::aggregator::AggregatedAnalysisRow;

    let row = AggregatedAnalysisRow {
        model: "claude-sonnet-4".to_string(),
        edit_lines: 100,
        read_lines: 200,
        write_lines: 50,
        bash_count: 10,
        edit_count: 20,
        read_count: 30,
        todo_write_count: 5,
        write_count: 8,
    };

    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("editLines"));
    assert!(json.contains("readLines"));
    assert!(json.contains("writeLines"));
    assert!(json.contains("bashCount"));
    assert!(json.contains("todoWriteCount"));

    let deserialized: AggregatedAnalysisRow = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.model, row.model);
    assert_eq!(deserialized.edit_lines, row.edit_lines);
}

#[test]
fn test_analysis_with_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let empty_file = temp_dir.path().join("empty.jsonl");
    std::fs::write(&empty_file, "").unwrap();

    let result = parse_session_file(&empty_file).expect("empty input has a defined JSON shape");
    assert_eq!(result, json!({}));
}

#[test]
fn test_analysis_with_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let invalid_file = temp_dir.path().join("invalid.jsonl");
    std::fs::write(&invalid_file, "not valid json\n{incomplete").unwrap();

    let result = parse_session_file(&invalid_file);
    assert!(result.is_err(), "Should fail on invalid JSON");
}

#[test]
fn test_analysis_aggregation_logic() {
    use vibe_coding_tracker::analysis::aggregator::AggregatedAnalysisRow;

    let rows = [
        AggregatedAnalysisRow {
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 2,
            write_count: 3,
        },
        AggregatedAnalysisRow {
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 3,
            write_count: 5,
        },
    ];

    let total_edit_lines: usize = rows.iter().map(|r| r.edit_lines).sum();
    let total_read_lines: usize = rows.iter().map(|r| r.read_lines).sum();
    let total_write_lines: usize = rows.iter().map(|r| r.write_lines).sum();

    assert_eq!(total_edit_lines, 100);
    assert_eq!(total_read_lines, 200);
    assert_eq!(total_write_lines, 50);
}

/// Regression for the silent usage drop that happened when a Claude session
/// started with a metadata sentinel (`permission-mode`, `file-history-snapshot`,
/// `queue-operation`). Those records don't carry `parentUuid`, so the old
/// streaming detector — which only looked at the first line — classified the
/// whole file as Codex and the assistant `usage` entries never landed in the
/// Claude totals. This test writes a fixture with such a prelude and asserts both
/// the provider-known entry point and the auto-detect entry point return the
/// Claude model usage.
fn write_claude_fixture_with_sentinel_prelude(path: &std::path::Path, sentinel_type: &str) {
    let sentinel = match sentinel_type {
        "permission-mode" => {
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"sess-1"}"#
        }
        "file-history-snapshot" => {
            r#"{"type":"file-history-snapshot","messageId":"m1","isSnapshotUpdate":false,"snapshot":{}}"#
        }
        "queue-operation" => {
            r#"{"type":"queue-operation","operation":"enqueue","sessionId":"sess-1","content":"x","timestamp":"2026-04-23T00:00:00.000Z"}"#
        }
        _ => unreachable!(),
    };

    // Minimal assistant message with the fields the analyzer reads:
    // model + usage. No <synthetic> — those are intentionally skipped.
    let assistant = r#"{"type":"assistant","sessionId":"sess-1","parentUuid":"pu","timestamp":"2026-04-23T00:00:00.000Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"service_tier":"standard","cache_creation":{"ephemeral_5m_input_tokens":10}},"content":[]}}"#;

    let body = format!("{sentinel}\n{assistant}\n");
    std::fs::write(path, body).unwrap();
}

fn usage_input_tokens_for_model(analysis: &serde_json::Value, model: &str) -> i64 {
    analysis["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|r| r.get("conversationUsage"))
        .and_then(|cu| cu.get(model))
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1)
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_permission_mode() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "permission-mode");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    assert_eq!(analysis.extension_name, "Claude-Code");
    assert_eq!(analysis.records.len(), 1);

    let record = &analysis.records[0];
    let usage = record
        .conversation_usage
        .get("claude-opus-4-7")
        .expect("claude-opus-4-7 usage should be recorded despite the permission-mode prelude");
    assert_eq!(usage["input_tokens"], 100);
    assert_eq!(usage["output_tokens"], 50);
    assert_eq!(usage["cache_creation_input_tokens"], 10);
    assert_eq!(usage["cache_read_input_tokens"], 20);
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_file_history_snapshot() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "file-history-snapshot");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    let record = &analysis.records[0];
    assert!(
        record.conversation_usage.contains_key("claude-opus-4-7"),
        "claude-opus-4-7 usage should be recorded even when first line is file-history-snapshot"
    );
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_queue_operation() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "queue-operation");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    let record = &analysis.records[0];
    assert!(
        record.conversation_usage.contains_key("claude-opus-4-7"),
        "claude-opus-4-7 usage should be recorded even when first line is queue-operation"
    );
}

#[test]
fn test_autodetect_sees_past_queue_operation_prelude() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "queue-operation");

    let analysis = parse_session_file(&file).expect("auto-detect should handle the prelude");
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert_eq!(
        usage_input_tokens_for_model(&analysis, "claude-opus-4-7"),
        100,
    );
}

#[test]
fn test_autodetect_sees_past_sentinel_prelude() {
    // The auto-detect path (used by the CLI `vct analysis <file>` form) should
    // peek enough records to spot the Claude-shaped assistant row sitting
    // behind the metadata preamble.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "permission-mode");

    let analysis = parse_session_file(&file).expect("auto-detect should handle the prelude");

    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert_eq!(
        usage_input_tokens_for_model(&analysis, "claude-opus-4-7"),
        100,
        "auto-detect should extract the assistant record's usage, not drop the whole file"
    );
}

#[test]
fn test_autodetect_handles_ten_thousand_record_preamble() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    let sentinel = r#"{"type":"file-history-snapshot","messageId":"m1","isSnapshotUpdate":false,"snapshot":{}}"#;
    let assistant = r#"{"type":"assistant","sessionId":"sess-1","parentUuid":"pu","timestamp":"2026-04-23T00:00:00.000Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50},"content":[]}}"#;
    let mut body = String::with_capacity((sentinel.len() + 1) * 10_000 + assistant.len() + 1);
    for _ in 0..10_000 {
        body.push_str(sentinel);
        body.push('\n');
    }
    body.push_str(assistant);
    body.push('\n');
    std::fs::write(&file, body).unwrap();

    let analysis = parse_session_file(&file).expect("auto-detect should remain linear");

    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert_eq!(
        usage_input_tokens_for_model(&analysis, "claude-opus-4-7"),
        100
    );
}
