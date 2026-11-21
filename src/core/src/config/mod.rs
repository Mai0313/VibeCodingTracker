//! Persistent user settings stored in `~/.vct/config.toml`.
//!
//! Programmatic writes edit the TOML AST via `toml_edit`, preserving hand-added comments and unknown keys. Reads are infallible and degrade to [`Config::default`] when the file is missing or malformed.
//!
//! The typed config structs generate both the JSON schema (`schemars`) and the commented default file on first run, prefixed with a `#:schema` directive. Legacy standard-table files are migrated in place on load; inline tables are left untouched on disk and normalized in memory at read time.

use crate::models::{ExtensionType, TimeRange};
use crate::utils::{get_cache_dir, resolve_paths, write_string_atomic};
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, value};

/// Published JSON schema URL written into the leading `#:schema` directive of `config.toml`.
const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/Mai0313/VibeCodingTracker/main/vct.schema.json";

/// Current `config.toml` layout version stamped into `[general].version`; files without the key read as version 1.
const CONFIG_VERSION: u32 = 4;

/// Quota panels introduced after version 1; back-filled once into older files, never resurrecting user-removed panels (bump [`CONFIG_VERSION`] when extending).
const PANELS_ADDED_BY_VERSION: &[(u32, &str)] = &[(2, "grok"), (4, "antigravity")];

/// The full settings document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub analysis: AnalysisConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub remotes: RemotesConfig,
}

/// `[performance]` - controls for CPU-bound local scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceConfig {
    /// Rayon workers used by CLI session scans. `0` selects the measured auto
    /// default; a positive value is capped at the machine's available parallelism.
    #[serde(default)]
    pub scan_threads: usize,
}

impl PerformanceConfig {
    /// Returns the CLI scan worker count.
    ///
    /// Uses `scan_threads` when positive, else a positive `RAYON_NUM_THREADS`, else `ceil(available / 2)` clamped to `2..=8`; the final result is clamped to `1..=available`.
    pub fn resolved_scan_threads(&self) -> usize {
        let available = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        self.resolved_scan_threads_with(
            available,
            std::env::var("RAYON_NUM_THREADS").ok().as_deref(),
        )
    }

    fn resolved_scan_threads_with(&self, available: usize, rayon_env: Option<&str>) -> usize {
        let available = available.max(1);
        let requested = if self.scan_threads > 0 {
            self.scan_threads
        } else {
            rayon_env
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or_else(|| available.div_ceil(2).clamp(2, 8))
        };
        requested.clamp(1, available)
    }
}

/// Default interactive TUI layout identifier.
pub const DEFAULT_LAYOUT: &str = "classic";

/// `[general]` — settings shared across subcommands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GeneralConfig {
    /// Interactive TUI layout plugin name (default: `classic`; unknown names fall back to `classic`).
    #[serde(default = "default_layout")]
    pub layout: String,
    /// Default time range when no --daily/--weekly/--monthly/--all flag is given.
    /// One of: "daily" | "weekly" | "monthly" | "all".
    #[serde(default)]
    pub default_time_range: TimeRange,
    /// Check for a newer vct release when the CLI starts.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Layout version of this file, stamped by vct. Only the upgrade pass reads
    /// it; leave it alone unless you want a past upgrade to run again.
    #[serde(
        default = "legacy_config_version",
        deserialize_with = "de_config_version"
    )]
    pub version: u32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            layout: default_layout(),
            default_time_range: TimeRange::default(),
            auto_update: true,
            // Newly constructed defaults use the current version; only files loaded without the key default to legacy.
            version: CONFIG_VERSION,
        }
    }
}

/// `[usage]` — usage dashboard preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageConfig {
    /// Merge models that share a base name across provider prefixes.
    #[serde(default = "default_true")]
    pub merge_models: bool,
    /// Seconds between automatic refreshes of the Usage tab (minimum 1).
    #[serde(default = "default_refresh_secs")]
    pub refresh_interval: u64,
    /// Live quota-panel preferences.
    #[serde(default)]
    pub quota: QuotaConfig,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            merge_models: true,
            refresh_interval: default_refresh_secs(),
            quota: QuotaConfig::default(),
        }
    }
}

impl UsageConfig {
    /// Returns the Usage tab refresh interval in seconds, clamped to at least 1.
    pub fn refresh_secs(&self) -> u64 {
        self.refresh_interval.max(1)
    }

    /// Returns the live quota-panel poll interval in seconds, clamped to at least 1.
    pub fn quota_refresh_secs(&self) -> u64 {
        self.quota.refresh_interval.max(1)
    }

    /// Returns whether the quota panel for `provider` is enabled (case-insensitive).
    pub fn shows_quota_panel(&self, provider: &str) -> bool {
        quota_panel_selected(&self.quota.panels, provider)
    }
}

/// `[usage.quota]` — live quota-panel preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QuotaConfig {
    /// Which live quota panels to show in the app (by provider name:
    /// `claude` / `codex` / `copilot` / `cursor` / `grok` / `antigravity`). An
    /// empty list hides every quota card.
    #[serde(default = "default_quota_panels")]
    pub panels: Vec<String>,
    /// Seconds between live quota-panel polls, shared by every provider
    /// (minimum 1). Higher is safer against a provider's rate limits.
    /// The Antigravity panel never polls more often than every 300 seconds, since
    /// each poll starts the agy CLI.
    #[serde(default = "default_quota_refresh_secs")]
    pub refresh_interval: u64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            panels: default_quota_panels(),
            refresh_interval: default_quota_refresh_secs(),
        }
    }
}

/// Returns whether `name` matches an entry in `panels` (case-insensitive).
pub fn quota_panel_selected(panels: &[String], name: &str) -> bool {
    panels.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// `[analysis]` — analysis dashboard preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisConfig {
    /// Seconds between automatic refreshes of the Analysis tab (minimum 1).
    #[serde(default = "default_refresh_secs")]
    pub refresh_interval: u64,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            refresh_interval: default_refresh_secs(),
        }
    }
}

impl AnalysisConfig {
    /// Returns the Analysis tab refresh interval in seconds, clamped to at least 1.
    pub fn refresh_secs(&self) -> u64 {
        self.refresh_interval.max(1)
    }
}

/// `[providers]` — per-provider include toggles.
///
/// Each provider defaults to `true`; setting one to `false` skips it entirely
/// (no directory scan, no API) in both the usage and analysis roll-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    #[serde(default = "default_true")]
    pub hermes: bool,
    #[serde(default = "default_true")]
    pub grok: bool,
    #[serde(default = "default_true")]
    pub dsh: bool,
    #[serde(default = "default_true")]
    pub antigravity: bool,
}

impl ProvidersConfig {
    /// Returns whether `provider` is enabled.
    pub fn includes(&self, provider: ExtensionType) -> bool {
        match provider {
            ExtensionType::ClaudeCode => self.claude,
            ExtensionType::Codex => self.codex,
            ExtensionType::Copilot => self.copilot,
            ExtensionType::Gemini => self.gemini,
            ExtensionType::OpenCode => self.opencode,
            ExtensionType::Cursor => self.cursor,
            ExtensionType::Hermes => self.hermes,
            ExtensionType::Grok => self.grok,
            ExtensionType::DeepSeek => self.dsh,
            ExtensionType::Antigravity => self.antigravity,
        }
    }
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
            hermes: true,
            grok: true,
            dsh: true,
            antigravity: true,
        }
    }
}

