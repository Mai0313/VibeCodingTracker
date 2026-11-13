//! Integration tests for the persistent settings file (`~/.vct/config.toml`).
//!
//! Every case is rooted at a `TempHome`'s `~/.vct` via the `config::*_in` seams,
//! so nothing touches the real home directory or process environment.

use std::fs;
use vct_core::TimeRange;
use vct_core::config::{self, Config};
use vct_core::resolve_time_range_with_default;
use vct_test_support::TempHome;

#[test]
fn load_in_creates_default_commented_file_when_absent() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;

    let cfg = config::load_in(dir);

    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.general.default_time_range, TimeRange::All);
    assert!(cfg.general.auto_update);
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
    assert!(text.contains("auto_update = true"));
    assert!(text.contains("grok = true"));
    // A generated comment must survive so the file is self-documenting.
    assert!(text.contains("# Toggled live"));
}

#[test]
fn first_run_leaves_existing_version_record_untouched() {
    // The self-update record (`version/vct.json`) is a separate concern from
    // the settings file; creating config.toml must not touch or fold it in.
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    let record = dir.join("version").join("vct.json");
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    let original = r#"{"schema_version":1,"provider":"vct","latest_version":"1.6.0","last_checked_at":"2026-07-09T18:11:41.390888319Z","dismissed_version":null}"#;
    fs::write(&record, original).unwrap();

    config::load_in(dir);

    assert!(record.exists(), "the record must be left in place");
    assert_eq!(fs::read_to_string(&record).unwrap(), original);
    // The self-update record's fields must not leak into the settings file.
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
    // Already in the current format (new keys + `#:schema` + version marker), so
    // load must not rewrite it.
    let original = "#:schema https://example.test/vct.schema.json\n[general]\ndefault_time_range = \"weekly\"\nauto_update = false\nversion = 4\n\n[usage]\nmerge_models = true\n\n[usage.quota]\npanels = [\"claude\"]\n\n[providers]\ncursor = false\n";
    fs::write(dir.join("config.toml"), original).unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(!cfg.general.auto_update);
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
fn generated_schema_exposes_grok_as_an_enabled_provider_with_a_quota_panel() {
    let schema: serde_json::Value =
        serde_json::from_str(&config::schema_json()).expect("generated schema is valid JSON");
    let providers = &schema["properties"]["providers"];

    assert_eq!(providers["default"]["grok"], true);
    assert_eq!(providers["properties"]["grok"]["default"], true);
    assert!(
        schema["properties"]["usage"]["properties"]["quota"]["default"]["panels"]
            .as_array()
            .expect("quota panels array")
            .iter()
            .any(|panel| panel == "grok"),
        "Grok ships as a quota panel, so it belongs in the default list"
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
    // The user's values are honored through the migration, and a panel that
    // shipped after this file was written is back-filled once.
    assert_eq!(
        cfg.usage.quota.panels,
        vec![
            "claude".to_string(),
            "grok".to_string(),
            "antigravity".to_string()
        ]
    );
    assert_eq!(cfg.usage.refresh_interval, 15);
    assert_eq!(cfg.analysis.refresh_interval, 20);
    assert!(cfg.general.auto_update);

    // The file on disk is upgraded to the current layout.
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.starts_with("#:schema "));
    assert!(text.contains("[usage.quota]"));
    assert!(!text.contains("quota_panels"));
    assert!(!text.contains("refresh_interval_secs"));
    assert!(text.contains("auto_update = true"));

    // A second load leaves the now-current file untouched.
    config::load_in(dir);
    assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), text);
}

#[test]
fn load_in_preserves_an_explicit_auto_update_preference() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_time_range = \"weekly\"\nauto_update = false\nversion = 2\n",
    )
    .unwrap();

    let cfg = config::load_in(dir);
    assert!(!cfg.general.auto_update);
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("auto_update = false"));
    assert!(text.contains("version = 4"));
    config::load_in(dir);
    assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), text);
}

#[test]
fn load_in_backfills_auto_update_for_a_legacy_general_table_once() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_time_range = \"weekly\"\nversion = 2\n",
    )
    .unwrap();

    let cfg = config::load_in(dir);
    assert!(cfg.general.auto_update);
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("auto_update = true"));
    assert!(text.contains("version = 4"));
    config::load_in(dir);
    assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), text);
}

