//! Integration tests for the persistent settings file (`~/.vct/config.toml`).
//!
//! Every case is rooted at a `TempHome`'s `~/.vct` via the `config::*_in` seams,
//! so nothing touches the real home directory or process environment.

mod common;

use common::TempHome;
use std::fs;
use vibe_coding_tracker::TimeRange;
use vibe_coding_tracker::cli::resolve_time_range_with_default;
use vibe_coding_tracker::config::{self, Config};

#[test]
fn load_in_creates_default_commented_file_when_absent() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;

    let cfg = config::load_in(dir);

    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.general.default_time_range, TimeRange::All);
    assert!(cfg.usage.show_quota_panels);
    assert!(!cfg.usage.merge_models);
    assert!(cfg.update.check_enabled);

    let path = dir.join("config.toml");
    assert!(path.exists(), "first run must create config.toml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[usage]"));
    assert!(text.contains("merge_models = false"));
    // A header comment must survive so the file is self-documenting.
    assert!(text.contains("# Toggled live"));
}

#[test]
fn migrates_legacy_version_json_and_removes_it() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    let legacy = dir.join("version.json");
    fs::write(
        &legacy,
        r#"{"latest_version":"1.6.0","last_checked_at":"2026-07-09T18:11:41.390888319Z","dismissed_version":null}"#,
    )
    .unwrap();

    let cfg = config::load_in(dir);

    assert_eq!(cfg.update.latest_version, "1.6.0");
    assert_eq!(cfg.update.last_checked_at, "2026-07-09T18:11:41.390888319Z");
    // A JSON null folds down to an empty string.
    assert_eq!(cfg.update.dismissed_version, "");
    assert!(cfg.update.check_enabled);
    assert!(!legacy.exists(), "legacy version.json must be removed");

    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains(r#"latest_version = "1.6.0""#));
}

#[test]
fn save_merge_models_round_trips_and_preserves_comments() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    config::load_in(dir); // materialize defaults

    config::save_merge_models_in(dir, true).unwrap();

    let cfg = config::load_in(dir);
    assert!(cfg.usage.merge_models);
    assert!(cfg.usage.show_quota_panels); // untouched

    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("merge_models = true"));
    // The edit is format-preserving, so comments stay put.
    assert!(text.contains("# Show the live quota panels"));
}

#[test]
fn record_update_check_preserves_unrelated_fields() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    config::load_in(dir);
    config::save_merge_models_in(dir, true).unwrap();

    config::record_update_check_in(dir, "2.0.0").unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.update.latest_version, "2.0.0");
    assert!(!cfg.update.last_checked_at.is_empty());
    assert!(cfg.usage.merge_models); // preserved across the update-check write
    assert!(cfg.update.check_enabled);
}

#[test]
fn existing_file_is_parsed_and_left_in_place() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_time_range = \"weekly\"\n\n[usage]\nmerge_models = true\nshow_quota_panels = false\n",
    )
    .unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(cfg.usage.merge_models);
    assert!(!cfg.usage.show_quota_panels);
    // A missing [update] section still defaults check_enabled to true.
    assert!(cfg.update.check_enabled);
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
