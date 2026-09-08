//! Shared HTTP helpers for the quota fetchers.
//!
//! One blocking client is built without a default `User-Agent` and shared
//! across the provider workers; each request sets its own UA. The version those
//! User-Agents carry comes from [`detect_cli_version`], which probes the
//! installed provider CLIs only once the binary opts in. Also holds the
//! ISO-timestamp conversion used by the fetchers.

use crate::utils::get_version_cache_path_in;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Version of the on-disk shape below. A file carrying any other version is
/// ignored and the CLI re-probed, the same fail-safe the quota snapshot cache
/// applies to its own files.
const SCHEMA_VERSION: u32 = 1;

/// One provider's `~/.vct/version/<provider>.json`: the installed CLI's version
/// and when it was last probed.
#[derive(Serialize, Deserialize)]
struct CliVersionCache {
    schema_version: u32,
    provider: String,
    version: String,
    last_checked_at: String,
}

/// Whether this process may probe the installed provider CLIs for their
/// versions. Off until [`enable_cli_version_detection`] turns it on.
static CLI_VERSION_DETECTION: AtomicBool = AtomicBool::new(false);

/// Lets [`detect_cli_version`] probe the installed CLIs for this process.
///
/// Probing spawns `<bin> --version` and caches the answer under `~/.vct` —
/// side effects that belong to the application rather than to a library call,
/// so `vct-core` keeps them off until the binary opts in (once, before
/// dispatch). Any other embedder — including the test suite — gets each
/// provider's fallback version without a subprocess or a home directory write.
pub fn enable_cli_version_detection() {
    CLI_VERSION_DETECTION.store(true, Ordering::Relaxed);
}

/// Whether CLI version probing has been enabled for this process.
fn cli_version_detection_enabled() -> bool {
    CLI_VERSION_DETECTION.load(Ordering::Relaxed)
}

/// Detects an installed CLI's version by running `<bin> --version`, caching the
/// result under `~/.vct/version/<provider>.json` for the day so it is not re-run
/// on every launch. Falls back to `fallback` when the CLI is absent or
/// unreadable, so the User-Agent it feeds is always a plausible client version —
/// and returns that same fallback outright until
/// [`enable_cli_version_detection`] is called.
pub fn detect_cli_version(bin: &str, provider: &str, fallback: &str) -> String {
    if !cli_version_detection_enabled() {
        return fallback.to_string();
    }
    if let Some(v) = read_cached_version(provider) {
        return v;
    }
    if let Some(v) = run_cli_version(bin) {
        let _ = write_cached_version(provider, &v);
        return v;
    }
    fallback.to_string()
}

/// Extracts the first version-shaped token (`2.1.201`, `0.142.5`) from a CLI's
/// `--version` output, tolerating a leading program name (`codex-cli 0.142.5`)
/// or a trailing label (`2.1.201 (Claude Code)`).
pub fn parse_version(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
}

/// Reads a provider's version cache, returning the version only if it was
/// stamped earlier on the current UTC day.
fn read_cached_version(provider: &str) -> Option<String> {
    read_cached_version_in(&crate::utils::get_cache_dir().ok()?, provider)
}

/// The injectable core of [`read_cached_version`]: reads from an explicit cache
/// directory. Production passes `~/.vct`; tests pass a temp dir.
fn read_cached_version_in(dir: &Path, provider: &str) -> Option<String> {
    let text = std::fs::read_to_string(get_version_cache_path_in(dir, provider)).ok()?;
    let cached: CliVersionCache = serde_json::from_str(&text).ok()?;
    if cached.schema_version != SCHEMA_VERSION || cached.provider != provider {
        return None;
    }
    is_today_utc(&cached.last_checked_at).then_some(cached.version)
}

/// Persists `version` stamped with the current UTC time (best-effort, atomic).
fn write_cached_version(provider: &str, version: &str) -> Result<()> {
    write_cached_version_in(&crate::utils::get_cache_dir()?, provider, version)
}

/// The injectable core of [`write_cached_version`], writing into an explicit
/// cache directory.
fn write_cached_version_in(dir: &Path, provider: &str, version: &str) -> Result<()> {
    crate::utils::write_json_atomic(
        get_version_cache_path_in(dir, provider),
        &CliVersionCache {
            schema_version: SCHEMA_VERSION,
            provider: provider.to_string(),
            version: version.to_string(),
            last_checked_at: crate::utils::now_rfc3339_utc_nanos(),
        },
    )
}