/// `[remotes]` — other machines whose usage is merged into this one's.
///
/// vct runs `vct ledger export` on each host over ssh and adds the sessions it
/// returns to the usage and analysis views. Only computed totals cross the
/// connection, never conversations. Missing vct on a Linux or macOS remote is
/// installed into `~/.local/bin` automatically over ssh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemotesConfig {
    /// ssh destinations to merge, as you would pass them to `ssh`: a host
    /// name, `user@host`, or an alias from `~/.ssh/config`. Each must accept a
    /// non-interactive login (a key or an agent, no password prompt).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// How to start vct on the hosts, for example "/opt/vct/bin/vct". Empty
    /// looks for it on the remote PATH, in ~/.local/bin, ~/.cargo/bin and
    /// under ~/.nvm, and installs into ~/.local/bin if missing.
    #[serde(default)]
    pub command: String,
    /// Seconds between fetches while the app is open (minimum 60).
    #[serde(default = "default_remotes_refresh_interval")]
    pub refresh_interval: u64,
}

impl Default for RemotesConfig {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            command: String::new(),
            refresh_interval: default_remotes_refresh_interval(),
        }
    }
}

fn default_remotes_refresh_interval() -> u64 {
    300
}

/// `[logging]` — file logging preferences.
///
/// Diagnostics are written to `~/.vct/logs/vct-YYYY-MM-DD.log` (plain text, one
/// line per record). Nothing is ever printed to the terminal, so the TUI is
/// never disturbed. The log file is created lazily on the first record, so a
/// command that logs nothing never touches `~/.vct`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoggingConfig {
    /// Minimum level written to the log file.
    /// One of: "off" | "error" | "warn" | "info" | "debug" | "trace".
    #[serde(default)]
    pub level: LogLevel,
    /// Days of daily log files to keep; older `vct-*.log` files are pruned on
    /// startup. `0` keeps every file.
    #[serde(default = "default_log_retention_days")]
    pub retention_days: u32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::default(),
            retention_days: default_log_retention_days(),
        }
    }
}

/// Minimum severity written to the log file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Disable file logging entirely.
    Off,
    /// Only errors.
    Error,
    /// Errors and warnings (the default — quiet when healthy, records problems).
    #[default]
    Warn,
    /// Adds high-level informational breadcrumbs.
    Info,
    /// Adds verbose diagnostics (per-file parse skips, quota poll detail).
    Debug,
    /// Everything, including cache-hit traces.
    Trace,
}

fn default_true() -> bool {
    true
}

fn default_layout() -> String {
    DEFAULT_LAYOUT.to_string()
}

fn default_log_retention_days() -> u32 {
    7
}

fn default_refresh_secs() -> u64 {
    10
}

fn default_quota_refresh_secs() -> u64 {
    60
}

/// Returns the legacy version assumed when `[general].version` is absent.
fn legacy_config_version() -> u32 {
    1
}

/// Deserializes `[general].version`, falling back to [`legacy_config_version`] on non-integer values so malformed markers do not reset the whole config.
fn de_config_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)
        .ok()
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or_else(legacy_config_version))
}

