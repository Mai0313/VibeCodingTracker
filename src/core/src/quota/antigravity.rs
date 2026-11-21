//! Antigravity quota fetcher.
//!
//! Resolves quota over pure HTTP with no `agy` subprocess polling, walking a four-tier chain:
//!
//! 1. `ANTIGRAVITY_LS_ADDRESS` (with optional `ANTIGRAVITY_CSRF_TOKEN` or `GET /` `window.__APP_CONFIG__` CSRF extraction).
//! 2. Local daemon discovery files (`$ANTIGRAVITY_APP_DATA_DIR/daemon` and `~/.gemini/*/daemon/{ls_,hub_}*.json`), newest first, skipping dead PIDs.
//! 3. On Linux, `/proc` discovery of own-user `*-hub-server` / `agy --hub` processes and their listening loopback ports.
//! 4. Direct Cloud Code HTTP (`v1internal:retrieveUserQuotaSummary`) using local OAuth credentials (OS credential store or `~/.gemini/antigravity-cli/antigravity-oauth-token`), read-only with no OAuth refresh or embedded client secret.

use crate::models::{
    AntigravityBucket, AntigravityLsQuotaResponse, AntigravityQuotaSnapshot,
    AntigravityQuotaSummary, QuotaSource, QuotaWindow,
};
use crate::quota::cache::load_antigravity_cache_from;
use crate::quota::http::{detect_cli_version, iso_to_unix_secs};
use crate::quota::provider::{QuotaOutcome, QuotaSnapshot};
use crate::quota::refresh::file_mtime;
use crate::utils::{get_antigravity_token_path, resolve_paths};
use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::blocking::Client;
use serde::Deserialize;
#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Login hint shown when Antigravity OAuth credentials are missing, expired, or rejected.
pub const ANTIGRAVITY_LOGIN_HINT: &str = "run: agy models";

/// Connect-RPC path for querying quota from a local Antigravity Language Server.
const LS_QUOTA_PATH: &str =
    "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary";

/// Default Cloud Code API base URL for personal (`consumer`) accounts.
const CCPA_CONSUMER_BASE: &str = "https://daily-cloudcode-pa.googleapis.com";

/// Default Cloud Code API base URL for Google Cloud (`gcp`) accounts.
const CCPA_GCP_BASE: &str = "https://cloudcode-pa.googleapis.com";

/// Fallback Antigravity CLI version for `User-Agent` when `agy --version` cannot be resolved.
const ANTIGRAVITY_FALLBACK_VERSION: &str = "1.5.5";

/// Short connect timeout for local Language Server HTTP calls.
const LS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Short request timeout for `GET /` CSRF discovery probes.
const LS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Request timeout for `RetrieveUserQuotaSummary` RPC calls.
const LS_RPC_TIMEOUT: Duration = Duration::from_secs(8);

/// Margin in seconds before expiry when an access token is already treated as expired.
const TOKEN_EXPIRY_LEEWAY_SECS: i64 = 60;

/// Cached local hub endpoint discovered from `/proc`.
#[derive(Clone, PartialEq, Eq)]
struct CachedHub {
    pid: u32,
    base_url: String,
    csrf_token: String,
}

impl fmt::Debug for CachedHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedHub")
            .field("pid", &self.pid)
            .field("base_url", &self.base_url)
            .field("csrf_token", &"<redacted>")
            .finish()
    }
}

/// Parsed daemon discovery file (`ls_*.json` / `hub_*.json`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonDiscoveryFile {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    http_port: Option<u16>,
    #[serde(default)]
    #[allow(dead_code)]
    https_port: Option<u16>,
    #[serde(default)]
    csrf_token: Option<String>,
}

/// Validated local Language Server endpoint parsed from a discovery file.
#[derive(Clone, PartialEq, Eq)]
struct DaemonEndpoint {
    pid: u32,
    http_port: u16,
    csrf_token: String,
}

impl fmt::Debug for DaemonEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonEndpoint")
            .field("pid", &self.pid)
            .field("http_port", &self.http_port)
            .field("csrf_token", &"<redacted>")
            .finish()
    }
}

/// Antigravity OAuth credential JSON file shape.
#[derive(Clone, Deserialize)]
struct AntigravityOAuthFile {
    #[serde(default)]
    token: Option<AntigravityOAuthToken>,
    #[serde(default)]
    auth_method: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

/// Nested `token` object inside an Antigravity OAuth credential payload.
#[derive(Clone, Deserialize)]
struct AntigravityOAuthToken {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expiry: Option<String>,
}

/// Validated Antigravity OAuth credentials used by the direct Cloud Code fallback.
#[derive(Clone)]
struct AntigravityOAuthCreds {
    access_token: String,
    refresh_token: Option<String>,
    expiry_unix: Option<i64>,
    auth_method: String,
    project_id: Option<String>,
}

impl fmt::Debug for AntigravityOAuthCreds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AntigravityOAuthCreds")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expiry_unix", &self.expiry_unix)
            .field("auth_method", &self.auth_method)
            .field("project_id", &self.project_id)
            .finish()
    }
}

/// Subset of `POST /v1internal:loadCodeAssist` response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistResponse {
    #[serde(default)]
    cloudaicompanion_project: Option<String>,
}

/// Outcome of a direct Cloud Code HTTP quota fetch.
enum CcpaFetch {
    Ok(AntigravityQuotaSnapshot),
    /// HTTP 401 or 403; credentials are expired or rejected.
    NeedsLogin,
    /// Network, parse, or other non-auth error.
    Transient,
}

/// Builds a blocking HTTP client for loopback Language Server calls with no proxy and short connect timeout.
fn build_local_ls_client() -> Option<Client> {
    Client::builder()
        .no_proxy()
        .connect_timeout(LS_CONNECT_TIMEOUT)
        .timeout(LS_RPC_TIMEOUT)
        .build()
        .ok()
}

/// Returns whether `base_url` targets a loopback host (`127.0.0.1`, `localhost`, or `[::1]`).
fn is_loopback_url(base_url: &str) -> bool {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Normalizes an `ANTIGRAVITY_LS_ADDRESS` value into a base URL without a trailing slash.
fn normalize_ls_base_url(addr: &str) -> Option<String> {
    let trimmed = addr.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        Some(format!("http://{trimmed}"))
    }
}

/// Slices a single balanced JSON object starting at byte 0 of `input` (which must begin with `{`), respecting string literals and escapes.
fn slice_json_object(input: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&input[..=idx]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extracts `csrfToken` from a `window.__APP_CONFIG__ = {...}` JSON literal embedded in HTML.
fn extract_csrf_token_from_html(html: &str) -> Option<String> {
    const MARKER: &str = "window.__APP_CONFIG__";
    for (pos, _) in html.match_indices(MARKER) {
        let after = &html[pos + MARKER.len()..];
        let Some(eq) = after.find('=') else {
            continue;
        };
        let after_eq = after[eq + 1..].trim_start();
        if !after_eq.starts_with('{') {
            continue;
        }
        let Some(json_slice) = slice_json_object(after_eq) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<serde_json::Value>(json_slice) else {
            continue;
        };
        if let Some(token) = config
            .get("csrfToken")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(token.to_string());
        }
    }
    None
}

/// Probes `GET <base_url>/` with a short timeout and extracts `csrfToken` from `window.__APP_CONFIG__`.
fn fetch_csrf_token(client: &Client, base_url: &str) -> Option<String> {
    let resp = client
        .get(format!("{base_url}/"))
        .timeout(LS_PROBE_TIMEOUT)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let html = resp.text().ok()?;
    extract_csrf_token_from_html(&html)
}