/// Runs `<bin> --version` and parses the version token from stdout.
fn run_cli_version(bin: &str) -> Option<String> {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

/// Whether `ts` (an RFC3339 timestamp) falls on the current UTC calendar day.
///
/// The version cache stores a full RFC3339 nanosecond stamp but is only
/// refreshed once per day, so staleness is decided on the UTC date alone. An
/// unparseable stamp reads as stale so the version is re-detected.
fn is_today_utc(ts: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&chrono::Utc).date_naive() == chrono::Utc::now().date_naive())
        .unwrap_or(false)
}

/// Builds the shared blocking HTTP client (8s timeout, no default UA).
///
/// The UA is intentionally left unset so each request can supply its own via a
/// header; setting it on both the client and the request would send a duplicate
/// `User-Agent`.
///
/// # Errors
///
/// Returns an error if the client cannot be constructed.
pub fn build_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .context("Failed to build HTTP client")
}

/// Parses an ISO-8601 timestamp into Unix **seconds**, or `None` on failure.
///
/// [`crate::utils::parse_iso_timestamp`] returns Unix *milliseconds* and `0` on
/// failure; this divides by 1000 and maps `0` to `None` so a bad timestamp
/// renders as "no reset" rather than the epoch.
pub fn iso_to_unix_secs(s: &str) -> Option<i64> {
    let ms = crate::utils::parse_iso_timestamp(s);
    (ms > 0).then_some(ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_to_unix_secs_handles_bad_input() {
        assert_eq!(iso_to_unix_secs("not-a-date"), None);
        assert!(iso_to_unix_secs("2026-07-03T17:09:59.651608+00:00").unwrap() > 0);
    }

    #[test]
    fn is_today_utc_matches_now_but_not_a_past_day() {
        let now = crate::utils::now_rfc3339_utc_nanos();
        assert!(is_today_utc(&now));
        assert!(!is_today_utc("2000-01-01T00:00:00Z"));
        assert!(!is_today_utc("not-a-timestamp"));
    }

    /// Nothing in the suite may enable probing: it would spawn a provider CLI
    /// and write into the developer's real `~/.vct`.
    #[test]
    fn cli_version_detection_stays_off_outside_the_binary() {
        assert!(!cli_version_detection_enabled());
        assert_eq!(detect_cli_version("cargo", "cargo", "1.2.3"), "1.2.3");
    }

    #[test]
    fn cached_version_round_trips_and_goes_stale_the_next_utc_day() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_cached_version_in(dir.path(), "grok"), None);

        write_cached_version_in(dir.path(), "grok", "0.2.112").unwrap();
        assert_eq!(
            read_cached_version_in(dir.path(), "grok").as_deref(),
            Some("0.2.112")
        );

        // The seed carries this build's own schema and provider, so the stale
        // date is the only thing left that can reject it.
        std::fs::write(
            get_version_cache_path_in(dir.path(), "grok"),
            serde_json::to_string(&CliVersionCache {
                schema_version: SCHEMA_VERSION,
                provider: "grok".into(),
                version: "0.1.0".into(),
                last_checked_at: "2000-01-01T00:00:00Z".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(read_cached_version_in(dir.path(), "grok"), None);
    }

    /// The record lands under `version/<provider>.json`, and one written by a
    /// different schema or naming another provider is ignored rather than served.
    #[test]
    fn version_cache_is_scoped_by_path_schema_and_provider() {
        let dir = tempfile::tempdir().unwrap();
        write_cached_version_in(dir.path(), "cursor", "2026.08.04").unwrap();

        let path = dir.path().join("version").join("cursor.json");
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(body["schema_version"], serde_json::json!(SCHEMA_VERSION));
        assert_eq!(body["provider"], "cursor");
        assert_eq!(body["version"], "2026.08.04");

        let today = crate::utils::now_rfc3339_utc_nanos();
        for (schema, provider) in [(SCHEMA_VERSION + 1, "cursor"), (SCHEMA_VERSION, "claude")] {
            std::fs::write(
                &path,
                serde_json::to_string(&CliVersionCache {
                    schema_version: schema,
                    provider: provider.into(),
                    version: "9.9.9".into(),
                    last_checked_at: today.clone(),
                })
                .unwrap(),
            )
            .unwrap();
            assert_eq!(read_cached_version_in(dir.path(), "cursor"), None);
        }
    }

    #[test]
    fn parse_version_handles_leading_or_trailing_labels() {
        // Claude: version first, label after.
        assert_eq!(
            parse_version("2.1.201 (Claude Code)").as_deref(),
            Some("2.1.201")
        );
        // Codex: program name first, version after.
        assert_eq!(
            parse_version("codex-cli 0.142.5").as_deref(),
            Some("0.142.5")
        );
        assert_eq!(parse_version("  2.0.14\n").as_deref(), Some("2.0.14"));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("Claude Code"), None);
    }
}