fn default_quota_panels() -> Vec<String> {
    [
        "claude",
        "codex",
        "copilot",
        "cursor",
        "grok",
        "antigravity",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Loads settings from `~/.vct/config.toml`, creating it with defaults on first run.
///
/// Infallible: any error resolving, reading, or parsing degrades to [`Config::default`].
pub fn load() -> Config {
    match get_cache_dir() {
        Ok(dir) => load_in(&dir),
        Err(_) => Config::default(),
    }
}

/// Reads settings from `~/.vct/config.toml` without creating or modifying files; legacy layouts are migrated in memory only.
///
/// Infallible: any error resolving, reading, or parsing degrades to [`Config::default`].
pub fn load_read_only() -> Config {
    match resolve_paths() {
        Ok(paths) => load_read_only_in(&paths.cache_dir),
        Err(_) => Config::default(),
    }
}

/// Returns whether `~/.vct/config.toml` is absent or parses cleanly as [`Config`].
///
/// Distinguishes an intentionally empty setting from a malformed file that [`load`] silently defaulted.
pub fn file_is_valid() -> bool {
    resolve_paths().is_ok_and(|paths| file_is_valid_in(&paths.cache_dir))
}

/// Returns whether `config.toml` under `dir` is absent or parses cleanly as [`Config`].
pub fn file_is_valid_in(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join("config.toml")) {
        Ok(text) => toml_edit::de::from_str::<Config>(&text).is_ok(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Reads settings from `config.toml` under `dir` without writing to disk.
pub fn load_read_only_in(dir: &Path) -> Config {
    let path = dir.join("config.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Config::default();
    };
    parse_config_text(&text)
}

/// Loads settings from `config.toml` under `dir`, creating or migrating it in place.
pub fn load_in(dir: &Path) -> Config {
    let path = dir.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // First run: materialize the generated commented template.
            let text = default_document().to_string();
            let _ = write_string_atomic(&path, &text);
            return Config::default();
        }
        Err(_) => return Config::default(),
    };
    let Ok(mut doc) = text.parse::<DocumentMut>() else {
        return Config::default();
    };
    // Best-effort in-place migration: write failures still yield the migrated in-memory Config, and malformed TOML is never overwritten.
    if migrate_document(&mut doc, &text) {
        let _ = write_string_atomic(&path, &doc.to_string());
    }
    parse_document_config(doc)
}

/// Parses TOML text with the same in-memory migration as [`load_in`] without writing to disk.
fn parse_config_text(text: &str) -> Config {
    let Ok(mut doc) = text.parse::<DocumentMut>() else {
        return Config::default();
    };
    migrate_document(&mut doc, text);
    parse_document_config(doc)
}

fn parse_document_config(doc: DocumentMut) -> Config {
    // Backstop for legacy forms the structural pass leaves alone (such as inline tables).
    let legacy = LegacyOverrides::extract(&doc);
    let mut config: Config = toml_edit::de::from_document(doc).unwrap_or_default();
    legacy.apply(&mut config);
    config
}

/// Outcome of [`migrate_config_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStatus {
    /// No file existed, so a fresh commented default was written.
    Created,
    /// A legacy-format file was rewritten to the current on-disk format.
    Migrated,
    /// The file was already current; nothing was written.
    AlreadyCurrent,
}

/// Migrates `path` to the current on-disk layout in place, creating the commented default when absent.
///
/// # Errors
///
/// Returns an error if reading, parsing, or writing `path` fails.
pub fn migrate_config_file(path: &Path) -> Result<MigrationStatus> {
    if !path.exists() {
        write_string_atomic(path, &default_document().to_string())?;
        return Ok(MigrationStatus::Created);
    }
    let text = std::fs::read_to_string(path)?;
    match migrate_text(&text)? {
        Some(migrated) => {
            write_string_atomic(path, &migrated)?;
            Ok(MigrationStatus::Migrated)
        }
        None => Ok(MigrationStatus::AlreadyCurrent),
    }
}

/// Applies structural migrations to raw TOML config text.
///
/// Returns `Ok(Some(migrated))` if the document changed, or `Ok(None)` if it is already current.
///
/// # Errors
///
/// Returns an error if `text` is not valid TOML.
pub fn migrate_text(text: &str) -> Result<Option<String>> {
    let mut doc: DocumentMut = text.parse()?;
    Ok(migrate_document(&mut doc, text).then(|| doc.to_string()))
}

/// Read-time fallbacks extracted from legacy keys (`quota_panels`, `refresh_interval_secs`) that structural migration leaves untouched.
struct LegacyOverrides {
    quota_panels: Option<Vec<String>>,
    usage_refresh_interval: Option<u64>,
    analysis_refresh_interval: Option<u64>,
}

impl LegacyOverrides {
    fn extract(doc: &DocumentMut) -> Self {
        let mut out = Self {
            quota_panels: None,
            usage_refresh_interval: None,
            analysis_refresh_interval: None,
        };
        if let Some(usage) = doc.get("usage").and_then(Item::as_table_like) {
            // A present `[usage.quota].panels` (even `[]`) always overrides legacy `quota_panels`.
            let has_new_panels = usage
                .get("quota")
                .and_then(Item::as_table_like)
                .is_some_and(|q| q.contains_key("panels"));
            if !has_new_panels
                && let Some(legacy) = usage.get("quota_panels").and_then(Item::as_array)
            {
                out.quota_panels = Some(
                    legacy
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                );
            }
            out.usage_refresh_interval = legacy_refresh_secs(usage);
        }
        if let Some(analysis) = doc.get("analysis").and_then(Item::as_table_like) {
            out.analysis_refresh_interval = legacy_refresh_secs(analysis);
        }
        out
    }

    fn apply(self, config: &mut Config) {
        if let Some(panels) = self.quota_panels {
            config.usage.quota.panels = panels;
        }
        if let Some(secs) = self.usage_refresh_interval {
            config.usage.refresh_interval = secs;
        }
        if let Some(secs) = self.analysis_refresh_interval {
            config.analysis.refresh_interval = secs;
        }
    }
}

/// Applies read-time fallbacks for legacy keys (`quota_panels`, `refresh_interval_secs`) that structural migration leaves untouched.
#[cfg(test)]
fn migrate_legacy(config: &mut Config, raw: &str) {
    let Ok(doc) = raw.parse::<DocumentMut>() else {
        return;
    };
    LegacyOverrides::extract(&doc).apply(config);
}

/// Reads legacy `refresh_interval_secs` when `refresh_interval` is absent, avoiding a serde `alias` that would reject files carrying both keys.
fn legacy_refresh_secs(section: &dyn TableLike) -> Option<u64> {
    if section.contains_key("refresh_interval") {
        return None;
    }
    section
        .get("refresh_interval_secs")
        .and_then(Item::as_integer)
        .and_then(|secs| u64::try_from(secs).ok())
}

/// Runs idempotent structural migrations on standard `[header]` tables, returning whether `doc` changed.
fn migrate_document(doc: &mut DocumentMut, raw: &str) -> bool {
    let mut changed = false;

    // Migrate `quota_panels` into `[usage.quota]` after leaf renames so sub-tables stay after leaf keys.
    if doc.get("usage").is_some_and(Item::is_table) {
        if let Some(usage) = doc.get_mut("usage").and_then(Item::as_table_mut) {
            changed |= rename_refresh_key(usage, &["usage", "refresh_interval"]);
        }
        changed |= migrate_quota_panels(doc);
    }

    if let Some(analysis) = doc.get_mut("analysis").and_then(Item::as_table_mut) {
        changed |= rename_refresh_key(analysis, &["analysis", "refresh_interval"]);
    }

    // Runs after `migrate_quota_panels` so it sees any newly relocated panel list.
    changed |= backfill_new_quota_panels(doc);

    // Runs last so the directive attaches to the first element even if `backfill_new_quota_panels` created `[general]` on an empty file.
    // Skip serializing `doc` when no structural edit ran: `doc` still matches `raw`.
    let has_directive = if changed {
        has_schema_directive(doc)
    } else {
        has_schema_directive_str(raw)
    };
    if !has_directive {
        changed |= prepend_schema_directive(doc);
    }

    changed
}

/// Back-fills quota panels added after `[general].version` and stamps [`CONFIG_VERSION`], skipping empty panel lists and inline tables.
fn backfill_new_quota_panels(doc: &mut DocumentMut) -> bool {
    let from_version = match doc.get("general") {
        Some(item) => {
            let Some(general) = item.as_table() else {
                // Inline `general = { ... }` has no standard table to stamp the version marker.
                return false;
            };
            general
                .get("version")
                .and_then(Item::as_integer)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or_else(legacy_config_version)
        }
        None => legacy_config_version(),
    };
    if from_version >= CONFIG_VERSION {
        return false;
    }

    // Leave the version unstamped when the panel list lives in an inline table so the quota upgrade stays eligible later; still backfill `[general].auto_update`.
    if panels_out_of_reach(doc) {
        return backfill_auto_update(doc);
    }

    let mut changed = false;
    if let Some(panels) = doc
        .get_mut("usage")
        .and_then(Item::as_table_mut)
        .and_then(|usage| usage.get_mut("quota"))
        .and_then(Item::as_table_mut)
        .and_then(|quota| quota.get_mut("panels"))
        .and_then(Item::as_array_mut)
        // Keep an explicit empty list intact so a hidden quota band stays off.
        .filter(|panels| !panels.is_empty())
    {
        let missing: Vec<&str> = PANELS_ADDED_BY_VERSION
            .iter()
            .filter(|(version, _)| from_version < *version)
            .map(|(_, name)| *name)
            .filter(|name| {
                !panels
                    .iter()
                    .filter_map(|p| p.as_str())
                    .any(|p| p.eq_ignore_ascii_case(name))
            })
            .collect();
        for name in missing {
            panels.push(name);
            changed = true;
        }
    }

    changed |= stamp_config_version(doc);
    changed |= backfill_auto_update(doc);
    changed
}

/// Inserts `auto_update = true` into a standard `[general]` table when missing, leaving inline tables untouched.
fn backfill_auto_update(doc: &mut DocumentMut) -> bool {
    let Some(general) = doc.get_mut("general").and_then(Item::as_table_mut) else {
        return false;
    };
    if !general.contains_key("auto_update") {
        let schema = cached_json_schema();
        general.insert("auto_update", value(true));
        apply_comment(
            general,
            "auto_update",
            "",
            schema,
            &["general", "auto_update"],
        );
        return true;
    }
    false
}

/// Returns whether the effective quota panel list sits inside an inline table that structural migration cannot edit.
fn panels_out_of_reach(doc: &DocumentMut) -> bool {
    let Some(usage) = doc.get("usage") else {
        return false;
    };
    let holds_panels = |table: &dyn TableLike| {
        table.contains_key("quota_panels")
            || table
                .get("quota")
                .and_then(Item::as_table_like)
                .is_some_and(|quota| quota.contains_key("panels"))
    };
    match usage.as_table() {
        // Inline `usage = { ... }` cannot be rewritten.
        None => usage.as_table_like().is_some_and(holds_panels),
        Some(usage) => {
            // Standard `[usage]` with an inline `quota` child.
            let inline_quota = usage
                .get("quota")
                .filter(|quota| !quota.is_table())
                .and_then(Item::as_table_like)
                .is_some_and(|quota| quota.contains_key("panels"));
            // Or a legacy `quota_panels` left behind because `quota` is inline.
            inline_quota || usage.contains_key("quota_panels")
        }
    }
}

/// Stamps `[general].version` to [`CONFIG_VERSION`], creating `[general]` if absent.
fn stamp_config_version(doc: &mut DocumentMut) -> bool {
    let schema = cached_json_schema();
    let mut changed = false;
    if doc.get("general").is_none() {
        let mut table = Table::new();
        table.set_implicit(false);
        if let Some(desc) = schema_description(schema, &["general"]) {
            table
                .decor_mut()
                .set_prefix(format!("\n{}", comment_block(&desc)));
        }
        doc.insert("general", Item::Table(table));
        changed = true;
    }
    let Some(general) = doc.get_mut("general").and_then(Item::as_table_mut) else {
        return changed;
    };
    let had_key = general.contains_key("version");
    let version_is_current = general
        .get("version")
        .and_then(Item::as_integer)
        .is_some_and(|version| version == i64::from(CONFIG_VERSION));
    general.insert("version", value(i64::from(CONFIG_VERSION)));
    if !had_key {
        apply_comment(general, "version", "", schema, &["general", "version"]);
    }
    changed || !version_is_current
}

/// Returns whether the first non-blank line of `text` is a `#:schema` directive.
fn has_schema_directive_str(text: &str) -> bool {
    // Check only the first non-blank line so `#:schema` inside a multiline string or later comment does not suppress prepending.
    text.lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with("#:schema"))
}

