// Integration tests for the persistent session ledger.
//
// These drive the ledger-backed scans against a `TempHome`, opening the ledger
// under that home's `~/.vct` so nothing outside the temp directory is read or
// written. What they pin is the ledger's contract rather than any parser's:
// an unchanged source is never parsed twice, a source that is gone keeps
// contributing, a changed one is re-read, and the time range never decides
// what the ledger remembers.

use rusqlite::Connection;
use std::path::Path;
use std::time::{Duration, SystemTime};
use vct_core::TimeRange;
use vct_core::analysis::aggregator::{
    AnalysisCollection, aggregate_sessions_by_model_from_paths_with_cache,
};
use vct_core::config::ProvidersConfig;
use vct_core::ledger::{LEDGER_DIR_NAME, SessionLedger, provider_path};
use vct_core::models::ExtensionType;
use vct_core::usage::aggregator::{UsageCollection, aggregate_usage_from_paths_with_cache};
use vct_test_support::{TempHome, fixture_str};

fn only(provider: ExtensionType) -> ProvidersConfig {
    ProvidersConfig {
        claude: provider == ExtensionType::ClaudeCode,
        codex: provider == ExtensionType::Codex,
        copilot: provider == ExtensionType::Copilot,
        gemini: provider == ExtensionType::Gemini,
        opencode: provider == ExtensionType::OpenCode,
        cursor: provider == ExtensionType::Cursor,
        hermes: provider == ExtensionType::Hermes,
        grok: provider == ExtensionType::Grok,
        dsh: provider == ExtensionType::DeepSeek,
    }
}

fn open(home: &TempHome) -> SessionLedger {
    SessionLedger::open(&home.paths.cache_dir)
}

fn usage(
    home: &TempHome,
    range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
) -> UsageCollection {
    aggregate_usage_from_paths_with_cache(&home.paths, range, providers, ledger)
        .expect("usage scan")
}

fn analysis(
    home: &TempHome,
    range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
) -> AnalysisCollection {
    aggregate_sessions_by_model_from_paths_with_cache(&home.paths, range, providers, ledger)
        .expect("analysis scan")
}

fn ledger_file(home: &TempHome, provider: ExtensionType) -> serde_json::Value {
    let path = provider_path(&home.paths.cache_dir.join(LEDGER_DIR_NAME), provider);
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read ledger file"))
        .expect("ledger file is JSON")
}

fn set_modified(path: &Path, modified: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for touching")
        .set_modified(modified)
        .expect("set mtime");
}

fn years_ago(years: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(years * 366 * 24 * 60 * 60)
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn seed_opencode_db(path: &Path, sessions: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    for (session, model) in sessions {
        connection
            .execute(
                "INSERT INTO session (id, directory, time_updated) VALUES (?1, '/repo', 1780757089000)",
                [session],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                [
                    format!("{session}-message"),
                    session.to_string(),
                    format!(
                        r#"{{"role":"assistant","providerID":"openai","modelID":"{model}","cost":0.25,"tokens":{{"input":31,"output":17,"reasoning":3,"cache":{{"read":11,"write":5}}}},"time":{{"created":1780757088000,"completed":1780757089000}}}}"#
                    ),
                ],
            )
            .unwrap();
    }
}

fn seed_hermes_db(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE session_model_usage (
                 session_id TEXT NOT NULL, model TEXT NOT NULL, billing_provider TEXT NOT NULL DEFAULT '',
                 input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                 reasoning_tokens INTEGER NOT NULL DEFAULT 0, estimated_cost_usd REAL NOT NULL DEFAULT 0,
                 actual_cost_usd REAL NOT NULL DEFAULT 0, first_seen REAL, last_seen REAL);
             CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, model TEXT, billing_provider TEXT,
                 input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
                 cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
                 reasoning_tokens INTEGER DEFAULT 0, estimated_cost_usd REAL DEFAULT 0,
                 actual_cost_usd REAL DEFAULT 0, started_at REAL NOT NULL, ended_at REAL);
             INSERT INTO session_model_usage VALUES
                 ('hermes-session', 'hermes-model', 'openai', 41, 23, 13, 7, 3, 0.5, 0.4, 1780757088, 1780757089);
             INSERT INTO sessions VALUES
                 ('hermes-session', 'hermes-model', 'openai', 41, 23, 13, 7, 3, 0.5, 0.4, 1780757088, 1780757089);",
        )
        .unwrap();
}