/// Calls `RetrieveUserQuotaSummary` on a local Language Server and returns `(status_code, body)`.
fn call_ls_quota_raw(client: &Client, base_url: &str, csrf_token: &str) -> Result<(u16, String)> {
    let resp = client
        .post(format!("{base_url}{LS_QUOTA_PATH}"))
        .timeout(LS_RPC_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("x-codeium-csrf-token", csrf_token)
        .header("x-antigravity-csrf-token", csrf_token)
        .body(r#"{"forceRefresh":true}"#)
        .send()
        .context("Failed to call Antigravity Language Server RetrieveUserQuotaSummary")?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .context("Failed to read Antigravity Language Server quota response")?;
    Ok((status, body))
}

/// Maps a unified [`AntigravityQuotaSummary`] into an [`AntigravityQuotaSnapshot`] stamped at `now`.
fn map_quota_summary(summary: AntigravityQuotaSummary, now: i64) -> AntigravityQuotaSnapshot {
    let mut buckets = Vec::new();
    for group in summary.groups {
        if group.buckets.is_empty() {
            continue;
        }
        for bucket in group.buckets {
            let Some(remaining) = bucket.remaining_fraction else {
                continue;
            };
            buckets.push(AntigravityBucket {
                group: group.display_name.clone().unwrap_or_default(),
                window: bucket.window.or(bucket.display_name).unwrap_or_default(),
                usage: QuotaWindow {
                    used_percent: (1.0 - remaining.clamp(0.0, 1.0)) * 100.0,
                    resets_at_unix: bucket.reset_time.as_deref().and_then(iso_to_unix_secs),
                },
            });
        }
    }
    let limit_reached = buckets.iter().any(|b| b.usage.used_percent >= 100.0);
    AntigravityQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        buckets,
        limit_reached,
        needs_login: false,
    }
}

/// Maps an LS Connect-RPC `RetrieveUserQuotaSummary` JSON body (`{"response": {"groups": [...]}}`) into a snapshot.
///
/// # Errors
///
/// Returns an error if `body` is not valid JSON or lacks the `response` envelope.
pub fn map_ls_quota(body: &str, now: i64) -> Result<AntigravityQuotaSnapshot> {
    let resp: AntigravityLsQuotaResponse =
        serde_json::from_str(body).context("Failed to parse Antigravity LS quota response")?;
    let summary = resp
        .response
        .context("Antigravity LS quota response missing 'response' field")?;
    Ok(map_quota_summary(summary, now))
}

/// Maps a direct Cloud Code `v1internal:retrieveUserQuotaSummary` JSON body (`{"groups": [...]}`) into a snapshot.
///
/// # Errors
///
/// Returns an error if `body` is not valid JSON matching [`AntigravityQuotaSummary`].
pub fn map_ccpa_quota(body: &str, now: i64) -> Result<AntigravityQuotaSnapshot> {
    let summary: AntigravityQuotaSummary =
        serde_json::from_str(body).context("Failed to parse Antigravity CCPA quota response")?;
    Ok(map_quota_summary(summary, now))
}

/// Returns whether process `pid` is currently alive on this host.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill(pid, 0)` checks process existence without delivering a signal.
    let rc = unsafe { libc::kill(pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

/// Returns whether process `pid` is currently alive on Windows via `OpenProcess` + `GetExitCodeProcess`.
#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` returns null on failure or a valid handle that we close below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code: u32 = 0;
    // SAFETY: `handle` is valid and `exit_code` is a valid out-pointer; handle is closed immediately after.
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe { CloseHandle(handle) };
    ok != 0 && exit_code == STILL_ACTIVE as u32
}

/// Fallback PID check on platforms other than Unix and Windows.
#[cfg(not(any(unix, windows)))]
fn is_pid_alive(pid: u32) -> bool {
    pid > 0
}

/// Returns deduplicated daemon discovery directories: `<app_data_dir>/daemon` (when given) and `<gemini_dir>/*/daemon`.
fn daemon_discovery_dirs(gemini_dir: Option<&Path>, app_data_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    let mut push_if_dir = |candidate: PathBuf| {
        if candidate.is_dir() && seen.insert(candidate.clone()) {
            dirs.push(candidate);
        }
    };

    if let Some(app_data) = app_data_dir.filter(|p| !p.as_os_str().is_empty()) {
        push_if_dir(app_data.join("daemon"));
    }
    if let Some(gemini) = gemini_dir
        && let Ok(entries) = std::fs::read_dir(gemini)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                push_if_dir(path.join("daemon"));
            }
        }
    }
    dirs
}

/// Lists `ls_*.json` and `hub_*.json` files in `dirs`, sorted by mtime descending with ascending path tie-break.
fn collect_daemon_discovery_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates: Vec<(Option<SystemTime>, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if (name.starts_with("ls_") || name.starts_with("hub_"))
                && name.ends_with(".json")
                && path.is_file()
            {
                let mtime = file_mtime(&path);
                candidates.push((mtime, path));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    candidates.into_iter().map(|(_, path)| path).collect()
}

/// Parses a single daemon discovery JSON body into a validated [`DaemonEndpoint`].
fn parse_daemon_discovery_file(body: &str) -> Option<DaemonEndpoint> {
    let file: DaemonDiscoveryFile = serde_json::from_str(body).ok()?;
    let pid = file.pid.filter(|&p| p > 0)?;
    let http_port = file.http_port.filter(|&p| p > 0)?;
    let csrf_token = file
        .csrf_token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    Some(DaemonEndpoint {
        pid,
        http_port,
        csrf_token,
    })
}

/// Tries each live candidate in `files` in order, returning the first successful `(status, body)`.
fn try_discovery_files_raw(
    client: &Client,
    files: &[PathBuf],
    pid_alive: impl Fn(u32) -> bool,
) -> Option<(u16, String)> {
    for path in files {
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(endpoint) = parse_daemon_discovery_file(&body) else {
            continue;
        };
        if !pid_alive(endpoint.pid) {
            continue;
        }
        let base_url = format!("http://127.0.0.1:{}", endpoint.http_port);
        match call_ls_quota_raw(client, &base_url, &endpoint.csrf_token) {
            Ok((status, text))
                if (200..300).contains(&status) && map_ls_quota(&text, 0).is_ok() =>
            {
                return Some((status, text));
            }
            Ok((status, _)) => {
                log::debug!(
                    "antigravity quota: daemon {} returned unusable HTTP {status}",
                    path.display()
                );
            }
            Err(e) => {
                log::debug!(
                    "antigravity quota: daemon {} unreachable: {e}",
                    path.display()
                );
            }
        }
    }
    None
}

/// Returns whether a NUL-separated `/proc/<pid>/cmdline` belongs to a `*-hub-server` or `agy --hub` process.
#[cfg(any(target_os = "linux", test))]
fn cmdline_is_antigravity_hub(cmdline: &[u8]) -> bool {
    let mut parts = cmdline.split(|&b| b == 0).filter(|s| !s.is_empty());
    let Some(argv0_bytes) = parts.next() else {
        return false;
    };
    let argv0 = String::from_utf8_lossy(argv0_bytes);
    let Some(basename) = Path::new(argv0.as_ref())
        .file_name()
        .and_then(|n| n.to_str())
    else {
        return false;
    };
    if basename.ends_with("-hub-server") {
        return true;
    }
    if basename == "agy" {
        return parts.any(|arg| arg == b"--hub");
    }
    false
}

/// Extracts the socket inode number from a `/proc/<pid>/fd/*` symlink target like `socket:[12345]`.
#[cfg(any(target_os = "linux", test))]
fn parse_socket_inode(link_target: &str) -> Option<u64> {
    link_target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse::<u64>()
        .ok()
}

/// Parses `/proc/net/tcp` (`ipv6 == false`) or `/proc/net/tcp6` (`ipv6 == true`) and returns ascending `(port, loopback_host)` pairs for `LISTEN` (`0A`) sockets whose inodes are in `inodes`.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_net_tcp_listening_endpoints(
    table: &str,
    inodes: &HashSet<u64>,
    ipv6: bool,
) -> Vec<(u16, &'static str)> {
    // Wildcard `::` can accept IPv4 (dual-stack) or IPv6-only (`IPV6_V6ONLY=1`), so probe both loopback hosts.
    const IPV6_WILDCARD: &str = "00000000000000000000000000000000";
    // IPv4-mapped loopback `::ffff:127.0.0.1` in `/proc/net/tcp6` hex order.
    const IPV6_MAPPED_LOOPBACK: &str = "0000000000000000FFFF00000100007F";

    let mut endpoints = BTreeSet::new();
    for line in table.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let _slot = cols.next();
        let Some(local_addr) = cols.next() else {
            continue;
        };
        let _rem_addr = cols.next();
        let Some(state) = cols.next() else {
            continue;
        };
        if state != "0A" {
            continue;
        }
        // Columns after `st`: tx_queue:rx_queue (4), tr:tm->when (5), retrnsmt (6), uid (7), timeout (8), inode (9).
        let Some(inode_str) = cols.nth(5) else {
            continue;
        };
        let Ok(inode) = inode_str.parse::<u64>() else {
            continue;
        };
        if !inodes.contains(&inode) {
            continue;
        }
        let Some((hex_ip, hex_port)) = local_addr.rsplit_once(':') else {
            continue;
        };
        if let Ok(port) = u16::from_str_radix(hex_port, 16)
            && port > 0
        {
            if !ipv6 {
                endpoints.insert((port, "127.0.0.1"));
            } else if hex_ip.eq_ignore_ascii_case(IPV6_WILDCARD) {
                endpoints.insert((port, "127.0.0.1"));
                endpoints.insert((port, "[::1]"));
            } else if hex_ip.eq_ignore_ascii_case(IPV6_MAPPED_LOOPBACK) {
                endpoints.insert((port, "127.0.0.1"));
            } else {
                endpoints.insert((port, "[::1]"));
            }
        }
    }
    endpoints.into_iter().collect()
}