/// Returns whether the first non-blank line of `doc` is a `#:schema` directive.
fn has_schema_directive(doc: &DocumentMut) -> bool {
    has_schema_directive_str(&doc.to_string())
}

/// Prepends the `#:schema` directive to the first element of `doc`, returning whether it was added.
fn prepend_schema_directive(doc: &mut DocumentMut) -> bool {
    let directive = format!("#:schema {SCHEMA_URL}\n");
    let root = doc.as_table_mut();
    let Some(first_key) = root.iter().next().map(|(k, _)| k.to_string()) else {
        return false;
    };
    if let Some(item) = root.get_mut(&first_key)
        && let Some(table) = item.as_table_mut()
    {
        let existing = decor_prefix(table.decor());
        table
            .decor_mut()
            .set_prefix(format!("{directive}{existing}"));
        return true;
    }
    if let Some(mut km) = root.key_mut(&first_key) {
        let existing = decor_prefix(km.leaf_decor());
        km.leaf_decor_mut()
            .set_prefix(format!("{directive}{existing}"));
        return true;
    }
    false
}

/// Returns the prefix trivia of `decor`, or an empty string.
fn decor_prefix(decor: &toml_edit::Decor) -> String {
    decor
        .prefix()
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string()
}

/// Migrates `refresh_interval_secs` to `refresh_interval` in `table`, dropping malformed legacy values so serde defaults apply instead of failing the file.
fn rename_refresh_key(table: &mut Table, comment_path: &[&str]) -> bool {
    if !table.contains_key("refresh_interval_secs") {
        return false;
    }
    // Both present: drop the legacy key since `refresh_interval` already wins.
    if table.contains_key("refresh_interval") {
        table.remove("refresh_interval_secs");
        return true;
    }
    let Some((old_key, item)) = table.remove_entry("refresh_interval_secs") else {
        return false;
    };
    if let Some(secs) = item.as_integer().and_then(|n| u64::try_from(n).ok()) {
        table.insert("refresh_interval", value(secs as i64));
        // Preserve hand-added comments; fall back to the schema description.
        let existing = decor_prefix(old_key.leaf_decor());
        apply_comment(
            table,
            "refresh_interval",
            &existing,
            cached_json_schema(),
            comment_path,
        );
    }
    true
}

/// Moves `[usage].quota_panels` into `[usage.quota].panels`, leaving inline `quota` tables untouched.
fn migrate_quota_panels(doc: &mut DocumentMut) -> bool {
    let Some(usage) = doc.get("usage").and_then(Item::as_table) else {
        return false;
    };
    if !usage.contains_key("quota_panels") {
        return false;
    }
    if usage.get("quota").is_some_and(Item::is_inline_table) {
        return false;
    }
    let has_new_panels = usage
        .get("quota")
        .and_then(Item::as_table_like)
        .is_some_and(|q| q.contains_key("panels"));

    let usage = doc
        .get_mut("usage")
        .and_then(Item::as_table_mut)
        .expect("usage table present");
    let Some((old_key, old_item)) = usage.remove_entry("quota_panels") else {
        return false;
    };
    if has_new_panels {
        // `[usage.quota].panels` already exists; drop the legacy key only.
        return true;
    }
    let legacy_panels: Vec<String> = old_item
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // Carry the legacy key's comment onto the new `panels` key.
    let comment = decor_prefix(old_key.leaf_decor());
    let schema = cached_json_schema();

    if usage.get("quota").and_then(Item::as_table).is_some() {
        let quota = usage
            .get_mut("quota")
            .and_then(Item::as_table_mut)
            .expect("quota table present");
        quota.insert("panels", value(panels_array(&legacy_panels)));
        apply_comment(
            quota,
            "panels",
            &comment,
            schema,
            &["usage", "quota", "panels"],
        );
    } else {
        usage.insert(
            "quota",
            Item::Table(build_quota_table(&legacy_panels, &comment, schema)),
        );
    }
    true
}

/// Converts a slice of panel names into a TOML [`Array`].
fn panels_array(panels: &[String]) -> Array {
    panels.iter().map(String::as_str).collect()
}

/// Builds a `[usage.quota]` table with `panels` and the default `refresh_interval`, annotated from `schema`.
fn build_quota_table(panels: &[String], panels_comment: &str, schema: &Value) -> Table {
    let mut table = Table::new();
    table.set_implicit(false);
    if let Some(desc) = schema_description(schema, &["usage", "quota"]) {
        table
            .decor_mut()
            .set_prefix(format!("\n{}", comment_block(&desc)));
    }
    table.insert("panels", value(panels_array(panels)));
    apply_comment(
        &mut table,
        "panels",
        panels_comment,
        schema,
        &["usage", "quota", "panels"],
    );
    table.insert(
        "refresh_interval",
        value(default_quota_refresh_secs() as i64),
    );
    apply_comment(
        &mut table,
        "refresh_interval",
        "",
        schema,
        &["usage", "quota", "refresh_interval"],
    );
    table
}

/// Attaches `existing` as `key`'s leading `#` comment when non-empty, else the field's schema description.
fn apply_comment(table: &mut Table, key: &str, existing: &str, schema: &Value, path: &[&str]) {
    let prefix = if existing.trim().is_empty() {
        schema_description(schema, path).map(|desc| format!("\n{}", comment_block(&desc)))
    } else {
        Some(existing.to_string())
    };
    if let Some(prefix) = prefix
        && let Some(mut km) = table.key_mut(key)
    {
        km.leaf_decor_mut().set_prefix(prefix);
    }
}