#[test]
fn a_reopened_ledger_serves_the_scan_without_parsing() {
    let home = TempHome::new();
    home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = only(ExtensionType::ClaudeCode);

    let mut ledger = open(&home);
    let cold = usage(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 1);
    ledger.save().unwrap();

    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    assert_eq!(file["schema_version"], 1);
    assert_eq!(file["provider"], "claude");
    let session = &file["sessions"]["project/session.jsonl"];
    assert!(
        session.is_object(),
        "keyed by the path under the projects root"
    );
    assert!(session["scanned"]["usage"]["files"]["session.jsonl"]["len"].is_u64());
    assert!(session["scanned"]["analysis"]["tiers"].is_null());
    assert!(session["cwd"].is_string());
    let days = session["days"].as_object().unwrap();
    assert_eq!(days.len(), 1, "a file session has exactly one day");
    let day = days.values().next().unwrap();
    assert!(day["usage"]["claude-sonnet-4-20250514"]["input_tokens"].is_number());
    assert_eq!(day["active"]["usage"], true);
    assert_eq!(day["active"]["analysis"], true);

    let mut reopened = open(&home);
    let warm = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(reopened.stats().parsed_sources, 0);
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_eq!(warm.data.models, cold.data.models);
    assert_eq!(warm.data.provider_days.claude, 1);
}

#[test]
fn a_deleted_session_keeps_contributing_and_is_marked_missing() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);
    let before = usage(&home, TimeRange::All, providers, &mut ledger);
    ledger.save().unwrap();

    std::fs::remove_file(&source).unwrap();
    let mut reopened = open(&home);
    let after = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(after.diagnostics.candidates, 0);
    assert_eq!(after.diagnostics.retained, 1);
    assert!(!after.diagnostics.all_failed());
    assert_eq!(after.data.models, before.data.models);
    assert_eq!(after.data.provider_days.claude, 1);

    reopened.save().unwrap();
    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    assert_eq!(
        file["sessions"]["project/session.jsonl"]["missing_since"],
        today()
    );

    // Restoring the file clears the mark without a re-parse.
    home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let mut restored = open(&home);
    let back = usage(&home, TimeRange::All, providers, &mut restored);
    assert_eq!(back.diagnostics.retained, 0);
    assert_eq!(back.data.models, before.data.models);
    restored.save().unwrap();
    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    assert!(file["sessions"]["project/session.jsonl"]["missing_since"].is_null());
}

#[test]
fn a_deleted_provider_root_keeps_every_session() {
    let home = TempHome::new();
    home.put_claude_session(
        "alpha",
        "one.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_claude_session(
        "beta",
        "two.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);
    let before = usage(&home, TimeRange::All, providers, &mut ledger);
    let before_analysis = analysis(&home, TimeRange::All, providers, &mut ledger);
    ledger.save().unwrap();

    std::fs::remove_dir_all(&home.paths.claude_session_dir).unwrap();
    let mut reopened = open(&home);
    let after = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(after.diagnostics.candidates, 0);
    assert_eq!(after.diagnostics.retained, 2);
    assert_eq!(after.data.models, before.data.models);
    let after_analysis = analysis(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(after_analysis.diagnostics.retained, 2);
    assert_eq!(
        serde_json::to_value(&after_analysis.data.rows).unwrap(),
        serde_json::to_value(&before_analysis.data.rows).unwrap()
    );
}

#[test]
fn a_daily_scan_neither_reparses_nor_forgets_older_sessions() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "project",
        "old.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    set_modified(&source, years_ago(3));
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);

    let all = usage(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 1);
    assert!(!all.data.models.is_empty());

    let daily = usage(&home, TimeRange::Daily, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 0);
    assert_eq!(daily.diagnostics.retained, 0);
    assert!(
        daily.data.models.is_empty(),
        "three-year-old usage is outside today"
    );
    assert_eq!(daily.data.provider_days.claude, 0);

    ledger.save().unwrap();
    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    assert!(file["sessions"]["project/old.jsonl"]["missing_since"].is_null());

    let again = usage(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 0);
    assert_eq!(again.data.models, all.data.models);
}

