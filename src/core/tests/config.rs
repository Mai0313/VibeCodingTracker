//! Integration tests for the persistent settings file (`~/.vct/config.toml`).
//!
//! Every case is rooted at a `TempHome`'s `~/.vct` via the `config::*_in` seams,
//! so nothing touches the real home directory or process environment.

use std::fs;
use vct_test_support::TempHome;
use vibe_coding_tracker::TimeRange;
use vibe_coding_tracker::config::{self, Config};
use vibe_coding_tracker::resolve_time_range_with_default;

#[test]
fn load_in_creates_default_commented_file_when_absent() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;

    let cfg = config::load_in(dir);

    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.general.default_time_range, TimeRange::All);
    assert!(cfg.usage.shows_quota_panel("cursor"));
    assert!(!cfg.usage.merge_models);
    // New sections default sensibly.
    assert!(cfg.providers.cursor);
    assert!(cfg.providers.grok);
    assert_eq!(cfg.usage.refresh_interval, 10);
    assert_eq!(cfg.usage.quota.refresh_interval, 60);
    assert_eq!(cfg.analysis.refresh_interval, 10);
    assert_eq!(cfg.performance.scan_threads, 0);
    assert_eq!(cfg.logging.retention_days, 7);

    let path = dir.join("config.toml");
    assert!(path.exists(), "first run must create config.toml");
    let text = fs::read_to_string(&path).unwrap();
    // The `#:schema` directive drives editor autocomplete/validation.
    assert!(text.starts_with("#:schema "));
    assert!(text.contains("[usage]"));
    assert!(text.contains("[usage.quota]"));
    assert!(text.contains("[providers]"));
    assert!(text.contains("[performance]"));
    assert!(text.contains("[logging]"));
    assert!(text.contains("merge_models = false"));
    assert!(text.contains("grok = true"));
    // A generated comment must survive so the file is self-documenting.
    assert!(text.contains("# Toggled live"));
}

#[test]
fn first_run_leaves_existing_version_json_untouched() {
    // The self-update record (`version.json`) is a separate concern from the
    // settings file; creating config.toml must not touch or fold it in.
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    let version_json = dir.join("version.json");
    let original = r#"{"latest_version":"1.6.0","last_checked_at":"2026-07-09T18:11:41.390888319Z","dismissed_version":null}"#;
    fs::write(&version_json, original).unwrap();

    config::load_in(dir);

    assert!(version_json.exists(), "version.json must be left in place");
    assert_eq!(fs::read_to_string(&version_json).unwrap(), original);
    // The settings file must not carry any update/version bookkeeping.
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(!text.contains("[update]"));
    assert!(!text.contains("latest_version"));
}

#[test]
fn save_merge_models_round_trips_and_preserves_comments() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    config::load_in(dir); // materialize defaults

    config::save_merge_models_in(dir, true).unwrap();

    let cfg = config::load_in(dir);
    assert!(cfg.usage.merge_models);
    assert!(cfg.usage.shows_quota_panel("cursor")); // untouched

    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("merge_models = true"));
    // The edit is format-preserving, so comments stay put.
    assert!(text.contains("# Which live quota panels to show"));
    assert!(text.contains("[providers]"));
    // The `#:schema` directive must survive the write-back, or the `m` toggle
    // would strip editor validation from the file.
    assert!(text.starts_with("#:schema "));
}

#[test]
fn save_merge_models_survives_malformed_usage_section() {
    // A hand-edited file where `usage` is a scalar must not panic the best-effort
    // write from the TUI's `m` toggle; the section is repaired to a table.
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("config.toml"), "usage = \"bad\"\n").unwrap();

    config::save_merge_models_in(dir, true).unwrap();

    let cfg = config::load_in(dir);
    assert!(cfg.usage.merge_models);
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("merge_models = true"));
}

#[test]
fn save_merge_models_preserves_an_inline_usage_table() {
    // A valid inline-table `[usage]` must NOT be wiped by the toggle: its sibling
    // keys have to survive the write-back (regression — `is_table()` misses inline
    // tables, so the guard must use `is_table_like()`).
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "usage = { merge_models = false, quota_panels = [\"claude\"], refresh_interval_secs = 30 }\n",
    )
    .unwrap();

    config::save_merge_models_in(dir, true).unwrap();

    let cfg = config::load_in(dir);
    assert!(cfg.usage.merge_models);
    // The other inline keys must not be lost / reverted to defaults; the legacy
    // `quota_panels` / `refresh_interval_secs` names are honored via the read-time
    // migration shim (an inline `usage` table is left for it, not restructured).
    assert_eq!(cfg.usage.quota.panels, vec!["claude".to_string()]);
    assert_eq!(cfg.usage.refresh_interval, 30);
}

