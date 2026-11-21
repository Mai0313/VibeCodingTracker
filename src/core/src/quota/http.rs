//! Shared HTTP client construction, opt-in CLI version detection, and ISO-8601 timestamp helpers for quota fetchers.
//!
//! The shared blocking client omits a default `User-Agent` so each request sets its own. [`detect_cli_version`] returns each caller's fallback until [`enable_cli_version_detection`] is called.

use crate::utils::get_version_cache_path_in;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// On-disk schema version for [`CliVersionCache`]; mismatched files are ignored and re-probed.
const SCHEMA_VERSION: u32 = 1;
/// Upper bound on `<bin> --version` wall-clock runtime before terminating the child.
const CLI_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum stdout bytes read from `<bin> --version`.
const MAX_CLI_VERSION_BYTES: u64 = 4096;

/// On-disk record at `~/.vct/version/<provider>.json` caching a probed CLI version and its probe timestamp.
#[derive(Serialize, Deserialize)]
struct CliVersionCache {
    schema_version: u32,
    provider: String,
    /// Detected version string, or empty when the probe failed (negative cache entry).
    version: String,
    last_checked_at: String,
}

/// Same-day version cache lookup result.
#[derive(Debug, PartialEq, Eq)]
enum CachedVersion {
    Hit(String),
    Miss,
}

/// Process-wide gate enabling CLI `--version` probing via [`enable_cli_version_detection`].
static CLI_VERSION_DETECTION: AtomicBool = AtomicBool::new(false);

/// Enables CLI version probing for [`detect_cli_version`] in this process.
///
/// Probing spawns `<bin> --version` and writes `~/.vct/version/<provider>.json`. Library callers that do not opt in always receive the fallback version without subprocesses or filesystem writes.
pub fn enable_cli_version_detection() {
    CLI_VERSION_DETECTION.store(true, Ordering::Relaxed);
}

/// Whether CLI version probing has been enabled for this process.
fn cli_version_detection_enabled() -> bool {
    CLI_VERSION_DETECTION.load(Ordering::Relaxed)
}

/// Returns the installed version of `bin` for `provider`, or `fallback` if unavailable or probing is disabled.
///
/// Runs `<bin> --version` at most once per UTC day and caches both successes and misses under `~/.vct/version/<provider>.json`. Always returns `fallback` until [`enable_cli_version_detection`] is called.
pub fn detect_cli_version(bin: &str, provider: &str, fallback: &str) -> String {
    if !cli_version_detection_enabled() {
        return fallback.to_string();
    }
    if let Some(cached) = read_cached_version_entry(provider) {
        return match cached {
            CachedVersion::Hit(v) => v,
            CachedVersion::Miss => fallback.to_string(),
        };
    }
    if let Some(v) = run_cli_version(bin) {
        let _ = write_cached_version(provider, &v);
        return v;
    }
    let _ = write_cached_version(provider, "");
    fallback.to_string()
}

/// Returns the first whitespace-delimited token starting with an ASCII digit and containing `.` (for example `2.1.201` from `2.1.201 (Claude Code)` or `codex-cli 0.142.5`).
pub fn parse_version(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
}

/// Returns `provider`'s cached CLI version entry if stamped on the current UTC day.
fn read_cached_version_entry(provider: &str) -> Option<CachedVersion> {
    read_cached_version_entry_in(&crate::utils::get_cache_dir().ok()?, provider)
}

/// Injectable core of [`read_cached_version_entry`] reading from cache root `dir`.
fn read_cached_version_entry_in(dir: &Path, provider: &str) -> Option<CachedVersion> {
    let text = std::fs::read_to_string(get_version_cache_path_in(dir, provider)).ok()?;
    let cached: CliVersionCache = serde_json::from_str(&text).ok()?;
    if cached.schema_version != SCHEMA_VERSION || cached.provider != provider {
        return None;
    }
    if !is_today_utc(&cached.last_checked_at) {
        return None;
    }
    if cached.version.is_empty() {
        Some(CachedVersion::Miss)
    } else {
        Some(CachedVersion::Hit(cached.version))
    }
}

/// Returns `provider`'s cached positive CLI version from cache root `dir`, if stamped today.
#[cfg(test)]
fn read_cached_version_in(dir: &Path, provider: &str) -> Option<String> {
    match read_cached_version_entry_in(dir, provider) {
        Some(CachedVersion::Hit(v)) => Some(v),
        _ => None,
    }
}

/// Persists `version` stamped with the current UTC time (best-effort, atomic).
fn write_cached_version(provider: &str, version: &str) -> Result<()> {
    write_cached_version_in(&crate::utils::get_cache_dir()?, provider, version)
}

/// Injectable core of [`write_cached_version`] writing into cache root `dir`.
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

/// Runs `<bin> --version` with a bounded timeout and parses the version token from stdout.
fn run_cli_version(bin: &str) -> Option<String> {
    run_cli_version_with_timeout(bin, &["--version"], CLI_VERSION_TIMEOUT)
}

/// Runs `bin` with `args`, enforcing `timeout` and a bounded stdout read, and parses the version token.
fn run_cli_version_with_timeout(bin: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut command = std::process::Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.take(MAX_CLI_VERSION_BYTES).read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let bytes = reader.join().ok()?;
                return parse_version(&String::from_utf8_lossy(&bytes));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Returns whether RFC3339 timestamp `ts` falls on the current UTC calendar day, or `false` if unparseable.
fn is_today_utc(ts: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&chrono::Utc).date_naive() == chrono::Utc::now().date_naive())
        .unwrap_or(false)
}

/// Builds the shared blocking HTTP client with an 8-second timeout and no default `User-Agent`.
///
/// Each request sets its own `User-Agent` header; leaving the client default unset prevents duplicate `User-Agent` headers.
///
/// # Errors
///
/// Returns an error if the underlying `reqwest` client cannot be constructed.
pub fn build_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .context("Failed to build HTTP client")
}

/// Parses an ISO-8601 timestamp into Unix seconds, returning `None` on parse failure or non-positive timestamps.
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

    /// Keeps probing disabled under test so no provider CLI is spawned and `~/.vct` is untouched.
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

        // Same schema and provider so only the stale date triggers rejection.
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

    /// Verifies the file path and that mismatched schema versions or provider names are ignored.
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

    #[test]
    fn negative_version_cache_round_trips_and_expires_next_day() {
        let dir = tempfile::tempdir().unwrap();
        write_cached_version_in(dir.path(), "codex", "").unwrap();
        assert_eq!(
            read_cached_version_entry_in(dir.path(), "codex"),
            Some(CachedVersion::Miss)
        );
        assert_eq!(read_cached_version_in(dir.path(), "codex"), None);

        std::fs::write(
            get_version_cache_path_in(dir.path(), "codex"),
            serde_json::to_string(&CliVersionCache {
                schema_version: SCHEMA_VERSION,
                provider: "codex".into(),
                version: String::new(),
                last_checked_at: "2000-01-01T00:00:00Z".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(read_cached_version_entry_in(dir.path(), "codex"), None);
    }

    #[cfg(unix)]
    #[test]
    fn cli_version_probe_kills_hung_process_on_timeout() {
        let start = Instant::now();
        let res =
            run_cli_version_with_timeout("sh", &["-c", "sleep 10"], Duration::from_millis(120));
        assert_eq!(res, None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "hung probe should be terminated quickly, took {:?}",
            start.elapsed()
        );
    }
}
