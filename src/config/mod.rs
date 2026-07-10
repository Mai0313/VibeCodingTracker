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
use crate::utils::{get_cache_dir, write_string_atomic};
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
# Which live quota panels to show in the usage TUI. Remove a name to hide that
# panel; use an empty list ([]) to hide the whole band.
quota_panels = ["claude", "codex", "copilot", "cursor"]
# Seconds between automatic refreshes of the usage TUI (minimum 1).
refresh_interval_secs = 10

[analysis]
# Seconds between automatic refreshes of the analysis TUI (minimum 1).
refresh_interval_secs = 10

[providers]
# Include each provider's sessions in usage / analysis. Set a provider to false
# to skip it entirely (no directory scan, no API) — e.g. cursor = false for
# someone who does not use Cursor.
claude = true
codex = true
copilot = true
gemini = true
opencode = true
cursor = true
"#;

/// The full settings document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub analysis: AnalysisConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
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
    /// Which live quota panels to show in the usage TUI (by provider name:
    /// `claude` / `codex` / `copilot` / `cursor`). An empty list hides the band.
    #[serde(default = "default_quota_panels")]
    pub quota_panels: Vec<String>,
    /// Seconds between automatic usage-TUI refreshes.
    #[serde(default = "default_refresh_secs")]
    pub refresh_interval_secs: u64,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            merge_models: false,
            quota_panels: default_quota_panels(),
            refresh_interval_secs: default_refresh_secs(),
        }
    }
}

impl UsageConfig {
    /// The refresh cadence, clamped to a sane minimum so a `0` cannot busy-loop.
    pub fn refresh_secs(&self) -> u64 {
        self.refresh_interval_secs.max(1)
    }

    /// Whether the quota panel for `provider` is enabled (case-insensitive).
    pub fn shows_quota_panel(&self, provider: &str) -> bool {
        self.quota_panels
            .iter()
            .any(|p| p.eq_ignore_ascii_case(provider))
    }
}

/// `[analysis]` — analysis dashboard preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Seconds between automatic analysis-TUI refreshes.
    #[serde(default = "default_refresh_secs")]
    pub refresh_interval_secs: u64,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: default_refresh_secs(),
        }
    }
}

impl AnalysisConfig {
    /// The refresh cadence, clamped to a sane minimum so a `0` cannot busy-loop.
    pub fn refresh_secs(&self) -> u64 {
        self.refresh_interval_secs.max(1)
    }
}

/// `[providers]` — per-provider include toggles.
///
/// Each provider defaults to `true`; setting one to `false` skips it entirely
/// (no directory scan, no API) in both the usage and analysis roll-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default = "default_true")]
    pub claude: bool,
    #[serde(default = "default_true")]
    pub codex: bool,
    #[serde(default = "default_true")]
    pub copilot: bool,
    #[serde(default = "default_true")]
    pub gemini: bool,
    #[serde(default = "default_true")]
    pub opencode: bool,
    #[serde(default = "default_true")]
    pub cursor: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            claude: true,
            codex: true,
            copilot: true,
            gemini: true,
            opencode: true,
            cursor: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_refresh_secs() -> u64 {
    10
}

fn default_quota_panels() -> Vec<String> {
    ["claude", "codex", "copilot", "cursor"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Loads settings from `~/.vct/config.toml`, creating it with defaults on first
/// run.
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
    // First run: materialize the commented template.
    let text = default_document().to_string();
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
        // A hand-edited file could make `usage` a scalar (`usage = "bad"`);
        // replace any non-table-like value with an empty table so indexing into
        // it below cannot panic. `is_table_like` accepts both the `[usage]`
        // header form AND an inline table (`usage = { ... }`), so a valid config
        // in either form is left intact (its keys/comments preserved) — only a
        // genuine scalar is repaired.
        if !doc["usage"].is_table_like() {
            doc["usage"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["usage"]["merge_models"] = value(enabled);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_expected_defaults() {
        let cfg: Config = toml_edit::de::from_str(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.general.default_time_range, TimeRange::All);
        assert_eq!(cfg.usage.quota_panels, default_quota_panels());
        assert!(cfg.usage.shows_quota_panel("cursor"));
        assert!(!cfg.usage.merge_models);
        assert_eq!(cfg.usage.refresh_interval_secs, 10);
        assert_eq!(cfg.analysis.refresh_interval_secs, 10);
        assert_eq!(cfg.providers, ProvidersConfig::default());
        assert!(cfg.providers.cursor);
    }

    #[test]
    fn missing_sections_use_defaults() {
        // An empty file must still default the opt-out settings, which only holds
        // because the section structs impl Default by hand.
        let cfg: Config = toml_edit::de::from_str("").unwrap();
        assert_eq!(cfg.usage.quota_panels, default_quota_panels());
        assert!(cfg.providers.cursor);
        assert_eq!(cfg.usage.refresh_secs(), 10);
    }

    #[test]
    fn partial_usage_section_keeps_panel_default() {
        let cfg: Config = toml_edit::de::from_str("[usage]\nmerge_models = true\n").unwrap();
        assert!(cfg.usage.merge_models);
        assert_eq!(cfg.usage.quota_panels, default_quota_panels());
    }

    #[test]
    fn quota_panels_can_be_narrowed_or_emptied() {
        let cfg: Config =
            toml_edit::de::from_str("[usage]\nquota_panels = [\"claude\"]\n").unwrap();
        assert!(cfg.usage.shows_quota_panel("claude"));
        assert!(!cfg.usage.shows_quota_panel("cursor"));

        let empty: Config = toml_edit::de::from_str("[usage]\nquota_panels = []\n").unwrap();
        assert!(!empty.usage.shows_quota_panel("claude"));
    }

    #[test]
    fn refresh_secs_clamps_zero_to_one() {
        let cfg = UsageConfig {
            refresh_interval_secs: 0,
            ..UsageConfig::default()
        };
        assert_eq!(cfg.refresh_secs(), 1);
    }

    #[test]
    fn providers_default_all_enabled() {
        let p = ProvidersConfig::default();
        assert!(p.claude && p.codex && p.copilot && p.gemini && p.opencode && p.cursor);
    }
}