/// Lists PIDs of current-user processes in `/proc` matching `*-hub-server` or `agy --hub`, sorted ascending.
#[cfg(target_os = "linux")]
fn find_linux_hub_pids() -> Vec<u32> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no preconditions and always succeeds.
    let my_uid = unsafe { libc::geteuid() };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.uid() != my_uid {
            continue;
        }
        let Ok(cmdline) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
        if cmdline_is_antigravity_hub(&cmdline) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

/// Returns ascending loopback base URLs for listening TCP/TCP6 sockets owned by `pid` on Linux.
#[cfg(target_os = "linux")]
fn linux_pid_listening_base_urls(pid: u32) -> Vec<String> {
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let Ok(entries) = std::fs::read_dir(fd_dir) else {
        return Vec::new();
    };
    let mut inodes = HashSet::new();
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if let Some(inode) = target.to_str().and_then(parse_socket_inode) {
            inodes.insert(inode);
        }
    }
    if inodes.is_empty() {
        return Vec::new();
    }
    let mut endpoints = BTreeSet::new();
    for (table_path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        if let Ok(table) = std::fs::read_to_string(table_path) {
            endpoints.extend(parse_proc_net_tcp_listening_endpoints(
                &table, &inodes, ipv6,
            ));
        }
    }
    endpoints
        .into_iter()
        .map(|(port, host)| format!("http://{host}:{port}"))
        .collect()
}

/// Scans `/proc` for a live local hub and calls `RetrieveUserQuotaSummary`, returning `(hub, status, body)`.
#[cfg(target_os = "linux")]
fn discover_linux_hub_raw(client: &Client) -> Option<(CachedHub, u16, String)> {
    for pid in find_linux_hub_pids() {
        for base_url in linux_pid_listening_base_urls(pid) {
            let Some(csrf_token) = fetch_csrf_token(client, &base_url) else {
                continue;
            };
            match call_ls_quota_raw(client, &base_url, &csrf_token) {
                Ok((status, body))
                    if (200..300).contains(&status) && map_ls_quota(&body, 0).is_ok() =>
                {
                    return Some((
                        CachedHub {
                            pid,
                            base_url,
                            csrf_token,
                        },
                        status,
                        body,
                    ));
                }
                Ok((status, _)) => {
                    log::debug!(
                        "antigravity quota: local hub pid {pid} at {base_url} returned unusable HTTP {status}"
                    );
                }
                Err(e) => {
                    log::debug!("antigravity quota: local hub pid {pid} at {base_url} failed: {e}");
                }
            }
        }
    }
    None
}

/// Decodes a Windows `CredentialBlob` buffer, supporting both `go-keyring` raw UTF-8 and UTF-16LE encodings.
#[cfg(any(windows, test))]
fn decode_credential_blob(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && !text.contains('\0')
    {
        return Some(text.to_string());
    }
    if bytes.len().is_multiple_of(2) {
        let utf16: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if let Ok(text) = String::from_utf16(&utf16) {
            let trimmed = text.trim_end_matches('\0');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// UTF-16 NUL-terminated Windows Credential Manager target `gemini:antigravity`.
#[cfg(windows)]
const WINDOWS_CRED_TARGET: &[u16] = &[
    b'g' as u16,
    b'e' as u16,
    b'm' as u16,
    b'i' as u16,
    b'n' as u16,
    b'i' as u16,
    b':' as u16,
    b'a' as u16,
    b'n' as u16,
    b't' as u16,
    b'i' as u16,
    b'g' as u16,
    b'r' as u16,
    b'a' as u16,
    b'v' as u16,
    b'i' as u16,
    b't' as u16,
    b'y' as u16,
    0,
];

/// Returns whether Windows Credential Manager holds `gemini:antigravity` without prompting.
#[cfg(windows)]
fn os_credential_exists() -> bool {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `WINDOWS_CRED_TARGET` is NUL-terminated UTF-16 and `cred` is a valid out-pointer.
    let ok = unsafe {
        CredReadW(
            WINDOWS_CRED_TARGET.as_ptr(),
            CRED_TYPE_GENERIC,
            0,
            &mut cred,
        )
    };
    if ok == 0 || cred.is_null() {
        return false;
    }
    // SAFETY: `CredReadW` succeeded and returned an allocation owned until `CredFree`.
    unsafe { CredFree(cred.cast()) };
    true
}

/// Reads the `gemini:antigravity` credential blob silently from Windows Credential Manager.
#[cfg(windows)]
fn read_os_credential() -> Option<String> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `WINDOWS_CRED_TARGET` is NUL-terminated UTF-16 and `cred` is a valid out-pointer.
    let ok = unsafe {
        CredReadW(
            WINDOWS_CRED_TARGET.as_ptr(),
            CRED_TYPE_GENERIC,
            0,
            &mut cred,
        )
    };
    if ok == 0 || cred.is_null() {
        return None;
    }
    // SAFETY: `CredReadW` succeeded; we copy the blob bytes before freeing `cred`.
    let bytes = unsafe {
        let len = (*cred).CredentialBlobSize as usize;
        let ptr = (*cred).CredentialBlob;
        let copied = if !ptr.is_null() && len > 0 {
            Some(std::slice::from_raw_parts(ptr, len).to_vec())
        } else {
            None
        };
        CredFree(cred.cast());
        copied
    }?;
    decode_credential_blob(&bytes)
}

/// Returns whether macOS Keychain has `service="gemini"`, `account="antigravity"` using attributes only (`load_data` stays `false`) so Keychain never prompts.
#[cfg(target_os = "macos")]
fn os_credential_exists() -> bool {
    use security_framework::item::{ItemClass, ItemSearchOptions};

    ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service("gemini")
        .account("antigravity")
        .load_attributes(true)
        .search()
        .is_ok_and(|results| !results.is_empty())
}

/// Reads `service="gemini"`, `account="antigravity"` from macOS Keychain, returning `None` on errors (such as headless `errSecInteractionNotAllowed`) so callers fall back cleanly.
#[cfg(target_os = "macos")]
fn read_os_credential() -> Option<String> {
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    match ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service("gemini")
        .account("antigravity")
        .load_data(true)
        .search()
    {
        Ok(results) => results.into_iter().find_map(|item| match item {
            SearchResult::Data(bytes) => String::from_utf8(bytes).ok(),
            _ => None,
        }),
        Err(e) => {
            log::debug!("antigravity quota: macOS Keychain read unavailable: {e}");
            None
        }
    }
}

/// Returns `false` on platforms where Antigravity stores OAuth credentials only in the fallback file.
#[cfg(not(any(windows, target_os = "macos")))]
fn os_credential_exists() -> bool {
    false
}

/// Returns `None` on platforms where Antigravity stores OAuth credentials only in the fallback file.
#[cfg(not(any(windows, target_os = "macos")))]
fn read_os_credential() -> Option<String> {
    None
}

/// Unwraps an optional `go-keyring-base64:` prefix into plain UTF-8 JSON.
fn unwrap_keyring_payload(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(encoded) = trimmed.strip_prefix("go-keyring-base64:") {
        let payload = encoded.trim();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload))
            .ok()?;
        String::from_utf8(decoded).ok()
    } else {
        Some(trimmed.to_string())
    }
}

/// Parses an Antigravity OAuth credential payload (unwrapping `go-keyring-base64:` if present).
fn parse_oauth_credentials(raw: &str) -> Option<AntigravityOAuthCreds> {
    let json = unwrap_keyring_payload(raw)?;
    let parsed: AntigravityOAuthFile = serde_json::from_str(&json).ok()?;
    let token = parsed.token?;
    let access_token = token
        .access_token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let refresh_token = token
        .refresh_token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let expiry_unix = token.expiry.as_deref().and_then(iso_to_unix_secs);
    let auth_method = parsed
        .auth_method
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "consumer".to_string());
    let project_id = parsed
        .project_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(AntigravityOAuthCreds {
        access_token,
        refresh_token,
        expiry_unix,
        auth_method,
        project_id,
    })
}

/// Reads the raw Antigravity OAuth credential payload from the OS store or fallback token file.
fn read_oauth_payload(fallback_path: Option<&Path>) -> Option<String> {
    if let Some(os_payload) = read_os_credential() {
        return Some(os_payload);
    }
    let path = fallback_path?;
    std::fs::read_to_string(path).ok()
}

/// Maps `std::env::consts::OS` to the `os_type` token expected in Antigravity's Cloud Code `User-Agent`.
fn ccpa_os_type(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    }
}

/// Maps `std::env::consts::ARCH` to the `arch` token expected in Antigravity's Cloud Code `User-Agent`.
fn ccpa_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Returns the cached `agy` CLI version string, resolved once per process.
fn antigravity_cli_version() -> &'static str {
    static VER: OnceLock<String> = OnceLock::new();
    VER.get_or_init(|| detect_cli_version("agy", "antigravity", ANTIGRAVITY_FALLBACK_VERSION))
        .as_str()
}