#[test]
fn load_read_only_in_never_writes_or_creates_config() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;

    assert_eq!(config::load_read_only_in(dir), Config::default());
    assert!(!dir.exists());

    fs::create_dir_all(dir).unwrap();
    let path = dir.join("config.toml");
    let original = "[general]\ndefault_time_range = \"weekly\"\nversion = 2\n";
    fs::write(&path, original).unwrap();

    let cfg = config::load_read_only_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(cfg.general.auto_update);
    assert_eq!(fs::read_to_string(path).unwrap(), original);
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

#[test]
fn a_setting_is_read_changed_and_saved_by_its_path() {
    use vct_core::config::SettingValue;

    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    let mut cfg = config::load_in(dir);
    let path = ["usage", "quota", "refresh_interval"];
    assert_eq!(
        config::get_setting(&cfg, &path),
        Some(SettingValue::Integer(60))
    );

    config::apply_setting(&mut cfg, &path, &SettingValue::Integer(90)).unwrap();
    config::save_setting_in(dir, &path, &SettingValue::Integer(90)).unwrap();
    let panels = SettingValue::List(vec!["codex".to_string()]);
    config::save_setting_in(dir, &["usage", "quota", "panels"], &panels).unwrap();
    let range = SettingValue::String("weekly".to_string());
    config::save_setting_in(dir, &["general", "default_time_range"], &range).unwrap();

    assert_eq!(cfg.usage.quota.refresh_interval, 90);
    let reloaded = config::load_in(dir);
    assert_eq!(reloaded.usage.quota.refresh_interval, 90);
    assert_eq!(reloaded.usage.quota.panels, vec!["codex".to_string()]);
    assert_eq!(reloaded.general.default_time_range, TimeRange::Weekly);
    // The generated comments are still there.
    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("# Seconds between live quota-panel polls"));
}

#[test]
fn a_setting_refuses_a_value_its_field_cannot_hold() {
    use vct_core::config::SettingValue;

    let mut cfg = Config::default();
    let before = cfg.clone();
    let bad = SettingValue::String("yearly".to_string());
    assert!(config::apply_setting(&mut cfg, &["general", "default_time_range"], &bad).is_err());
    assert!(config::apply_setting(&mut cfg, &["general", "nope"], &bad).is_err());
    assert_eq!(cfg, before);
    assert!(config::setting_description(&["usage", "merge_models"]).is_some());
}

#[test]
fn a_setting_is_saved_into_a_file_that_lacks_its_section() {
    use vct_core::config::SettingValue;

    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    let hosts = SettingValue::List(vec!["box".to_string()]);
    let poll = SettingValue::Integer(120);

    // A file written before `[remotes]` existed, and with no `[usage.quota]`.
    fs::write(dir.join("config.toml"), "[usage]\nmerge_models = true\n").unwrap();
    config::save_setting_in(dir, &["remotes", "hosts"], &hosts).unwrap();
    config::save_setting_in(dir, &["usage", "quota", "refresh_interval"], &poll).unwrap();
    let cfg = config::load_in(dir);
    assert_eq!(cfg.remotes.hosts, vec!["box".to_string()]);
    assert_eq!(cfg.usage.quota.refresh_interval, 120);
    assert!(cfg.usage.merge_models);

    // An inline `usage` table gains the nested section and keeps its keys.
    fs::write(dir.join("config.toml"), "usage = { merge_models = true }\n").unwrap();
    config::save_setting_in(dir, &["usage", "quota", "refresh_interval"], &poll).unwrap();
    let cfg = config::load_in(dir);
    assert_eq!(cfg.usage.quota.refresh_interval, 120);
    assert!(cfg.usage.merge_models);

    // A scalar where a section belongs is replaced, at any depth.
    fs::write(dir.join("config.toml"), "usage = \"bad\"\n").unwrap();
    config::save_setting_in(dir, &["usage", "quota", "refresh_interval"], &poll).unwrap();
    assert_eq!(config::load_in(dir).usage.quota.refresh_interval, 120);
}