#[test]
fn a_resumed_session_is_reread_and_moves_to_its_new_day() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    set_modified(&source, years_ago(3));
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);
    usage(&home, TimeRange::All, providers, &mut ledger);
    ledger.save().unwrap();
    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    let old_day = file["sessions"]["project/session.jsonl"]["days"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_ne!(old_day, today());

    let mut resumed = fixture_str("sessions/claude_code.jsonl");
    resumed.push('\n');
    std::fs::write(&source, resumed).unwrap();
    let after = usage(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 1);
    assert_eq!(after.data.provider_days.claude, 1);
    ledger.save().unwrap();
    let file = ledger_file(&home, ExtensionType::ClaudeCode);
    let days: Vec<_> = file["sessions"]["project/session.jsonl"]["days"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(days, vec![today()], "the whole file moves to its new day");
}

#[test]
fn an_opencode_session_deleted_from_the_database_is_retained() {
    let home = TempHome::new();
    seed_opencode_db(
        &home.paths.opencode_db,
        &[("ses_keep", "kept-model"), ("ses_gone", "gone-model")],
    );
    let providers = only(ExtensionType::OpenCode);
    let mut ledger = open(&home);
    let before = usage(&home, TimeRange::All, providers, &mut ledger);
    assert!(before.data.models.contains_key("openai/gone-model"));
    ledger.save().unwrap();
    let file = ledger_file(&home, ExtensionType::OpenCode);
    assert!(file["source"]["usage"]["files"]["opencode.db"].is_object());
    assert_eq!(file["sessions"]["ses_gone"]["cwd"], "/repo");

    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection
        .execute_batch(
            "DELETE FROM message WHERE session_id = 'ses_gone'; DELETE FROM session WHERE id = 'ses_gone';",
        )
        .unwrap();
    drop(connection);

    let mut reopened = open(&home);
    let after = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(reopened.stats().parsed_sources, 1);
    assert_eq!(after.diagnostics.retained, 1);
    assert_eq!(after.data.models, before.data.models);
    assert_eq!(
        after.data.stored_costs.opencode,
        before.data.stored_costs.opencode
    );
    reopened.save().unwrap();
    let file = ledger_file(&home, ExtensionType::OpenCode);
    assert_eq!(file["sessions"]["ses_gone"]["missing_since"], today());
    assert!(file["sessions"]["ses_keep"]["missing_since"].is_null());
}

#[test]
fn a_database_that_stops_being_readable_keeps_serving_its_sessions() {
    let home = TempHome::new();
    seed_opencode_db(
        &home.paths.opencode_db,
        &[("ses_a", "model-a"), ("ses_b", "model-b")],
    );
    let providers = only(ExtensionType::OpenCode);
    let mut ledger = open(&home);
    let before = usage(&home, TimeRange::All, providers, &mut ledger);
    ledger.save().unwrap();

    // Every row turns into a schema this build does not understand: the read
    // succeeds but understands nothing, so the sessions are served from the
    // ledger, counted as retained, and not marked missing.
    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection
        .execute_batch(r#"UPDATE message SET data = '{"role":"assistant","futureUsage":{}}';"#)
        .unwrap();
    drop(connection);
    let mut reopened = open(&home);
    let drifted = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(drifted.diagnostics.candidates, 1);
    assert_eq!(drifted.diagnostics.parsed, 0);
    assert_eq!(drifted.diagnostics.retained, 2);
    assert!(!drifted.diagnostics.all_failed());
    assert_eq!(drifted.diagnostics.failures.len(), 1);
    assert_eq!(drifted.data.models, before.data.models);
    reopened.save().unwrap();
    let file = ledger_file(&home, ExtensionType::OpenCode);
    assert!(file["sessions"]["ses_a"]["missing_since"].is_null());
    assert_eq!(file["source"]["usage"]["parsed"], false);

    // The same holds when the query itself fails against the bytes on disk.
    let connection = Connection::open(&home.paths.opencode_db).unwrap();
    connection.execute_batch("DROP TABLE session;").unwrap();
    drop(connection);
    let mut reopened = open(&home);
    let broken = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(broken.diagnostics.parsed, 0);
    assert_eq!(broken.diagnostics.retained, 2);
    assert!(!broken.diagnostics.all_failed());
    assert_eq!(broken.data.models, before.data.models);
    let again = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(
        reopened.stats().parsed_sources,
        0,
        "the failure verdict is retained"
    );
    assert_eq!(again.diagnostics, broken.diagnostics);
}

#[test]
fn cursor_writes_no_stored_cost() {
    let home = TempHome::new();
    home.put_cursor_session(
        "hash",
        "conversation",
        "cursor-model",
        1_780_757_089_000,
        100,
    );
    let mut ledger = open(&home);
    usage(
        &home,
        TimeRange::All,
        only(ExtensionType::Cursor),
        &mut ledger,
    );
    ledger.save().unwrap();
    let file = ledger_file(&home, ExtensionType::Cursor);
    let day = file["sessions"]["hash/conversation/store.db"]["days"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(day["usage"]["cursor-model"].is_object());
    assert!(day.get("stored_cost").is_none());
}

#[test]
fn a_hermes_database_that_disappears_keeps_its_sessions() {
    let home = TempHome::new();
    seed_hermes_db(&home.paths.hermes_db);
    let providers = only(ExtensionType::Hermes);
    let mut ledger = open(&home);
    let before = usage(&home, TimeRange::All, providers, &mut ledger);
    assert!(before.data.models.contains_key("hermes-model"));
    ledger.save().unwrap();

    std::fs::remove_file(&home.paths.hermes_db).unwrap();
    let mut reopened = open(&home);
    let after = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(after.diagnostics.candidates, 0);
    assert_eq!(after.diagnostics.retained, 1);
    assert_eq!(after.data.models, before.data.models);
    assert_eq!(
        after.data.stored_costs.hermes,
        before.data.stored_costs.hermes
    );
    assert_eq!(after.data.provider_days.hermes, 1);
}

#[test]
fn a_stale_parser_stamp_rereads_present_sessions_and_keeps_missing_ones() {
    let home = TempHome::new();
    home.put_claude_session(
        "project",
        "present.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let gone = home.put_claude_session(
        "project",
        "gone.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);
    let both = usage(&home, TimeRange::All, providers, &mut ledger);
    std::fs::remove_file(&gone).unwrap();
    usage(&home, TimeRange::All, providers, &mut ledger);
    ledger.save().unwrap();

    // A ledger written by a build whose parser differs from this one.
    let path = provider_path(
        &home.paths.cache_dir.join(LEDGER_DIR_NAME),
        ExtensionType::ClaudeCode,
    );
    let stale = std::fs::read_to_string(&path).unwrap().replace(
        &format!("\"parser\": \"{}\"", vct_core::VERSION),
        "\"parser\": \"0.0.0\"",
    );
    assert_ne!(stale, std::fs::read_to_string(&path).unwrap());
    std::fs::write(&path, stale).unwrap();

    let mut reopened = open(&home);
    let after = usage(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(
        reopened.stats().parsed_sources,
        1,
        "only the present file re-parses"
    );
    assert_eq!(after.diagnostics.retained, 1);
    assert_eq!(after.data.models, both.data.models);
}

#[test]
fn the_analysis_view_shares_the_ledger_and_retains_too() {
    let home = TempHome::new();
    let source = home.put_claude_session(
        "project",
        "session.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    let providers = only(ExtensionType::ClaudeCode);
    let mut ledger = open(&home);
    let before = analysis(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 1);
    // The usage view is served by the analysis scan's parse.
    let usage_after_analysis = usage(&home, TimeRange::All, providers, &mut ledger);
    assert_eq!(ledger.stats().parsed_sources, 0);
    assert!(!usage_after_analysis.data.models.is_empty());
    ledger.save().unwrap();

    std::fs::remove_file(&source).unwrap();
    let mut reopened = open(&home);
    let after = analysis(&home, TimeRange::All, providers, &mut reopened);
    assert_eq!(after.diagnostics.retained, 1);
    assert_eq!(
        serde_json::to_value(&after.data.rows).unwrap(),
        serde_json::to_value(&before.data.rows).unwrap()
    );
    assert_eq!(after.data.provider_days.claude, 1);
}

#[test]
fn session_keys_are_provider_relative() {
    let home = TempHome::new();
    home.put_claude_session(
        "-home-u-repo",
        "sess.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_claude_session(
        "-home-u-repo/sess/subagents",
        "agent-1.jsonl",
        &fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_codex_session(
        "2026/06/06/rollout-2026-06-06T10-00-00-uuid.jsonl",
        &fixture_str("sessions/codex.jsonl"),
    );
    home.put_cursor_session(
        "hash",
        "conversation",
        "cursor-model",
        1_780_757_089_000,
        100,
    );
    seed_opencode_db(&home.paths.opencode_db, &[("ses_1", "model")]);
    let mut ledger = open(&home);
    usage(
        &home,
        TimeRange::All,
        ProvidersConfig::default(),
        &mut ledger,
    );
    ledger.save().unwrap();

    let keys = |provider| {
        ledger_file(&home, provider)["sessions"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(ExtensionType::ClaudeCode),
        [
            "-home-u-repo/sess.jsonl",
            "-home-u-repo/sess/subagents/agent-1.jsonl"
        ]
    );
    assert_eq!(
        keys(ExtensionType::Codex),
        ["rollout-2026-06-06T10-00-00-uuid.jsonl"]
    );
    assert_eq!(keys(ExtensionType::Cursor), ["hash/conversation/store.db"]);
    assert_eq!(keys(ExtensionType::OpenCode), ["ses_1"]);
}
