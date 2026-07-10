//! Integration tests for the persistent settings file (`~/.vct/config.toml`).
//!
//! Every case is rooted at a `TempHome`'s `~/.vct` via the `config::*_in` seams,
//! so nothing touches the real home directory or process environment.

mod common;

use common::TempHome;
use std::fs;
use vibe_coding_tracker::TimeRange;
use vibe_coding_tracker::cli::resolve_time_range_with_default;
use vibe_coding_tracker::config::{self, Config, CursorUsageSource};

#[test]
fn load_in_creates_default_commented_file_when_absent() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;

    let cfg = config::load_in(dir);

    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.general.default_time_range, TimeRange::All);
    assert!(cfg.usage.show_quota_panels);
    assert!(!cfg.usage.merge_models);
    // New sections default sensibly.
    assert!(cfg.providers.cursor);
    assert_eq!(cfg.cursor.usage_source, CursorUsageSource::Local);
    assert_eq!(cfg.usage.refresh_interval_secs, 10);
    assert_eq!(cfg.analysis.refresh_interval_secs, 10);

    let path = dir.join("config.toml");
    assert!(path.exists(), "first run must create config.toml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[usage]"));
    assert!(text.contains("[providers]"));
    assert!(text.contains("[cursor]"));
    assert!(text.contains("merge_models = false"));
    assert!(text.contains(r#"usage_source = "local""#));
    // A header comment must survive so the file is self-documenting.
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
    assert!(cfg.usage.show_quota_panels); // untouched

    let text = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("merge_models = true"));
    // The edit is format-preserving, so comments stay put.
    assert!(text.contains("# Show the live quota panels"));
    assert!(text.contains("[cursor]"));
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
fn existing_file_is_parsed_and_left_in_place() {
    let th = TempHome::new();
    let dir = &th.paths.cache_dir;
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_time_range = \"weekly\"\n\n[usage]\nmerge_models = true\nshow_quota_panels = false\n\n[providers]\ncursor = false\n\n[cursor]\nusage_source = \"api\"\n",
    )
    .unwrap();

    let cfg = config::load_in(dir);
    assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
    assert!(cfg.usage.merge_models);
    assert!(!cfg.usage.show_quota_panels);
    // A missing provider key still defaults to true; the given one is honored.
    assert!(!cfg.providers.cursor);
    assert!(cfg.providers.claude);
    assert_eq!(cfg.cursor.usage_source, CursorUsageSource::Api);
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
