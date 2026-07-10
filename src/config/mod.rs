//! Persistent user settings (`~/.vct/config.toml`).
//!
//! A small typed [`Config`] read with serde and written back with `toml_edit`
//! so hand-added comments and any unknown keys survive programmatic edits (the
//! same "preserve what we don't own" idea as the credential write-back in
//! `quota::refresh`). Reads are infallible: a missing or malformed file falls
//! back to defaults.
//!
//! On the first run (no `config.toml` yet) the file is created from a commented
//! template, folding in the legacy `~/.vct/version.json` update-check state if
//! present and then removing it.

use crate::cli::TimeRange;
use crate::utils::{get_cache_dir, now_rfc3339_utc_nanos, write_string_atomic};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use toml_edit::{DocumentMut, value};

/// The commented default written to `~/.vct/config.toml` on first run.
const DEFAULT_TEMPLATE: &str = r#"# ~/.vct/config.toml — Vibe Coding Tracker settings (auto-generated)

[general]
# Default time range when no --daily/--weekly/--monthly/--all flag is given.
# one of: "daily" | "weekly" | "monthly" | "all"
default_time_range = "all"

[usage]
# Start the usage dashboard with models merged across provider prefixes.
# Toggled live with `m`; the last state is saved back here.
merge_models = false
# Show the live quota panels (Claude / Codex / Copilot / Cursor) in the usage TUI.
show_quota_panels = true

[update]
# Whether vct performs *automatic* update checks (explicit `vct update` always runs).
check_enabled = true
# --- managed automatically (folded from the old version.json) ---
latest_version = ""
last_checked_at = ""
dismissed_version = ""
"#;

/// The full settings document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub update: UpdateConfig,
}

/// `[general]` — settings shared across subcommands.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Default period when the user passes no `--daily/--weekly/--monthly/--all`.
    #[serde(default)]
    pub default_time_range: TimeRange,
}

/// `[usage]` — usage dashboard preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageConfig {
    /// Seed the dashboard with provider-prefix merging on.
    #[serde(default)]
    pub merge_models: bool,
    /// Show the live quota panels in the usage TUI.
    #[serde(default = "default_true")]
    pub show_quota_panels: bool,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            merge_models: false,
            show_quota_panels: true,
        }
    }
}

/// `[update]` — self-update state, folded from the legacy `version.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Whether vct performs automatic update checks (groundwork; explicit
    /// `vct update` always runs regardless).
    #[serde(default = "default_true")]
    pub check_enabled: bool,
    /// Latest release tag seen on GitHub (empty when unknown).
    #[serde(default)]
    pub latest_version: String,
    /// When the update check last ran (RFC3339 UTC nanos; empty when never).
    #[serde(default)]
    pub last_checked_at: String,
    /// A release the user asked not to be reminded about (empty when none).
    #[serde(default)]
    pub dismissed_version: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check_enabled: true,
            latest_version: String::new(),
            last_checked_at: String::new(),
            dismissed_version: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Loads settings from `~/.vct/config.toml`, creating it with defaults (and
/// migrating a legacy `version.json`) on first run.
///
/// Infallible: any error resolving, reading, or parsing degrades to
/// [`Config::default`].
pub fn load() -> Config {
    match get_cache_dir() {
        Ok(dir) => load_in(&dir),
        Err(_) => Config::default(),
    }
}

/// [`load`] rooted at an explicit directory (test seam).
pub fn load_in(dir: &Path) -> Config {
    let path = dir.join("config.toml");
    if path.exists() {
        return std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml_edit::de::from_str(&text).ok())
            .unwrap_or_default();
    }
    // First run: materialize the commented template, folding in any legacy
    // version.json, and remove the old file once its values are carried over.
    let mut doc = default_document();
    migrate_legacy_version(dir, &mut doc);
    let text = doc.to_string();
    let _ = write_string_atomic(&path, &text);
    toml_edit::de::from_str(&text).unwrap_or_default()
}

/// Persists the usage dashboard's merge toggle back to the config.
pub fn save_merge_models(enabled: bool) -> Result<()> {
    save_merge_models_in(&get_cache_dir()?, enabled)
}

/// [`save_merge_models`] rooted at an explicit directory (test seam).
pub fn save_merge_models_in(dir: &Path, enabled: bool) -> Result<()> {
    edit_in(dir, |doc| {
        doc["usage"]["merge_models"] = value(enabled);
    })
}

/// Records that an update check just saw `latest` on GitHub, preserving
/// `check_enabled` / `dismissed_version` and any comments.
pub fn record_update_check(latest: &str) -> Result<()> {
    record_update_check_in(&get_cache_dir()?, latest)
}

/// [`record_update_check`] rooted at an explicit directory (test seam).
pub fn record_update_check_in(dir: &Path, latest: &str) -> Result<()> {
    let now = now_rfc3339_utc_nanos();
    edit_in(dir, |doc| {
        doc["update"]["latest_version"] = value(latest);
        doc["update"]["last_checked_at"] = value(now);
    })
}

/// Reads the current document (or the template when absent/malformed), applies
/// `mutate`, and writes it back atomically — preserving formatting and comments.
fn edit_in(dir: &Path, mutate: impl FnOnce(&mut DocumentMut)) -> Result<()> {
    let path = dir.join("config.toml");
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .unwrap_or_else(default_document);
    mutate(&mut doc);
    write_string_atomic(&path, &doc.to_string())
}

/// Parses the built-in template into an editable document.
fn default_document() -> DocumentMut {
    DEFAULT_TEMPLATE
        .parse::<DocumentMut>()
        .expect("built-in config template must be valid TOML")
}

/// Folds a legacy `~/.vct/version.json` into `doc`'s `[update]` section, then
/// removes it. No-op when the file is absent.
fn migrate_legacy_version(dir: &Path, doc: &mut DocumentMut) {
    let legacy = dir.join("version.json");
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return;
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        let field = |key: &str| {
            v.get(key)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        doc["update"]["latest_version"] = value(field("latest_version"));
        doc["update"]["last_checked_at"] = value(field("last_checked_at"));
        doc["update"]["dismissed_version"] = value(field("dismissed_version"));
    }
    let _ = std::fs::remove_file(&legacy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_expected_defaults() {
        let cfg: Config = toml_edit::de::from_str(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.general.default_time_range, TimeRange::All);
        assert!(cfg.usage.show_quota_panels);
        assert!(!cfg.usage.merge_models);
        assert!(cfg.update.check_enabled);
    }

    #[test]
    fn missing_sections_use_true_defaults() {
        // An empty file must still default the opt-out booleans to true, which
        // only holds because UsageConfig / UpdateConfig impl Default by hand.
        let cfg: Config = toml_edit::de::from_str("").unwrap();
        assert!(cfg.usage.show_quota_panels);
        assert!(cfg.update.check_enabled);
    }

    #[test]
    fn partial_usage_section_keeps_panel_default() {
        let cfg: Config = toml_edit::de::from_str("[usage]\nmerge_models = true\n").unwrap();
        assert!(cfg.usage.merge_models);
        assert!(cfg.usage.show_quota_panels);
    }
}