#[test]
fn existing_current_file_is_parsed_and_left_in_place() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    // Already in the current format (new keys + `#:schema`), so load must not
    // rewrite it.
    let original = "#:schema https://example.test/vct.schema.json\n[general]\ndefault_time_range = \"weekly\"\n\n[usage]\nmerge_models = true\n\n[usage.quota]\npanels = [\"claude\"]\n\n[providers]\ncursor = false\n";
    fs::write(dir.join("config.toml"), original).unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(cfg.usage.merge_models);
    assert!(cfg.usage.shows_quota_panel("claude"));
    assert!(!cfg.usage.shows_quota_panel("cursor"));
    // A missing provider key still defaults to true; the given one is honored.
    assert!(!cfg.providers.cursor);
    assert!(cfg.providers.claude);
    assert!(cfg.providers.grok);
    // A current file is byte-for-byte untouched.
    assert_eq!(
        fs::read_to_string(dir.join("config.toml")).unwrap(),
        original
    );
}

#[test]
fn generated_schema_exposes_grok_as_an_enabled_non_quota_provider() {
    let schema: serde_json::Value =
        serde_json::from_str(&config::schema_json()).expect("generated schema is valid JSON");
    let providers = &schema["properties"]["providers"];

    assert_eq!(providers["default"]["grok"], true);
    assert_eq!(providers["properties"]["grok"]["default"], true);
    assert!(
        !schema["properties"]["usage"]["properties"]["quota"]["default"]["panels"]
            .as_array()
            .expect("quota panels array")
            .iter()
            .any(|panel| panel == "grok")
    );
}

#[test]
fn load_in_migrates_a_legacy_file_in_place() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    let legacy = "[usage]\nmerge_models = false\nquota_panels = [\"claude\"]\nrefresh_interval_secs = 15\n\n[analysis]\nrefresh_interval_secs = 20\n";
    fs::write(dir.join("config.toml"), legacy).unwrap();

    let cfg = config::load_in(dir);
    // The user's values are honored through the migration.
    assert_eq!(cfg.usage.quota.panels, vec!["claude".to_string()]);
    assert_eq!(cfg.usage.refresh_interval, 15);
    assert_eq!(cfg.analysis.refresh_interval, 20);

    // The file on disk is upgraded to the current layout.
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.starts_with("#:schema "));
    assert!(text.contains("[usage.quota]"));
    assert!(!text.contains("quota_panels"));
    assert!(!text.contains("refresh_interval_secs"));

    // A second load leaves the now-current file untouched.
    config::load_in(dir);
    assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), text);
}

#[test]
fn load_in_does_not_reset_config_on_a_malformed_legacy_refresh_value() {
    // A legacy `refresh_interval_secs` holding a non-u64 value must not be promoted
    // into the typed field: doing so would make the migrated file fail to parse and
    // silently reset EVERY setting. Instead the bad value is dropped, its default
    // applies, and every other setting survives.
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_time_range = \"weekly\"\n[usage]\nrefresh_interval_secs = -5\n[providers]\ncursor = false\n",
    )
    .unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(!cfg.providers.cursor);
    assert_eq!(cfg.usage.refresh_interval, 10); // dropped -> default

    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(!text.contains("refresh_interval = -5"));
    // The file is now current; a second load does not rewrite it.
    config::load_in(dir);
    assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), text);
}

#[test]
fn migrate_config_file_reports_status() {
    use config::MigrationStatus;
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("config.toml");

    // Absent -> a fresh default is created.
    assert_eq!(
        config::migrate_config_file(&path).unwrap(),
        MigrationStatus::Created
    );
    assert!(path.exists());
    // The generated default is already current.
    assert_eq!(
        config::migrate_config_file(&path).unwrap(),
        MigrationStatus::AlreadyCurrent
    );

    // A legacy file is migrated once, then reports current.
    fs::write(
        &path,
        "[usage]\nquota_panels = [\"claude\"]\nrefresh_interval_secs = 15\n",
    )
    .unwrap();
    assert_eq!(
        config::migrate_config_file(&path).unwrap(),
        MigrationStatus::Migrated
    );
    assert_eq!(
        config::migrate_config_file(&path).unwrap(),
        MigrationStatus::AlreadyCurrent
    );
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("#:schema "));
    assert!(text.contains("[usage.quota]"));
}

#[test]
fn time_range_default_precedence() {
    // An explicit period flag always wins.
    assert_eq!(
        resolve_time_range_with_default(true, false, false, false, TimeRange::All),
        TimeRange::Daily
    );
    // Explicit --all overrides a non-default config value.
    assert_eq!(
        resolve_time_range_with_default(false, false, false, true, TimeRange::Weekly),
        TimeRange::All
    );
    // No flag at all falls back to the config default.
    assert_eq!(
        resolve_time_range_with_default(false, false, false, false, TimeRange::Monthly),
        TimeRange::Monthly
    );
}