/// Looks up a nested property's `description` in an inlined JSON schema.
fn schema_description(schema: &Value, path: &[&str]) -> Option<String> {
    let mut cur = schema;
    for key in path {
        cur = cur.get("properties")?.get(key)?;
    }
    cur.get("description")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Opens `~/.vct/config.toml` in `$VISUAL`, `$EDITOR`, or `vi` (`notepad` on Windows), creating it first if missing, and waits for the editor to exit.
///
/// The caller owns the terminal: an interactive app must leave its alternate screen before calling this.
///
/// # Errors
///
/// Returns an error if the config path cannot be resolved, the editor command is empty, or the editor fails to launch or exits non-zero.
pub fn open_in_editor() -> Result<()> {
    use anyhow::Context;

    let path = crate::utils::get_config_path()?;
    // Ensure the file exists before handing it to the editor.
    let _ = load();
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.to_string());
    // Split `$EDITOR`/`$VISUAL` into program + args to support values like `code --wait` or `vim -f`.
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty editor command"))?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("Failed to launch editor '{editor}'"))?;
    if !status.success() {
        anyhow::bail!("Editor '{editor}' exited with {status}");
    }
    Ok(())
}

/// One value a settings screen reads from or writes to a [`Config`] field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// An on/off field.
    Bool(bool),
    /// A whole number, such as an interval in seconds.
    Integer(i64),
    /// Free text, or the name of an enum variant.
    String(String),
    /// A list of names.
    List(Vec<String>),
}

impl SettingValue {
    fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(flag) => Some(Self::Bool(*flag)),
            Value::Number(number) => number.as_i64().map(Self::Integer),
            Value::String(text) => Some(Self::String(text.clone())),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(Self::List),
            _ => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Bool(flag) => Value::from(*flag),
            Self::Integer(number) => Value::from(*number),
            Self::String(text) => Value::from(text.as_str()),
            Self::List(items) => Value::from(items.clone()),
        }
    }

    fn to_item(&self) -> Item {
        match self {
            Self::Bool(flag) => value(*flag),
            Self::Integer(number) => value(*number),
            Self::String(text) => value(text.as_str()),
            Self::List(items) => value(items.iter().map(String::as_str).collect::<Array>()),
        }
    }
}

/// Returns the scalar or string-list field at `path` (for example `["usage", "quota", "refresh_interval"]`), or `None` if absent or non-scalar.
pub fn get_setting(config: &Config, path: &[&str]) -> Option<SettingValue> {
    let root = serde_json::to_value(config).ok()?;
    let mut current = &root;
    for key in path {
        current = current.get(key)?;
    }
    SettingValue::from_json(current)
}

/// Updates the field at `path` in memory.
///
/// # Errors
///
/// Returns an error, leaving `config` untouched, if `path` names no field or `value` is incompatible with that field.
pub fn apply_setting(config: &mut Config, path: &[&str], value: &SettingValue) -> Result<()> {
    let mut root = serde_json::to_value(&*config)?;
    let mut current = &mut root;
    for key in path {
        current = current
            .get_mut(key)
            .ok_or_else(|| anyhow::anyhow!("unknown setting {}", path.join(".")))?;
    }
    *current = value.to_json();
    *config = serde_json::from_value(root)?;
    Ok(())
}

/// Writes `value` to the field at `path` in `~/.vct/config.toml`, preserving existing comments and other keys.
///
/// # Errors
///
/// Returns an error if `path` is empty, the config directory cannot be resolved, or writing fails.
pub fn save_setting(path: &[&str], value: &SettingValue) -> Result<()> {
    save_setting_in(&get_cache_dir()?, path, value)
}

/// Writes `value` to the field at `path` in `config.toml` under `dir`, preserving existing comments and other keys.
///
/// # Errors
///
/// Returns an error if `path` is empty or writing fails.
pub fn save_setting_in(dir: &Path, path: &[&str], value: &SettingValue) -> Result<()> {
    let Some((key, sections)) = path.split_last() else {
        anyhow::bail!("a setting needs a name");
    };
    edit_in(dir, |doc| {
        let mut table = doc.as_item_mut();
        for section in sections {
            table = section_mut(table, section);
        }
        let Some(table) = table.as_table_like_mut() else {
            return;
        };
        // Assigning through the existing entry keeps the comment above it.
        match table.get_mut(key) {
            Some(existing) => *existing = value.to_item(),
            None => {
                table.insert(key, value.to_item());
            }
        }
    })
}

/// Returns mutable access to section `name` under `parent`, creating it (matching `parent`'s inline/standard style) or replacing a non-table value.
fn section_mut<'a>(parent: &'a mut Item, name: &str) -> &'a mut Item {
    let inline = parent.is_inline_table();
    let table = parent
        .as_table_like_mut()
        .expect("the document root and every section reached here are tables");
    if !table.get(name).is_some_and(Item::is_table_like) {
        let fresh = if inline {
            Item::Value(toml_edit::Value::InlineTable(toml_edit::InlineTable::new()))
        } else {
            Item::Table(Table::new())
        };
        table.insert(name, fresh);
    }
    table.get_mut(name).expect("the section was just ensured")
}

/// Returns the schema description for the setting at `path`, if any.
pub fn setting_description(path: &[&str]) -> Option<String> {
    schema_description(cached_json_schema(), path)
}

/// Loads `config.toml` under `dir` (falling back to [`default_document`]), applies `mutate`, and writes it back atomically.
fn edit_in(dir: &Path, mutate: impl FnOnce(&mut DocumentMut)) -> Result<()> {
    let path = dir.join("config.toml");
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .unwrap_or_else(default_document);
    mutate(&mut doc);
    write_string_atomic(&path, &doc.to_string())
}

/// Returns the process-cached self-contained JSON schema for [`Config`] with all subschemas inlined.
fn cached_json_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let settings =
            schemars::generate::SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true);
        let schema = settings.into_generator().into_root_schema_for::<Config>();
        serde_json::to_value(&schema).expect("config schema serializes to JSON")
    })
}

/// Returns the self-contained JSON schema for [`Config`] with all subschemas inlined.
pub fn json_schema() -> Value {
    cached_json_schema().clone()
}

/// Returns the pretty-printed JSON schema for [`Config`] with a trailing newline.
pub fn schema_json() -> String {
    let mut s = serde_json::to_string_pretty(cached_json_schema()).expect("schema serializes");
    s.push('\n');
    s
}

/// Builds the commented default `config.toml` document with a leading `#:schema` directive.
fn default_document() -> DocumentMut {
    let mut doc: DocumentMut = toml_edit::ser::to_string(&Config::default())
        .expect("default config serializes to TOML")
        .parse()
        .expect("serialized default config is valid TOML");
    annotate_table(doc.as_table_mut(), cached_json_schema(), true);
    doc
}