/// Formats the Cloud Code `User-Agent` matching the `agy` CLI.
fn format_ccpa_ua(version: &str, os: &str, arch: &str, auth_method: &str) -> String {
    format!(
        "antigravity/cli/{version} (aidev_client; os_type={}; arch={}; auth_method={auth_method})",
        ccpa_os_type(os),
        ccpa_arch(arch)
    )
}

/// Builds the Cloud Code `User-Agent` for the current host and `auth_method`.
fn antigravity_ccpa_ua(auth_method: &str) -> String {
    format_ccpa_ua(
        antigravity_cli_version(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        auth_method,
    )
}

/// Resolves the Cloud Code base URL from `override_url` (`CLOUD_CODE_URL`) and `auth_method`.
fn ccpa_base_url(auth_method: &str, override_url: Option<&str>) -> String {
    if let Some(url) = override_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(|u| u.trim_end_matches('/'))
    {
        return url.to_string();
    }
    if auth_method == "gcp" {
        CCPA_GCP_BASE.to_string()
    } else {
        CCPA_CONSUMER_BASE.to_string()
    }
}

/// Deterministic FNV-1a 64-bit hash used to key the in-memory `loadCodeAssist` project cache.
fn project_cache_key(base_url: &str, auth_method: &str, identity: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [base_url, auth_method, identity] {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Calls `POST <base_url>/v1internal:loadCodeAssist` and returns `(status_code, body)`.
fn call_load_code_assist_raw(
    client: &Client,
    base_url: &str,
    access_token: &str,
    ua: &str,
) -> Result<(u16, String)> {
    let resp = client
        .post(format!("{base_url}/v1internal:loadCodeAssist"))
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, ua)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(r#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#)
        .send()
        .context("Failed to send Antigravity loadCodeAssist request")?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .context("Failed to read Antigravity loadCodeAssist response body")?;
    Ok((status, body))
}

/// Calls `POST <base_url>/v1internal:retrieveUserQuotaSummary` and returns `(status_code, body)`.
fn call_ccpa_quota_raw(
    client: &Client,
    base_url: &str,
    access_token: &str,
    ua: &str,
    project: &str,
    send_user_project_header: bool,
) -> Result<(u16, String)> {
    let mut req = client
        .post(format!("{base_url}/v1internal:retrieveUserQuotaSummary"))
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, ua)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if send_user_project_header {
        req = req.header("X-Goog-User-Project", project);
    }
    let resp = req
        .json(&serde_json::json!({ "project": project }))
        .send()
        .context("Failed to send Antigravity retrieveUserQuotaSummary request")?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .context("Failed to read Antigravity retrieveUserQuotaSummary response body")?;
    Ok((status, body))
}

/// Injectable core of [`antigravity_quota_present`] with zero network I/O and no Keychain password prompts.
fn antigravity_quota_present_in(
    ls_address: Option<&str>,
    gemini_dir: Option<&Path>,
    app_data_dir: Option<&Path>,
    token_path: Option<&Path>,
    cache_path: Option<&Path>,
    os_cred_exists: impl FnOnce() -> bool,
    linux_hub_exists: impl FnOnce() -> bool,
) -> bool {
    if ls_address.is_some_and(|a| !a.trim().is_empty()) {
        return true;
    }
    let dirs = daemon_discovery_dirs(gemini_dir, app_data_dir);
    if !collect_daemon_discovery_files(&dirs).is_empty() {
        return true;
    }
    if token_path.is_some_and(Path::exists) || os_cred_exists() {
        return true;
    }
    if linux_hub_exists() {
        return true;
    }
    cache_path
        .and_then(load_antigravity_cache_from)
        .is_some_and(|snap| snap.is_present())
}

/// Returns whether any Antigravity quota source or valid cached snapshot exists, with zero network I/O and no macOS Keychain prompts.
pub fn antigravity_quota_present() -> bool {
    let ls_address = std::env::var("ANTIGRAVITY_LS_ADDRESS").ok();
    let app_data_dir = std::env::var_os("ANTIGRAVITY_APP_DATA_DIR").map(PathBuf::from);
    let paths = resolve_paths().ok();
    let gemini_dir = paths.as_ref().map(|p| p.gemini_dir.as_path());
    let token_path = paths.as_ref().map(|p| {
        p.gemini_dir
            .join("antigravity-cli")
            .join("antigravity-oauth-token")
    });
    let cache_path = paths
        .as_ref()
        .map(|p| p.cache_dir.join("quota").join("antigravity.json"));
    antigravity_quota_present_in(
        ls_address.as_deref(),
        gemini_dir,
        app_data_dir.as_deref(),
        token_path.as_deref(),
        cache_path.as_deref(),
        os_credential_exists,
        || {
            #[cfg(target_os = "linux")]
            {
                !find_linux_hub_pids().is_empty()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        },
    )
}

/// Formats the missing-source error for `fetch_antigravity_raw`.
fn missing_source_error(os: &str, token: Option<&Path>) -> String {
    let store = match (os, token) {
        ("windows", Some(path)) => format!(
            "in Windows Credential Manager (gemini:antigravity) or at {}",
            path.display()
        ),
        ("windows", None) => "in Windows Credential Manager (gemini:antigravity)".to_string(),
        ("macos", Some(path)) => {
            format!(
                "in Keychain (gemini / antigravity) or at {}",
                path.display()
            )
        }
        ("macos", None) => "in Keychain (gemini / antigravity)".to_string(),
        (_, Some(path)) => format!("at {}", path.display()),
        (_, None) => String::new(),
    };
    if store.is_empty() {
        format!(
            "no running Antigravity Language Server and no Antigravity sign-in ({ANTIGRAVITY_LOGIN_HINT})"
        )
    } else {
        format!(
            "no running Antigravity Language Server and no Antigravity sign-in {store} ({ANTIGRAVITY_LOGIN_HINT})"
        )
    }
}

/// Fetches the raw Antigravity quota JSON response as `(status_code, body)` using the same 4-tier LS -> direct Cloud Code chain as the dashboard.
///
/// # Errors
///
/// Returns an error if no local Language Server responds and no usable OAuth credential is available, or if the HTTP request fails.
pub fn fetch_antigravity_raw(client: &Client) -> Result<(u16, String)> {
    let local_client = build_local_ls_client();
    let ls_client = local_client.as_ref().unwrap_or(client);
    let ls_address = std::env::var("ANTIGRAVITY_LS_ADDRESS").ok();
    let csrf_token = std::env::var("ANTIGRAVITY_CSRF_TOKEN").ok();
    let app_data_dir = std::env::var_os("ANTIGRAVITY_APP_DATA_DIR").map(PathBuf::from);
    let cloud_code_url = std::env::var("CLOUD_CODE_URL").ok();
    let paths = resolve_paths().ok();
    let gemini_dir = paths.as_ref().map(|p| p.gemini_dir.as_path());
    let token_path = get_antigravity_token_path().ok();

    let discovery_dirs = daemon_discovery_dirs(gemini_dir, app_data_dir.as_deref());
    let discovery_files = collect_daemon_discovery_files(&discovery_dirs);

    fetch_antigravity_raw_with(
        client,
        ls_client,
        ls_address.as_deref(),
        csrf_token.as_deref(),
        &discovery_files,
        is_pid_alive,
        || {
            #[cfg(target_os = "linux")]
            {
                discover_linux_hub_raw(ls_client).map(|(_, status, body)| (status, body))
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
        || read_oauth_payload(token_path.as_deref()),
        cloud_code_url.as_deref(),
        std::env::consts::OS,
        token_path.as_deref(),
    )
}

/// Injectable core of [`fetch_antigravity_raw`].
#[allow(clippy::too_many_arguments)]
fn fetch_antigravity_raw_with(
    client: &Client,
    ls_client: &Client,
    ls_address: Option<&str>,
    csrf_override: Option<&str>,
    discovery_files: &[PathBuf],
    pid_alive: impl Fn(u32) -> bool,
    linux_hub_raw: impl FnOnce() -> Option<(u16, String)>,
    read_oauth: impl FnOnce() -> Option<String>,
    cloud_code_url: Option<&str>,
    os: &str,
    token_path: Option<&Path>,
) -> Result<(u16, String)> {
    let mut last_ls_response: Option<(u16, String)> = None;

    // Tier 1: explicit ANTIGRAVITY_LS_ADDRESS.
    if let Some(base_url) = ls_address.and_then(normalize_ls_base_url) {
        let http = if is_loopback_url(&base_url) {
            ls_client
        } else {
            client
        };
        let csrf = csrf_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| fetch_csrf_token(http, &base_url));
        if let Some(token) = csrf
            && let Ok((status, body)) = call_ls_quota_raw(http, &base_url, &token)
        {
            if (200..300).contains(&status) {
                return Ok((status, body));
            }
            last_ls_response = Some((status, body));
        }
    }

    // Tier 2: daemon discovery files.
    if let Some(res) = try_discovery_files_raw(ls_client, discovery_files, pid_alive) {
        return Ok(res);
    }

    // Tier 3: Linux `/proc` hub discovery.
    if let Some(res) = linux_hub_raw() {
        return Ok(res);
    }

    // Tier 4: direct Cloud Code HTTP fallback via local OAuth credentials.
    if let Some(raw_creds) = read_oauth() {
        let creds = parse_oauth_credentials(&raw_creds).with_context(|| {
            format!("no usable Antigravity credentials ({ANTIGRAVITY_LOGIN_HINT})")
        })?;
        let base_url = ccpa_base_url(&creds.auth_method, cloud_code_url);
        let ua = antigravity_ccpa_ua(&creds.auth_method);
        let (project, send_user_project) = match creds.project_id {
            Some(pid) => (pid, true),
            None => {
                let (status, body) =
                    call_load_code_assist_raw(client, &base_url, &creds.access_token, &ua)?;
                if !(200..300).contains(&status) {
                    return Ok((status, body));
                }
                let parsed: LoadCodeAssistResponse = serde_json::from_str(&body)
                    .context("Failed to parse Antigravity loadCodeAssist response")?;
                let project = parsed
                    .cloudaicompanion_project
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .context("Antigravity loadCodeAssist returned no cloudaicompanionProject")?;
                (project, false)
            }
        };
        return call_ccpa_quota_raw(
            client,
            &base_url,
            &creds.access_token,
            &ua,
            &project,
            send_user_project,
        );
    }

    if let Some(ls_resp) = last_ls_response {
        return Ok(ls_resp);
    }

    bail!("{}", missing_source_error(os, token_path));
}

/// Per-worker Antigravity quota state holding the cached local hub (Linux) and `loadCodeAssist` project ID.
#[derive(Default)]
pub struct AntigravityState {
    /// Loopback HTTP client (`no_proxy`, short connect timeout) built lazily on first poll.
    local_client: Option<Client>,
    /// Cached `(pid, port, csrf_token)` from Linux `/proc` hub discovery.
    cached_hub: Option<CachedHub>,
    /// Cached `(credential_hash, cloudaicompanionProject)` from `loadCodeAssist`.
    cached_project: Option<(u64, String)>,
}

impl AntigravityState {
    /// Runs one quota poll across the 4-tier HTTP resolution chain.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<AntigravityQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        if self.local_client.is_none() {
            self.local_client = build_local_ls_client();
        }
        let ls_client = self.local_client.clone().unwrap_or_else(|| client.clone());

        let ls_address = std::env::var("ANTIGRAVITY_LS_ADDRESS").ok();
        let csrf_token = std::env::var("ANTIGRAVITY_CSRF_TOKEN").ok();
        let app_data_dir = std::env::var_os("ANTIGRAVITY_APP_DATA_DIR").map(PathBuf::from);
        let cloud_code_url = std::env::var("CLOUD_CODE_URL").ok();
        let paths = resolve_paths().ok();
        let gemini_dir = paths.as_ref().map(|p| p.gemini_dir.as_path());
        let token_path = get_antigravity_token_path().ok();

        let discovery_dirs = daemon_discovery_dirs(gemini_dir, app_data_dir.as_deref());
        let discovery_files = collect_daemon_discovery_files(&discovery_dirs);

        self.resolve_with(
            client,
            &ls_client,
            now,
            ls_address.as_deref(),
            csrf_token.as_deref(),
            &discovery_files,
            is_pid_alive,
            || {
                #[cfg(target_os = "linux")]
                {
                    discover_linux_hub_raw(&ls_client)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    None
                }
            },
            || read_oauth_payload(token_path.as_deref()),
            cloud_code_url.as_deref(),
        )
    }

    /// Injectable core of [`Self::resolve`].
    #[allow(clippy::too_many_arguments)]
    fn resolve_with(
        &mut self,
        client: &Client,
        ls_client: &Client,
        now: i64,
        ls_address: Option<&str>,
        csrf_override: Option<&str>,
        discovery_files: &[PathBuf],
        pid_alive: impl Fn(u32) -> bool,
        discover_hub: impl FnOnce() -> Option<(CachedHub, u16, String)>,
        read_oauth: impl FnOnce() -> Option<String>,
        cloud_code_url: Option<&str>,
    ) -> QuotaOutcome<AntigravityQuotaSnapshot> {
        // Tier 1: explicit ANTIGRAVITY_LS_ADDRESS.
        if let Some(base_url) = ls_address.and_then(normalize_ls_base_url) {
            let http = if is_loopback_url(&base_url) {
                ls_client
            } else {
                client
            };
            let csrf = csrf_override
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| fetch_csrf_token(http, &base_url));
            if let Some(token) = csrf {
                match call_ls_quota_raw(http, &base_url, &token) {
                    Ok((status, body)) if (200..300).contains(&status) => {
                        match map_ls_quota(&body, now) {
                            Ok(snap) => return QuotaOutcome::Data(snap),
                            Err(e) => {
                                log::warn!("antigravity quota: failed to parse LS response: {e}");
                            }
                        }
                    }
                    Ok((status, _)) => {
                        log::warn!("antigravity quota: LS {base_url} returned HTTP {status}");
                    }
                    Err(e) => {
                        log::warn!("antigravity quota: LS {base_url} failed: {e}");
                    }
                }
            }
        }

        // Tier 2: local daemon discovery files.
        if let Some((_, body)) = try_discovery_files_raw(ls_client, discovery_files, &pid_alive) {
            match map_ls_quota(&body, now) {
                Ok(snap) => return QuotaOutcome::Data(snap),
                Err(e) => {
                    log::warn!("antigravity quota: failed to parse daemon LS response: {e}");
                }
            }
        }

        // Tier 3: Linux `/proc` hub discovery (with cached `(pid, base_url, csrf_token)` fast path).
        if let Some(hub) = self.cached_hub.clone() {
            if pid_alive(hub.pid)
                && let Ok((status, body)) =
                    call_ls_quota_raw(ls_client, &hub.base_url, &hub.csrf_token)
                && (200..300).contains(&status)
                && let Ok(snap) = map_ls_quota(&body, now)
            {
                return QuotaOutcome::Data(snap);
            }
            self.cached_hub = None;
        }
        if let Some((hub, _, body)) = discover_hub() {
            match map_ls_quota(&body, now) {
                Ok(snap) => {
                    self.cached_hub = Some(hub);
                    return QuotaOutcome::Data(snap);
                }
                Err(e) => {
                    log::warn!("antigravity quota: failed to parse hub LS response: {e}");
                }
            }
        }

        // Tier 4: direct Cloud Code HTTP fallback via local OAuth credentials.
        let Some(raw_creds) = read_oauth() else {
            return QuotaOutcome::Transient;
        };
        let Some(creds) = parse_oauth_credentials(&raw_creds) else {
            return QuotaOutcome::NeedsLogin;
        };
        if creds
            .expiry_unix
            .is_some_and(|exp| exp <= now + TOKEN_EXPIRY_LEEWAY_SECS)
        {
            return QuotaOutcome::NeedsLogin;
        }
        match self.fetch_ccpa(client, &creds, now, cloud_code_url) {
            CcpaFetch::Ok(snap) => QuotaOutcome::Data(snap),
            CcpaFetch::NeedsLogin => QuotaOutcome::NeedsLogin,
            CcpaFetch::Transient => QuotaOutcome::Transient,
        }
    }

    /// Calls `v1internal:loadCodeAssist` (cached) if needed and `v1internal:retrieveUserQuotaSummary`.
    fn fetch_ccpa(
        &mut self,
        client: &Client,
        creds: &AntigravityOAuthCreds,
        now: i64,
        cloud_code_url: Option<&str>,
    ) -> CcpaFetch {
        let base_url = ccpa_base_url(&creds.auth_method, cloud_code_url);
        let ua = antigravity_ccpa_ua(&creds.auth_method);

        let (project, send_user_project) = match &creds.project_id {
            Some(pid) => (pid.clone(), true),
            None => {
                let identity = creds
                    .refresh_token
                    .as_deref()
                    .unwrap_or(&creds.access_token);
                let key = project_cache_key(&base_url, &creds.auth_method, identity);
                if let Some((cached_key, cached_project)) = &self.cached_project
                    && *cached_key == key
                {
                    (cached_project.clone(), false)
                } else {
                    let (status, body) = match call_load_code_assist_raw(
                        client,
                        &base_url,
                        &creds.access_token,
                        &ua,
                    ) {
                        Ok(res) => res,
                        Err(e) => {
                            log::warn!("antigravity quota: loadCodeAssist failed: {e}");
                            return CcpaFetch::Transient;
                        }
                    };
                    if status == 401 || status == 403 {
                        return CcpaFetch::NeedsLogin;
                    }
                    if !(200..300).contains(&status) {
                        log::warn!("antigravity quota: loadCodeAssist returned HTTP {status}");
                        return CcpaFetch::Transient;
                    }
                    let parsed: LoadCodeAssistResponse = match serde_json::from_str(&body) {
                        Ok(p) => p,
                        Err(e) => {
                            log::warn!("antigravity quota: failed to parse loadCodeAssist: {e}");
                            return CcpaFetch::Transient;
                        }
                    };
                    let Some(project) = parsed
                        .cloudaicompanion_project
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                    else {
                        log::warn!(
                            "antigravity quota: loadCodeAssist returned no cloudaicompanionProject"
                        );
                        return CcpaFetch::Transient;
                    };
                    self.cached_project = Some((key, project.clone()));
                    (project, false)
                }
            }
        };

        let (status, body) = match call_ccpa_quota_raw(
            client,
            &base_url,
            &creds.access_token,
            &ua,
            &project,
            send_user_project,
        ) {
            Ok(res) => res,
            Err(e) => {
                log::warn!("antigravity quota: retrieveUserQuotaSummary failed: {e}");
                return CcpaFetch::Transient;
            }
        };
        if status == 401 || status == 403 {
            return CcpaFetch::NeedsLogin;
        }
        if !(200..300).contains(&status) {
            log::warn!("antigravity quota: retrieveUserQuotaSummary returned HTTP {status}");
            return CcpaFetch::Transient;
        }
        match map_ccpa_quota(&body, now) {
            Ok(snap) => CcpaFetch::Ok(snap),
            Err(e) => {
                log::warn!("antigravity quota: failed to parse CCPA quota response: {e}");
                CcpaFetch::Transient
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::http::build_client;
    use httpmock::prelude::*;
    use std::cell::Cell;

    const LS_FIXTURE: &str =
        include_str!("../../../../tests/fixtures/quota/antigravity_quota_summary_response.json");

    fn ccpa_fixture() -> String {
        let v: serde_json::Value = serde_json::from_str(LS_FIXTURE).unwrap();
        v["response"].to_string()
    }

    fn set_mtime(path: &Path, secs: u64) {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn maps_ls_quota_skipping_informational_groups_and_missing_fractions() {
        let snap = map_ls_quota(LS_FIXTURE, 1_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        assert_eq!(snap.fetched_at, 1_000);
        let rows: Vec<_> = snap
            .buckets
            .iter()
            .map(|b| {
                (
                    b.group.as_str(),
                    b.window.as_str(),
                    b.usage.used_percent.round(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Gemini Models", "weekly", 8.0),
                ("Gemini Models", "5h", 23.0),
                ("Claude and GPT models", "weekly", 57.0),
                ("Claude and GPT models", "5h", 0.0),
            ]
        );
        assert_eq!(
            snap.buckets[0].usage.resets_at_unix,
            iso_to_unix_secs("2026-11-14T08:37:03Z")
        );
        assert!(!snap.limit_reached);
        assert!(!snap.needs_login);

        // Spent limit flags limit_reached; missing window falls back to displayName; missing fraction is skipped.
        let edge = r#"{"groups":[
            {"displayName":"Empty Info Group"},
            {"displayName":"Pool","buckets":[
                {"displayName":"Daily","remainingFraction":0},
                {"window":"5h"}
            ]}
        ]}"#;
        let edge_snap = map_ccpa_quota(edge, 42).unwrap();
        assert_eq!(edge_snap.buckets.len(), 1);
        assert_eq!(edge_snap.buckets[0].window, "Daily");
        assert_eq!(edge_snap.buckets[0].usage.used_percent, 100.0);
        assert!(edge_snap.limit_reached);
    }

    #[test]
    fn map_ls_quota_rejects_missing_response_envelope() {
        assert!(map_ls_quota("{}", 0).is_err());
        assert!(map_ls_quota("not json", 0).is_err());
    }

    #[test]
    fn extracts_csrf_token_from_app_config_html() {
        let html = r#"<!doctype html><script>window.__APP_CONFIG__ = {"theme":"dark","csrfToken":"tok-abc-123","nested":{"brace":"}"}};</script>"#;
        assert_eq!(
            extract_csrf_token_from_html(html).as_deref(),
            Some("tok-abc-123")
        );

        // Skips an earlier reference in comments/code without a JSON object literal.
        let multi = r#"<script>// window.__APP_CONFIG__ placeholder
window.__APP_CONFIG__={"csrfToken":"tok-second"};</script>"#;
        assert_eq!(
            extract_csrf_token_from_html(multi).as_deref(),
            Some("tok-second")
        );

        assert_eq!(
            extract_csrf_token_from_html(r#"window.__APP_CONFIG__ = {"csrfToken":""};"#),
            None
        );
        assert_eq!(extract_csrf_token_from_html("<html>no config</html>"), None);
    }

    #[test]
    fn ls_rpc_sends_both_csrf_headers_and_connect_protocol_version() {
        let server = MockServer::start();
        let rpc = server.mock(|when, then| {
            when.method(POST)
                .path(LS_QUOTA_PATH)
                .header("content-type", "application/json")
                .header("connect-protocol-version", "1")
                .header("x-codeium-csrf-token", "secret-csrf")
                .header("x-antigravity-csrf-token", "secret-csrf")
                .body(r#"{"forceRefresh":true}"#);
            then.status(200).body(LS_FIXTURE);
        });

        let client = build_client().unwrap();
        let (status, body) = call_ls_quota_raw(&client, &server.base_url(), "secret-csrf").unwrap();
        rpc.assert();
        assert_eq!(status, 200);
        assert_eq!(map_ls_quota(&body, 10).unwrap().buckets.len(), 4);
    }

    #[test]
    fn tier1_discovers_csrf_via_get_root_when_env_token_absent() {
        let server = MockServer::start();
        let root = server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200)
                .body(r#"<html><script>window.__APP_CONFIG__ = {"csrfToken":"from-html"};</script></html>"#);
        });
        let rpc = server.mock(|when, then| {
            when.method(POST)
                .path(LS_QUOTA_PATH)
                .header("x-codeium-csrf-token", "from-html")
                .header("x-antigravity-csrf-token", "from-html");
            then.status(200).body(LS_FIXTURE);
        });

        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        let outcome = state.resolve_with(
            &client,
            &client,
            1_000,
            Some(&server.address().to_string()),
            None,
            &[],
            |_| false,
            || None,
            || None,
            None,
        );
        root.assert();
        rpc.assert();
        assert!(matches!(outcome, QuotaOutcome::Data(s) if s.buckets.len() == 4));
    }

    #[test]
    fn discovery_files_sort_by_mtime_desc_skip_dead_pids_and_fallback_to_next_live_server() {
        let live_server = MockServer::start();
        let live_rpc = live_server.mock(|when, then| {
            when.method(POST)
                .path(LS_QUOTA_PATH)
                .header("x-codeium-csrf-token", "live-csrf")
                .header("x-antigravity-csrf-token", "live-csrf");
            then.status(200).body(LS_FIXTURE);
        });
        let failing_server = MockServer::start();
        let failing_rpc = failing_server.mock(|when, then| {
            when.method(POST).path(LS_QUOTA_PATH);
            then.status(503).body("unavailable");
        });

        let dir = tempfile::tempdir().unwrap();
        let daemon_dir = dir.path().join("antigravity").join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();

        // Newest: dead PID -> must be skipped without any HTTP request.
        let dead_file = daemon_dir.join("ls_dead.json");
        std::fs::write(
            &dead_file,
            format!(
                r#"{{"pid":999,"httpPort":{},"csrfToken":"dead-csrf"}}"#,
                live_server.port()
            ),
        )
        .unwrap();
        set_mtime(&dead_file, 1_700_000_300);

        // Second newest: empty csrfToken -> must be skipped.
        let empty_csrf = daemon_dir.join("ls_empty.json");
        std::fs::write(
            &empty_csrf,
            format!(
                r#"{{"pid":100,"httpPort":{},"csrfToken":""}}"#,
                live_server.port()
            ),
        )
        .unwrap();
        set_mtime(&empty_csrf, 1_700_000_250);

        // Third newest: live PID, returns HTTP 503 -> must fall back to the next file.
        let bad_file = daemon_dir.join("hub_bad.json");
        std::fs::write(
            &bad_file,
            format!(
                r#"{{"pid":200,"httpPort":{},"csrfToken":"bad-csrf"}}"#,
                failing_server.port()
            ),
        )
        .unwrap();
        set_mtime(&bad_file, 1_700_000_200);

        // Fourth newest: live PID, returns 200.
        let good_file = daemon_dir.join("ls_good.json");
        std::fs::write(
            &good_file,
            format!(
                r#"{{"pid":300,"httpPort":{},"httpsPort":0,"csrfToken":"live-csrf"}}"#,
                live_server.port()
            ),
        )
        .unwrap();
        set_mtime(&good_file, 1_700_000_100);

        // Unrelated file must be ignored.
        std::fs::write(daemon_dir.join("other.json"), "{}").unwrap();

        let dirs = daemon_discovery_dirs(Some(dir.path()), None);
        let files = collect_daemon_discovery_files(&dirs);
        assert_eq!(files, vec![dead_file, empty_csrf, bad_file, good_file]);

        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        let outcome = state.resolve_with(
            &client,
            &client,
            1_000,
            None,
            None,
            &files,
            |pid| pid == 100 || pid == 200 || pid == 300,
            || None,
            || None,
            None,
        );
        failing_rpc.assert();
        live_rpc.assert();
        assert!(matches!(outcome, QuotaOutcome::Data(s) if s.buckets.len() == 4));
    }

    #[test]
    fn discovery_files_tie_break_by_path_ascending() {
        let dir = tempfile::tempdir().unwrap();
        let daemon_dir = dir.path().join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        let b_file = daemon_dir.join("ls_b.json");
        let a_file = daemon_dir.join("ls_a.json");
        std::fs::write(&b_file, "{}").unwrap();
        std::fs::write(&a_file, "{}").unwrap();
        set_mtime(&b_file, 1_700_000_100);
        set_mtime(&a_file, 1_700_000_100);

        let files = collect_daemon_discovery_files(&[daemon_dir]);
        assert_eq!(files, vec![a_file, b_file]);
    }

    #[test]
    fn proc_cmdline_and_tcp_table_parsing() {
        assert!(cmdline_is_antigravity_hub(
            b"/opt/tools/example-hub-server\0--port=0\0"
        ));
        assert!(cmdline_is_antigravity_hub(
            b"/usr/local/bin/agy\0--hub\0--verbose\0"
        ));
        assert!(!cmdline_is_antigravity_hub(b"/usr/local/bin/agy\0models\0"));
        assert!(!cmdline_is_antigravity_hub(b"/bin/bash\0-c\0hub-server\0"));
        assert!(!cmdline_is_antigravity_hub(b""));

        assert_eq!(parse_socket_inode("socket:[987654]"), Some(987654));
        assert_eq!(parse_socket_inode("/dev/null"), None);
        assert_eq!(parse_socket_inode("pipe:[123]"), None);

        // Port 0x1F90 = 8080, 0x0BB8 = 3000, 0x2328 = 9000 (ESTABLISHED 01, ignored).
        let tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11111 1 0 100 0 0 10 0
   1: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0 100 0 0 10 0
   2: 0100007F:2328 0100007F:9999 01 00000000:00000000 00:00000000 00000000  1000        0 33333 1 0 100 0 0 10 0
   3: 00000000:0400 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 99999 1 0 100 0 0 10 0
";
        let inodes: HashSet<u64> = [11111, 22222, 33333].into_iter().collect();
        assert_eq!(
            parse_proc_net_tcp_listening_endpoints(tcp, &inodes, false),
            vec![(3000, "127.0.0.1"), (8080, "127.0.0.1")]
        );

        // `::1` (5000 = 0x1388), `::` wildcard (6000 = 0x1770), and `::ffff:127.0.0.1` (7000 = 0x1B58).
        let tcp6 = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:1388 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 44444
   1: 00000000000000000000000000000000:1770 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55555
   2: 0000000000000000FFFF00000100007F:1B58 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 66666
";
        let inodes6: HashSet<u64> = [44444, 55555, 66666].into_iter().collect();
        assert_eq!(
            parse_proc_net_tcp_listening_endpoints(tcp6, &inodes6, true),
            vec![
                (5000, "[::1]"),
                (6000, "127.0.0.1"),
                (6000, "[::1]"),
                (7000, "127.0.0.1"),
            ]
        );
    }

    #[test]
    fn cached_hub_avoids_rescanning_while_pid_is_alive_and_clears_when_dead() {
        let server = MockServer::start();
        let rpc = server.mock(|when, then| {
            when.method(POST)
                .path(LS_QUOTA_PATH)
                .header("x-antigravity-csrf-token", "hub-csrf");
            then.status(200).body(LS_FIXTURE);
        });

        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        let scans = Cell::new(0);

        // First poll discovers the hub and caches it.
        let outcome = state.resolve_with(
            &client,
            &client,
            1_000,
            None,
            None,
            &[],
            |pid| pid == 4242,
            || {
                scans.set(scans.get() + 1);
                Some((
                    CachedHub {
                        pid: 4242,
                        base_url: server.base_url(),
                        csrf_token: "hub-csrf".to_string(),
                    },
                    200,
                    LS_FIXTURE.to_string(),
                ))
            },
            || None,
            None,
        );
        assert!(matches!(outcome, QuotaOutcome::Data(_)));
        assert_eq!(scans.get(), 1);

        // Second poll reuses the cached hub without calling discover_hub.
        let outcome = state.resolve_with(
            &client,
            &client,
            1_060,
            None,
            None,
            &[],
            |pid| pid == 4242,
            || {
                scans.set(scans.get() + 1);
                None
            },
            || None,
            None,
        );
        rpc.assert_calls(1);
        assert!(matches!(outcome, QuotaOutcome::Data(_)));
        assert_eq!(scans.get(), 1);

        // Third poll sees PID dead -> clears cache and rescans.
        let outcome = state.resolve_with(
            &client,
            &client,
            1_120,
            None,
            None,
            &[],
            |_| false,
            || {
                scans.set(scans.get() + 1);
                None
            },
            || None,
            None,
        );
        assert!(matches!(outcome, QuotaOutcome::Transient));
        assert_eq!(scans.get(), 2);
        assert!(state.cached_hub.is_none());
    }

    #[test]
    fn direct_ccpa_caches_load_code_assist_and_fetches_quota() {
        let server = MockServer::start();
        let load = server.mock(|when, then| {
            when.method(POST)
                .path("/v1internal:loadCodeAssist")
                .header("authorization", "Bearer tok-valid")
                .body(r#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#);
            then.status(200)
                .body(r#"{"cloudaicompanionProject":"proj-from-lca"}"#);
        });
        let quota = server.mock(|when, then| {
            when.method(POST)
                .path("/v1internal:retrieveUserQuotaSummary")
                .header("authorization", "Bearer tok-valid")
                .json_body(serde_json::json!({ "project": "proj-from-lca" }));
            then.status(200).body(ccpa_fixture());
        });

        let creds = serde_json::json!({
            "token": {
                "access_token": "tok-valid",
                "refresh_token": "rt-1",
                "expiry": "2099-01-01T00:00:00Z"
            },
            "auth_method": "consumer"
        })
        .to_string();

        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        for ts in [1_000, 1_060] {
            let outcome = state.resolve_with(
                &client,
                &client,
                ts,
                None,
                None,
                &[],
                |_| false,
                || None,
                || Some(creds.clone()),
                Some(&server.base_url()),
            );
            assert!(matches!(outcome, QuotaOutcome::Data(s) if s.buckets.len() == 4));
        }
        // `loadCodeAssist` was cached across both polls.
        load.assert_calls(1);
        quota.assert_calls(2);
    }

    #[test]
    fn direct_ccpa_with_explicit_project_id_sends_user_project_header_and_skips_lca() {
        let server = MockServer::start();
        let quota = server.mock(|when, then| {
            when.method(POST)
                .path("/v1internal:retrieveUserQuotaSummary")
                .header("authorization", "Bearer tok-gcp")
                .header("x-goog-user-project", "my-gcp-project")
                .json_body(serde_json::json!({ "project": "my-gcp-project" }));
            then.status(200).body(ccpa_fixture());
        });

        let creds = serde_json::json!({
            "token": {
                "access_token": "tok-gcp",
                "expiry": "2099-01-01T00:00:00Z"
            },
            "auth_method": "gcp",
            "project_id": "my-gcp-project"
        })
        .to_string();

        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        let outcome = state.resolve_with(
            &client,
            &client,
            1_000,
            None,
            None,
            &[],
            |_| false,
            || None,
            || Some(creds),
            Some(&server.base_url()),
        );
        quota.assert_calls(1);
        assert!(matches!(outcome, QuotaOutcome::Data(s) if s.buckets.len() == 4));
    }

    #[test]
    fn direct_ccpa_handles_expired_tokens_and_401_403_as_needs_login() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/v1internal:retrieveUserQuotaSummary");
            then.status(401).body(r#"{"error":"unauthorized"}"#);
        });
        let client = build_client().unwrap();
        let mut state = AntigravityState::default();

        // Missing access_token -> NeedsLogin.
        let missing = r#"{"token":{},"auth_method":"consumer"}"#.to_string();
        assert!(matches!(
            state.resolve_with(
                &client,
                &client,
                1_700_000_000,
                None,
                None,
                &[],
                |_| false,
                || None,
                || Some(missing),
                Some(&server.base_url()),
            ),
            QuotaOutcome::NeedsLogin
        ));

        // Expired (within 60s leeway) -> NeedsLogin without making any HTTP call.
        let exp_ts = iso_to_unix_secs("2026-11-21T08:00:00Z").unwrap();
        let expiring = serde_json::json!({
            "token": {
                "access_token": "tok-soon",
                "expiry": "2026-11-21T08:00:00Z"
            },
            "project_id": "p"
        })
        .to_string();
        assert!(matches!(
            state.resolve_with(
                &client,
                &client,
                exp_ts - 60,
                None,
                None,
                &[],
                |_| false,
                || None,
                || Some(expiring),
                Some(&server.base_url()),
            ),
            QuotaOutcome::NeedsLogin
        ));

        // HTTP 401 -> NeedsLogin.
        let valid_exp = serde_json::json!({
            "token": {
                "access_token": "tok-revoked",
                "expiry": "2099-01-01T00:00:00Z"
            },
            "project_id": "p"
        })
        .to_string();
        assert!(matches!(
            state.resolve_with(
                &client,
                &client,
                1_000,
                None,
                None,
                &[],
                |_| false,
                || None,
                || Some(valid_exp),
                Some(&server.base_url()),
            ),
            QuotaOutcome::NeedsLogin
        ));

        // HTTP 403 on loadCodeAssist -> NeedsLogin.
        let server403 = MockServer::start();
        server403.mock(|when, then| {
            when.method(POST).path("/v1internal:loadCodeAssist");
            then.status(403);
        });
        let no_proj = serde_json::json!({
            "token": {
                "access_token": "tok-forbidden",
                "expiry": "2099-01-01T00:00:00Z"
            }
        })
        .to_string();
        assert!(matches!(
            state.resolve_with(
                &client,
                &client,
                1_000,
                None,
                None,
                &[],
                |_| false,
                || None,
                || Some(no_proj),
                Some(&server403.base_url()),
            ),
            QuotaOutcome::NeedsLogin
        ));
    }

    #[test]
    fn no_tier_available_returns_transient() {
        let client = build_client().unwrap();
        let mut state = AntigravityState::default();
        assert!(matches!(
            state.resolve_with(
                &client,
                &client,
                1_000,
                None,
                None,
                &[],
                |_| false,
                || None,
                || None,
                None,
            ),
            QuotaOutcome::Transient
        ));
    }

    #[test]
    fn unwraps_go_keyring_base64_and_windows_credential_blobs() {
        let inner = r#"{"token":{"access_token":"tok-b64","expiry":"2099-01-01T00:00:00Z"},"auth_method":"gcp","project_id":"p1"}"#;
        let encoded = format!(
            "go-keyring-base64:{}",
            base64::engine::general_purpose::STANDARD.encode(inner)
        );
        let parsed = parse_oauth_credentials(&encoded).unwrap();
        assert_eq!(parsed.access_token, "tok-b64");
        assert_eq!(parsed.auth_method, "gcp");
        assert_eq!(parsed.project_id.as_deref(), Some("p1"));

        // Plain JSON works without the prefix too.
        let plain = parse_oauth_credentials(inner).unwrap();
        assert_eq!(plain.access_token, "tok-b64");

        // Debug redacts tokens.
        let dbg = format!("{plain:?}");
        assert!(!dbg.contains("tok-b64"));
        assert!(dbg.contains("<redacted>"));

        // Windows blob decoding handles UTF-8 and UTF-16LE.
        assert_eq!(
            decode_credential_blob(b"{\"token\":{}}").as_deref(),
            Some("{\"token\":{}}")
        );
        let utf16: Vec<u8> = "go-keyring-base64:e30="
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            decode_credential_blob(&utf16).as_deref(),
            Some("go-keyring-base64:e30=")
        );
    }

    #[test]
    fn ccpa_base_url_and_ua_follow_auth_method_and_platform() {
        assert_eq!(ccpa_base_url("consumer", None), CCPA_CONSUMER_BASE);
        assert_eq!(ccpa_base_url("gcp", None), CCPA_GCP_BASE);
        assert_eq!(
            ccpa_base_url("consumer", Some("https://override.test/")),
            "https://override.test"
        );
        assert_eq!(
            format_ccpa_ua("1.5.5", "macos", "aarch64", "consumer"),
            "antigravity/cli/1.5.5 (aidev_client; os_type=darwin; arch=arm64; auth_method=consumer)"
        );
        assert_eq!(
            format_ccpa_ua("1.5.5", "linux", "x86_64", "gcp"),
            "antigravity/cli/1.5.5 (aidev_client; os_type=linux; arch=amd64; auth_method=gcp)"
        );
    }

    #[test]
    fn presence_checks_all_five_sources_hermetically() {
        let dir = tempfile::tempdir().unwrap();
        let gemini = dir.path().join(".gemini");
        let token = gemini
            .join("antigravity-cli")
            .join("antigravity-oauth-token");
        let cache = dir
            .path()
            .join(".vct")
            .join("quota")
            .join("antigravity.json");

        assert!(!antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
        assert!(antigravity_quota_present_in(
            Some("127.0.0.1:42000"),
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
        assert!(antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || true,
            || false,
        ));
        assert!(antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || true,
        ));

        // Daemon discovery file marks presence.
        let daemon_dir = gemini.join("antigravity").join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        let daemon_file = daemon_dir.join("ls_1.json");
        std::fs::write(&daemon_file, "{}").unwrap();
        assert!(antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
        std::fs::remove_file(&daemon_file).unwrap();

        // Fallback token file marks presence.
        std::fs::create_dir_all(token.parent().unwrap()).unwrap();
        std::fs::write(&token, "{}").unwrap();
        assert!(antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
        std::fs::remove_file(&token).unwrap();

        // Valid v2 cached snapshot marks presence; an empty or mismatched cache does not.
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(
            &cache,
            r#"{"schema_version":1,"provider":"antigravity","snapshot":{"buckets":[{"group":"G","window":"5h","usage":{"used_percent":10.0,"resets_at_unix":null}}]}}"#,
        )
        .unwrap();
        assert!(!antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
        std::fs::write(
            &cache,
            r#"{"schema_version":2,"provider":"antigravity","snapshot":{"fetched_at":1,"buckets":[{"group":"G","window":"5h","usage":{"used_percent":10.0,"resets_at_unix":null}}]}}"#,
        )
        .unwrap();
        assert!(antigravity_quota_present_in(
            None,
            Some(&gemini),
            None,
            Some(&token),
            Some(&cache),
            || false,
            || false,
        ));
    }

    #[test]
    fn raw_fetcher_covers_ls_ccpa_and_error_paths() {
        let server = MockServer::start();
        let rpc = server.mock(|when, then| {
            when.method(POST).path(LS_QUOTA_PATH);
            then.status(200).body(LS_FIXTURE);
        });
        let client = build_client().unwrap();

        // LS success.
        let (status, body) = fetch_antigravity_raw_with(
            &client,
            &client,
            Some(&server.base_url()),
            Some("tok"),
            &[],
            |_| false,
            || None,
            || None,
            None,
            "linux",
            None,
        )
        .unwrap();
        rpc.assert_calls(1);
        assert_eq!(status, 200);
        assert!(body.contains("Gemini Models"));

        // CCPA 401 returns status + body verbatim.
        let ccpa = MockServer::start();
        ccpa.mock(|when, then| {
            when.method(POST)
                .path("/v1internal:retrieveUserQuotaSummary");
            then.status(401).body(r#"{"error":"expired"}"#);
        });
        let creds = serde_json::json!({
            "token": { "access_token": "tok-old" },
            "project_id": "p"
        })
        .to_string();
        let (status, body) = fetch_antigravity_raw_with(
            &client,
            &client,
            None,
            None,
            &[],
            |_| false,
            || None,
            || Some(creds),
            Some(&ccpa.base_url()),
            "linux",
            None,
        )
        .unwrap();
        assert_eq!(status, 401);
        assert!(body.contains("expired"));

        // No LS and no credentials -> error with login hint and OS store name.
        let token_path = PathBuf::from("/tmp/antigravity-oauth-token");
        for (os, expected) in [
            ("linux", token_path.display().to_string()),
            ("windows", "gemini:antigravity".to_string()),
            ("macos", "Keychain (gemini / antigravity)".to_string()),
        ] {
            let err = fetch_antigravity_raw_with(
                &client,
                &client,
                None,
                None,
                &[],
                |_| false,
                || None,
                || None,
                None,
                os,
                Some(&token_path),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(&expected), "{os}: {err}");
            assert!(err.contains(ANTIGRAVITY_LOGIN_HINT), "{os}: {err}");
        }
    }
}
