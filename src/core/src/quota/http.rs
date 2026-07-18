//! Shared HTTP helpers for the quota fetchers.
//!
//! One blocking client is built without a default `User-Agent` and shared
//! across the provider workers; each request sets its own UA. Also holds the
//! ISO-timestamp conversion used by the fetchers.

use anyhow::{Context, Result};

/// Detects an installed CLI's version by running `<bin> --version`, caching the
/// result under `~/.vct/<cache_file>` for the day so it is not re-run on every
/// launch. Falls back to `fallback` when the CLI is absent or unreadable, so the
/// User-Agent it feeds is always a plausible client version.
pub fn detect_cli_version(bin: &str, cache_file: &str, fallback: &str) -> String {
    if let Some(v) = read_cached_version(cache_file) {
        return v;
    }
    if let Some(v) = run_cli_version(bin) {
        let _ = write_cached_version(cache_file, &v);
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

/// Reads the `{version, last_checked_at}` cache, returning the version only if
/// it was stamped earlier on the current UTC day.
fn read_cached_version(cache_file: &str) -> Option<String> {
    let path = crate::utils::get_cache_dir().ok()?.join(cache_file);
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if !is_today_utc(v.get("last_checked_at")?.as_str()?) {
        return None;
    }
    v.get("version")?.as_str().map(str::to_string)
}

/// Persists `version` stamped with the current UTC time (best-effort, atomic).
fn write_cached_version(cache_file: &str, version: &str) -> Result<()> {
    let path = crate::utils::get_cache_dir()?.join(cache_file);
    crate::utils::write_json_atomic(
        path,
        &serde_json::json!({
            "version": version,
            "last_checked_at": crate::utils::now_rfc3339_utc_nanos(),
        }),
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