/// Recursively attaches schema descriptions as leading `#` comments on `table`, prepending `#:schema` on the root.
fn annotate_table(table: &mut Table, schema: &Value, is_root: bool) {
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for (idx, key) in keys.iter().enumerate() {
        let field_schema = props.get(key);
        let desc = field_schema
            .and_then(|s| s.get("description"))
            .and_then(Value::as_str);
        let is_table = table
            .get(key)
            .is_some_and(|i| i.is_table() || i.is_inline_table());
        if is_table {
            let field_schema = field_schema.cloned();
            // Trim the serializer's trailing key space so table headers render as `[usage]` instead of `[usage ]`.
            if let Some(mut km) = table.key_mut(key) {
                km.leaf_decor_mut().set_suffix("");
            }
            if let Some(item) = table.get_mut(key)
                && let Some(sub) = as_standard_table(item)
            {
                let mut prefix = String::new();
                if is_root && idx == 0 {
                    prefix.push_str(&format!("#:schema {SCHEMA_URL}\n\n"));
                } else {
                    prefix.push('\n');
                }
                if let Some(d) = desc {
                    prefix.push_str(&comment_block(d));
                }
                sub.decor_mut().set_prefix(prefix);
                if let Some(fs) = field_schema {
                    annotate_table(sub, &fs, false);
                }
            }
        } else if let Some(d) = desc
            && let Some(mut km) = table.key_mut(key)
        {
            km.leaf_decor_mut()
                .set_prefix(format!("\n{}", comment_block(d)));
        }
    }
}

/// Converts an inline table in `item` to a standard `[header]` table in place so header comments can attach.
fn as_standard_table(item: &mut Item) -> Option<&mut Table> {
    if item.is_inline_table()
        && let Some(inline) = item.as_inline_table().cloned()
    {
        *item = Item::Table(inline.into_table());
    }
    item.as_table_mut()
}

/// Formats `desc` as `# `-prefixed comment lines, each terminated with a newline.
fn comment_block(desc: &str) -> String {
    desc.lines()
        .map(|line| {
            if line.is_empty() {
                "#\n".to_string()
            } else {
                format!("# {line}\n")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_template_parses_to_expected_defaults() {
        let text = default_document().to_string();
        let cfg: Config = toml_edit::de::from_str(&text).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(text.starts_with(&format!("#:schema {SCHEMA_URL}")));
        assert_eq!(cfg.general.default_time_range, TimeRange::All);
        assert_eq!(cfg.usage.quota.panels, default_quota_panels());
        assert!(cfg.usage.shows_quota_panel("cursor"));
        assert!(cfg.usage.merge_models);
        assert_eq!(cfg.usage.refresh_interval, 10);
        assert_eq!(cfg.usage.quota.refresh_interval, 60);
        assert_eq!(cfg.analysis.refresh_interval, 10);
        assert_eq!(cfg.performance.scan_threads, 0);
        assert_eq!(cfg.providers, ProvidersConfig::default());
        assert!(cfg.providers.cursor);
        assert!(cfg.providers.grok);
        assert_eq!(cfg.logging.level, LogLevel::Warn);
        assert_eq!(cfg.logging.retention_days, 7);
        assert!(cfg.remotes.hosts.is_empty());
        assert_eq!(cfg.remotes.command, "");
        assert_eq!(cfg.remotes.refresh_interval, 300);
    }

    #[test]
    fn a_file_is_valid_when_absent_or_parsing_and_not_when_a_key_has_the_wrong_type() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_is_valid_in(dir.path()));
        let path = dir.path().join("config.toml");
        std::fs::write(&path, default_document().to_string()).unwrap();
        assert!(file_is_valid_in(dir.path()));
        std::fs::write(&path, "[remotes]\nhosts = \"devbox\"\n").unwrap();
        assert!(!file_is_valid_in(dir.path()));
        std::fs::write(&path, "[remotes\n").unwrap();
        assert!(!file_is_valid_in(dir.path()));
    }

    #[test]
    fn remotes_hosts_parse_and_leave_the_other_keys_at_their_defaults() {
        let cfg = parse_config_text("[remotes]\nhosts = [\"devbox\", \"wei@laptop\"]\n");
        assert_eq!(cfg.remotes.hosts, ["devbox", "wei@laptop"]);
        assert_eq!(cfg.remotes.command, "");
        assert_eq!(cfg.remotes.refresh_interval, 300);
    }

    #[test]
    fn missing_sections_use_defaults() {
        // Empty files rely on hand-written `Default` impls to preserve opt-out defaults.
        let cfg: Config = toml_edit::de::from_str("").unwrap();
        assert_eq!(cfg.usage.quota.panels, default_quota_panels());
        assert!(cfg.providers.cursor);
        assert!(cfg.providers.grok);
        assert_eq!(cfg.usage.refresh_secs(), 10);
        assert_eq!(cfg.usage.quota_refresh_secs(), 60);
        // An absent `[logging]` section backfills its default so adding the section requires no migration.
        assert_eq!(cfg.logging, LoggingConfig::default());
        assert_eq!(cfg.performance, PerformanceConfig::default());
    }

    #[test]
    fn scan_threads_resolve_config_then_env_then_auto() {
        let auto = PerformanceConfig { scan_threads: 0 };
        assert_eq!(auto.resolved_scan_threads_with(1, None), 1);
        assert_eq!(auto.resolved_scan_threads_with(2, None), 2);
        assert_eq!(auto.resolved_scan_threads_with(4, None), 2);
        assert_eq!(auto.resolved_scan_threads_with(8, None), 4);
        assert_eq!(auto.resolved_scan_threads_with(16, None), 8);
        assert_eq!(auto.resolved_scan_threads_with(32, None), 8);
        assert_eq!(auto.resolved_scan_threads_with(16, Some("7")), 7);
        assert_eq!(auto.resolved_scan_threads_with(4, Some("99")), 4);
        assert_eq!(auto.resolved_scan_threads_with(16, Some("0")), 8);
        assert_eq!(auto.resolved_scan_threads_with(16, Some("invalid")), 8);

        let configured = PerformanceConfig { scan_threads: 12 };
        assert_eq!(configured.resolved_scan_threads_with(8, Some("3")), 8);
    }

    #[test]
    fn partial_usage_section_keeps_quota_default() {
        let cfg: Config = toml_edit::de::from_str("[usage]\nmerge_models = false\n").unwrap();
        assert!(!cfg.usage.merge_models);
        assert_eq!(cfg.usage.quota.panels, default_quota_panels());
        assert_eq!(cfg.usage.quota_refresh_secs(), 60);
    }

    #[test]
    fn quota_panels_can_be_narrowed_or_emptied() {
        let cfg: Config =
            toml_edit::de::from_str("[usage.quota]\npanels = [\"claude\"]\n").unwrap();
        assert!(cfg.usage.shows_quota_panel("claude"));
        assert!(!cfg.usage.shows_quota_panel("cursor"));

        let empty: Config = toml_edit::de::from_str("[usage.quota]\npanels = []\n").unwrap();
        assert!(!empty.usage.shows_quota_panel("claude"));
    }

    #[test]
    fn refresh_interval_reads_legacy_secs() {
        let text = "[usage]\nrefresh_interval_secs = 5\n[analysis]\nrefresh_interval_secs = 7\n";
        let mut cfg: Config = toml_edit::de::from_str(text).unwrap();
        migrate_legacy(&mut cfg, text);
        assert_eq!(cfg.usage.refresh_secs(), 5);
        assert_eq!(cfg.analysis.refresh_secs(), 7);
    }

    #[test]
    fn coexisting_refresh_keys_preserve_the_rest_of_the_config() {
        // Coexisting legacy and new keys must not trigger a serde duplicate-field reset; the new key wins and other sections survive.
        let text = "[general]\ndefault_time_range = \"weekly\"\n[usage]\nrefresh_interval = 5\nrefresh_interval_secs = 10\n[providers]\ncursor = false\n";
        let mut cfg: Config = toml_edit::de::from_str(text).unwrap();
        migrate_legacy(&mut cfg, text);
        assert_eq!(cfg.usage.refresh_secs(), 5);
        assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
        assert!(!cfg.providers.cursor);
    }

    #[test]
    fn migrates_legacy_quota_panels() {
        let text = "[usage]\nquota_panels = [\"claude\"]\n";
        let mut cfg: Config = toml_edit::de::from_str(text).unwrap();
        migrate_legacy(&mut cfg, text);
        assert_eq!(cfg.usage.quota.panels, vec!["claude".to_string()]);
        assert!(!cfg.usage.shows_quota_panel("cursor"));
    }

    #[test]
    fn new_quota_panels_win_over_legacy() {
        let text = "[usage]\nquota_panels = [\"claude\"]\n[usage.quota]\npanels = []\n";
        let mut cfg: Config = toml_edit::de::from_str(text).unwrap();
        migrate_legacy(&mut cfg, text);
        assert!(cfg.usage.quota.panels.is_empty());
    }

    #[test]
    fn migrate_document_upgrades_a_full_legacy_file() {
        let legacy = "# old header\n\n[usage]\nmerge_models = false\nquota_panels = [\"claude\", \"codex\"]\nrefresh_interval_secs = 15\n\n[analysis]\nrefresh_interval_secs = 20\n";
        let migrated = migrate_text(legacy).unwrap().expect("legacy file changes");
        assert!(migrated.starts_with("#:schema "));
        assert!(!migrated.contains("quota_panels"));
        assert!(!migrated.contains("refresh_interval_secs"));
        assert!(migrated.contains("[usage.quota]"));
        assert!(migrated.contains("panels = [\"claude\", \"codex\", \"grok\", \"antigravity\"]"));
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(cfg.usage.refresh_interval, 15);
        assert_eq!(cfg.analysis.refresh_interval, 20);
        assert_eq!(
            cfg.usage.quota.panels,
            vec![
                "claude".to_string(),
                "codex".to_string(),
                "grok".to_string(),
                "antigravity".to_string()
            ]
        );
        assert_eq!(cfg.usage.quota.refresh_interval, 60);
        assert_eq!(cfg.general.version, CONFIG_VERSION);
    }

    #[test]
    fn migrate_document_is_idempotent() {
        let legacy = "[usage]\nquota_panels = [\"claude\"]\nrefresh_interval_secs = 15\n";
        let once = migrate_text(legacy).unwrap().expect("first pass changes");
        assert!(migrate_text(&once).unwrap().is_none());
    }

    #[test]
    fn migrate_document_noops_on_the_generated_default() {
        let current = default_document().to_string();
        assert!(migrate_text(&current).unwrap().is_none());
    }

    #[test]
    fn migrate_document_adds_only_the_schema_directive_when_keys_are_current() {
        let text = "[general]\nversion = 4\n\n[usage]\nrefresh_interval = 10\n\n[usage.quota]\npanels = [\"claude\"]\n";
        let migrated = migrate_text(text).unwrap().expect("adds #:schema");
        assert!(migrated.starts_with("#:schema "));
        assert!(migrated.contains("refresh_interval = 10"));
        assert!(migrated.contains("panels = [\"claude\"]"));
    }

    #[test]
    fn migrate_document_drops_stale_refresh_key_when_both_present() {
        let text = "#:schema x\n[usage]\nrefresh_interval = 5\nrefresh_interval_secs = 10\n";
        let migrated = migrate_text(text).unwrap().expect("drops the legacy key");
        assert!(!migrated.contains("refresh_interval_secs"));
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(cfg.usage.refresh_interval, 5);
    }

    #[test]
    fn migrate_document_leaves_an_inline_usage_table_to_the_read_shim() {
        let text = "usage = { quota_panels = [\"claude\"], refresh_interval_secs = 30 }\n";
        let migrated = migrate_text(text).unwrap().expect("adds #:schema");
        assert!(migrated.starts_with("#:schema "));
        assert!(migrated.contains("quota_panels"));
        assert!(migrate_text(&migrated).unwrap().is_none());
    }

    #[test]
    fn migrate_document_leaves_an_inline_general_table_unmodified() {
        let text = "#:schema x\ngeneral = { default_time_range = \"weekly\", version = 2 }\n";
        assert!(migrate_text(text).unwrap().is_none());
        let cfg = parse_config_text(text);
        assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
        assert!(cfg.general.auto_update);
    }

    #[test]
    fn migrate_text_errors_on_malformed_toml() {
        assert!(migrate_text("= not valid").is_err());
    }

    #[test]
    fn migrate_document_drops_a_malformed_legacy_refresh_value() {
        // Malformed legacy values are dropped rather than promoted so serde does not reject the file and reset other settings.
        let text = "[general]\ndefault_time_range = \"weekly\"\n[usage]\nrefresh_interval_secs = -5\n[providers]\ncursor = false\n";
        let migrated = migrate_text(text).unwrap().expect("legacy file changes");
        assert!(!migrated.contains("refresh_interval = -5"));
        assert!(!migrated.contains("refresh_interval_secs"));
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
        assert!(!cfg.providers.cursor);
        assert_eq!(cfg.usage.refresh_interval, 10);
        assert!(migrate_text(&migrated).unwrap().is_none());
    }

    #[test]
    fn migrate_document_preserves_a_hand_added_comment_on_a_renamed_key() {
        let text = "[usage]\n# keep me\nrefresh_interval_secs = 15\n";
        let migrated = migrate_text(text).unwrap().expect("legacy file changes");
        assert!(migrated.contains("# keep me"));
        assert!(migrated.contains("refresh_interval = 15"));
    }

    #[test]
    fn migrate_document_adds_the_directive_despite_a_buried_schema_line() {
        let text = "[usage]\nquota_panels = [\"claude\"]\nnote = \"\"\"\n#:schema fake\n\"\"\"\n";
        let migrated = migrate_text(text)
            .unwrap()
            .expect("adds the real directive");
        assert!(migrated.starts_with("#:schema https://"));
    }

    #[test]
    fn migrate_document_merges_panels_into_an_existing_quota_table() {
        let text = "[usage]\nquota_panels = [\"claude\"]\n\n[usage.quota]\nrefresh_interval = 30\n";
        let migrated = migrate_text(text).unwrap().expect("legacy file changes");
        assert!(!migrated.contains("quota_panels"));
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(
            cfg.usage.quota.panels,
            vec![
                "claude".to_string(),
                "grok".to_string(),
                "antigravity".to_string()
            ]
        );
        assert_eq!(cfg.usage.quota.refresh_interval, 30);
        assert!(migrate_text(&migrated).unwrap().is_none());
    }

    #[test]
    fn migrate_document_drops_legacy_quota_panels_when_new_panels_present() {
        let text = "#:schema x\n[usage]\nquota_panels = [\"gemini\"]\n\n[usage.quota]\npanels = [\"claude\"]\n";
        let migrated = migrate_text(text).unwrap().expect("drops the legacy key");
        assert!(!migrated.contains("quota_panels"));
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(
            cfg.usage.quota.panels,
            vec![
                "claude".to_string(),
                "grok".to_string(),
                "antigravity".to_string()
            ]
        );
    }

    #[test]
    fn migrate_document_skips_an_inline_quota_child() {
        // Leave inline `quota` untouched without stamping `version` so the file is not marked upgraded.
        let text =
            "#:schema x\n[usage]\nquota_panels = [\"claude\"]\nquota = { panels = [\"codex\"] }\n";
        assert!(migrate_text(text).unwrap().is_none());
        assert!(panels_out_of_reach(&text.parse::<DocumentMut>().unwrap()));
    }

    /// A panel shipped after the file was written is added once, and removing it afterwards sticks.
    #[test]
    fn backfilled_panel_is_added_once_and_stays_removed() {
        let text = "#:schema x\n[general]\ndefault_time_range = \"all\"\n\n[usage.quota]\npanels = [\"claude\", \"codex\"]\n";
        let migrated = migrate_text(text)
            .unwrap()
            .expect("version 1 file upgrades");
        assert!(migrated.contains("panels = [\"claude\", \"codex\", \"grok\", \"antigravity\"]"));
        assert!(migrated.contains("version = 4"));
        assert!(migrate_text(&migrated).unwrap().is_none());

        let pruned = migrated.replace(
            "[\"claude\", \"codex\", \"grok\", \"antigravity\"]",
            "[\"claude\", \"codex\"]",
        );
        assert!(migrate_text(&pruned).unwrap().is_none());
        let cfg: Config = toml_edit::de::from_str(&pruned).unwrap();
        assert!(!cfg.usage.shows_quota_panel("grok"));
        assert!(!cfg.usage.shows_quota_panel("antigravity"));
    }

    /// A file stamped by the version that shipped `grok` gains only panels added by later versions.
    #[test]
    fn a_version_3_file_gains_only_antigravity() {
        let text = "#:schema x\n[general]\nauto_update = true\nversion = 3\n\n[usage.quota]\npanels = [\"claude\"]\n";
        let migrated = migrate_text(text)
            .unwrap()
            .expect("version 3 file upgrades");
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(
            cfg.usage.quota.panels,
            vec!["claude".to_string(), "antigravity".to_string()]
        );
        assert_eq!(cfg.general.version, CONFIG_VERSION);
    }

    /// Panels dropped before this upgrade are not resurrected; only panels added by later versions are back-filled.
    #[test]
    fn backfill_leaves_previously_removed_panels_alone() {
        let text = "#:schema x\n[usage.quota]\npanels = [\"claude\"]\n";
        let migrated = migrate_text(text)
            .unwrap()
            .expect("version 1 file upgrades");
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(
            cfg.usage.quota.panels,
            vec![
                "claude".to_string(),
                "grok".to_string(),
                "antigravity".to_string()
            ],
            "codex / copilot / cursor were removed on purpose and stay removed"
        );
    }

    #[test]
    fn backfill_never_reopens_a_band_turned_off() {
        let text = "#:schema x\n[usage.quota]\npanels = []\n";
        let migrated = migrate_text(text).unwrap().expect("the marker is stamped");
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert!(cfg.usage.quota.panels.is_empty());
        assert_eq!(cfg.general.version, CONFIG_VERSION);
    }

    #[test]
    fn backfill_stamps_a_file_with_no_general_section() {
        let text = "#:schema x\n[providers]\ncursor = false\n";
        let migrated = migrate_text(text).unwrap().expect("the marker is stamped");
        let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
        assert_eq!(cfg.general.version, CONFIG_VERSION);
        assert!(!cfg.providers.cursor);
        assert!(cfg.usage.shows_quota_panel("grok"));
        assert!(migrate_text(&migrated).unwrap().is_none());
    }

    #[test]
    fn an_empty_file_reaches_the_current_layout_in_one_pass() {
        // `backfill_new_quota_panels` creates `[general]` on empty files, so `#:schema` must attach afterward to finish in one pass.
        for text in ["", "# just a comment\n"] {
            let migrated = migrate_text(text)
                .unwrap()
                .expect("an empty file is stamped");
            assert!(migrated.starts_with("#:schema "));
            assert!(migrated.contains("version = 4"));
            assert!(
                migrate_text(&migrated).unwrap().is_none(),
                "one pass must be enough"
            );
        }
    }

    #[test]
    fn a_panel_list_that_could_not_be_moved_is_not_marked_upgraded() {
        // Leaving `version` unstamped prevents retiring the one-shot upgrade while `quota_panels` remains stuck beside an inline `quota`.
        let text =
            "#:schema x\n[usage]\nquota_panels = [\"claude\"]\nquota = { refresh_interval = 30 }\n";
        assert!(migrate_text(text).unwrap().is_none());
        assert!(panels_out_of_reach(&text.parse::<DocumentMut>().unwrap()));
    }

    #[test]
    fn a_hand_edited_version_value_never_resets_the_config() {
        // Non-integer markers must read as legacy rather than failing deserialization and triggering a full reset.
        for bad in ["\"2\"", "2.5", "true", "-1"] {
            let text = format!(
                "[general]\nversion = {bad}\ndefault_time_range = \"weekly\"\n[providers]\ncursor = false\n"
            );
            let cfg: Config = toml_edit::de::from_str(&text).expect("document still parses");
            assert_eq!(cfg.general.version, legacy_config_version(), "bad = {bad}");
            assert_eq!(cfg.general.default_time_range, TimeRange::Weekly);
            assert!(!cfg.providers.cursor);
            let migrated = migrate_text(&text)
                .unwrap()
                .expect("the marker is repaired");
            let cfg: Config = toml_edit::de::from_str(&migrated).unwrap();
            assert_eq!(cfg.general.version, CONFIG_VERSION);
        }
    }

    #[test]
    fn every_backfilled_panel_ships_by_the_current_version() {
        for (version, name) in PANELS_ADDED_BY_VERSION {
            assert!(
                *version <= CONFIG_VERSION,
                "{name} is scheduled for a version this build never stamps"
            );
            assert!(
                default_quota_panels().iter().any(|p| p == name),
                "{name} is back-filled but is not a current default panel"
            );
        }
    }

    #[test]
    fn refresh_secs_clamps_zero_to_one() {
        let cfg = UsageConfig {
            refresh_interval: 0,
            quota: QuotaConfig {
                refresh_interval: 0,
                ..QuotaConfig::default()
            },
            ..UsageConfig::default()
        };
        assert_eq!(cfg.refresh_secs(), 1);
        assert_eq!(cfg.quota_refresh_secs(), 1);
    }

    #[test]
    fn providers_default_all_enabled() {
        let p = ProvidersConfig::default();
        assert!(
            p.claude
                && p.codex
                && p.copilot
                && p.gemini
                && p.opencode
                && p.cursor
                && p.hermes
                && p.grok
        );
    }

    #[test]
    fn committed_schema_matches_generated() {
        // Guards `vct.schema.json` against drift from the typed Config.
        // Regenerate with: `vct config schema > vct.schema.json`.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../vct.schema.json");
        let committed: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("vct.schema.json exists"))
                .expect("vct.schema.json is valid JSON");
        assert_eq!(committed, json_schema());
    }
}
