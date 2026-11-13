//! Antigravity quota fetcher.
//!
//! Runs the agy CLI's local `/usage` slash command in print mode and maps each
//! of its limits into a gauge. agy signs in, refreshes and stores its own OAuth
//! token, so vct never reads or writes that token and holds no client secret:
//! the sign-in file is only checked for existence, which with agy found is what
//! decides whether the panel shows at all. A build with its own sign-in
//! answers the same command without that file, so finding it is
//! enough.
//!
//! One run starts a whole agy process (a few seconds and ~260 MB), so the panel
//! polls no more often than [`ANTIGRAVITY_MIN_REFRESH_SECS`]. A signed-out agy
//! waits a minute for an interactive sign-in and may open a browser, so a run
//! that reports a failed sign-in is not repeated until the sign-in file changes,
//! across restarts too. An offline agy whose token has expired reports the same
//! failed sign-in, so agy is not run while Google's token endpoint is out of
//! reach.

use crate::models::{
    AgyPrintResponse, AntigravityBucket, AntigravityQuotaSnapshot, QuotaSource, QuotaWindow,
};
use crate::quota::http::iso_to_unix_secs;
use crate::quota::provider::QuotaOutcome;
use crate::quota::refresh::file_mtime;
use crate::utils::{get_antigravity_token_path, resolve_paths};
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Login hint shown when agy has no usable sign-in. `agy acp --login` signs in
/// from the terminal and exits; inside an interactive `agy`, `/login` does the
/// same.
pub const ANTIGRAVITY_LOGIN_HINT: &str = "run: agy acp --login";

/// Shortest interval between two Antigravity polls, whatever the shared quota
/// cadence asks for.
pub const ANTIGRAVITY_MIN_REFRESH_SECS: u64 = 300;

/// Where agy writes its run log instead of its own log directory, so a poll
/// neither adds a file there nor repoints agy's `cli.log` link.
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// An agy build that answers `/usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyBinary {
    /// The executable to run.
    pub path: PathBuf,
    /// Whether this build signs in without agy's sign-in file, so that file
    /// says nothing about it.
    pub own_sign_in: bool,
}

/// Where each build installs on `os` (a [`std::env::consts::OS`] value), in the
/// order they are tried after `agy` on `PATH`. `agy` can also be a shell
/// alias, which a child process never sees.
fn agy_install_paths(os: &str, home: &Path, local_appdata: &Path) -> Vec<AgyBinary> {
    let agy = |path: PathBuf| AgyBinary {
        path,
        own_sign_in: false,
    };
    let own_sign_in = |path: &str| AgyBinary {
        path: PathBuf::from(path),
        own_sign_in: true,
    };
    match os {
        "windows" => vec![agy(local_appdata.join("agy").join("bin").join("agy.exe"))],
        "macos" => vec![own_sign_in("/usr/local/bin/jetski")],
        _ => vec![
            agy(home.join(".local").join("bin").join("agy")),
            own_sign_in("/google/bin/releases/jetski-devs/tools/cli"),
        ],
    }
}

/// The first of `agy` on `path_var`, then `installs`, that exists and can sign
/// in: a build with its own sign-in, or agy when `agy_signed_in`. An agy without its
/// sign-in file therefore never hides a build with its own sign-in; with no such build, the first that
/// exists, so the missing sign-in can be reported.
fn locate_agy_in(
    path_var: Option<OsString>,
    installs: Vec<AgyBinary>,
    agy_signed_in: bool,
) -> Option<AgyBinary> {
    let name = if cfg!(windows) { "agy.exe" } else { "agy" };
    let found: Vec<AgyBinary> = path_var
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| AgyBinary {
            path: dir.join(name),
            own_sign_in: false,
        })
        .chain(installs)
        .filter(|binary| binary.path.is_file())
        .collect();
    found
        .iter()
        .find(|binary| binary.own_sign_in || agy_signed_in)
        .or(found.first())
        .cloned()
}

/// Finds agy: `agy` on `PATH`, then where agy or a build with its own sign-in installs on this OS,
/// preferring a build that can sign in.
pub fn locate_agy() -> Option<AgyBinary> {
    let home = resolve_paths().ok()?.home_dir;
    let local_appdata = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join("AppData").join("Local"), PathBuf::from);
    locate_agy_in(
        std::env::var_os("PATH"),
        agy_install_paths(std::env::consts::OS, &home, &local_appdata),
        get_antigravity_token_path().is_ok_and(|token| token.exists()),
    )
}

/// Whether the Antigravity panel has something to run: a build with its own sign-in, or agy with its
/// sign-in file. Checking costs no process, where asking agy would start one.
pub fn antigravity_quota_present() -> bool {
    match locate_agy() {
        Some(binary) if binary.own_sign_in => true,
        Some(_) => get_antigravity_token_path().is_ok_and(|token| token.exists()),
        None => false,
    }
}

/// agy's sign-in file, or `None` for a build with its own sign-in, which has none to check.
fn sign_in_file(binary: &AgyBinary) -> Result<Option<PathBuf>> {
    if binary.own_sign_in {
        Ok(None)
    } else {
        get_antigravity_token_path().map(Some)
    }
}

/// The `agy -p /usage` command line.
///
/// `output()` closes stdin and captures stderr unless told otherwise, so in the
/// TUI a sign-in prompt can neither read its keystrokes nor draw over it. The
/// exit status is never checked: a failed run still prints its JSON result,
/// which says why.
fn agy_usage_command(agy: &Path) -> Command {
    let mut command = Command::new(agy);
    command.args([
        "--log-file",
        NULL_DEVICE,
        "-p",
        "/usage",
        "--output-format",
        "json",
    ]);
    command
}

/// The last few hundred bytes of agy's stderr, for the log of a run that
/// printed no usable result.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let start = text.len().saturating_sub(300);
    let start = (start..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    text[start..].to_string()
}

/// What one `/usage` run amounted to.
enum AgyUsage {
    Data(AntigravityQuotaSnapshot),
    /// agy could not sign in; carries its message.
    SignedOut(String),
    /// Any other failure; carries the reason.
    Failed(String),
}

/// Maps the JSON a `/usage` run printed into a snapshot (pure).
fn map_agy_usage(body: &[u8], now: i64) -> AgyUsage {
    let resp: AgyPrintResponse = match serde_json::from_slice(body) {
        Ok(resp) => resp,
        Err(e) => return AgyUsage::Failed(format!("unreadable output: {e}")),
    };
    if resp.status.as_deref() != Some("SUCCESS") {
        let error = resp.error.unwrap_or_else(|| "no error message".to_string());
        // A missing or expired sign-in reads "authentication failed or timed
        // out" / "Authentication expired. Please log back in."
        return if error.to_ascii_lowercase().contains("authentication") {
            AgyUsage::SignedOut(error)
        } else {
            AgyUsage::Failed(error)
        };
    }

    let groups = resp
        .command
        .and_then(|command| command.data)
        .map(|data| data.groups)
        .unwrap_or_default();
    let mut buckets = Vec::new();
    for group in groups {
        for bucket in group.buckets {
            let Some(remaining) = bucket.remaining_fraction else {
                continue;
            };
            buckets.push(AntigravityBucket {
                group: group.name.clone().unwrap_or_default(),
                window: bucket.window.or(bucket.name).unwrap_or_default(),
                usage: QuotaWindow {
                    used_percent: (1.0 - remaining.clamp(0.0, 1.0)) * 100.0,
                    resets_at_unix: bucket.reset_time.as_deref().and_then(iso_to_unix_secs),
                },
            });
        }
    }
    let limit_reached = buckets.iter().any(|b| b.usage.used_percent >= 100.0);
    AgyUsage::Data(AntigravityQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        buckets,
        limit_reached,
        needs_login: false,
        signed_out_mtime: None,
    })
}

/// Whether a proxy is configured for agy's HTTPS requests, which agy then
/// sends through it rather than dialing out directly.
fn proxy_configured(var: impl Fn(&str) -> Option<OsString>) -> bool {
    ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// Whether agy can reach Google's OAuth token endpoint, which it refreshes its
/// token against. Behind a proxy the direct connection proves nothing, so it is
/// taken as reachable.
fn token_endpoint_reachable() -> bool {
    if proxy_configured(|name| std::env::var_os(name)) {
        return true;
    }
    let Ok(addrs) = ("oauth2.googleapis.com", 443).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok())
}

/// Runs `agy -p /usage` once for `vct quota antigravity` and returns its raw
/// stdout, whether or not the run succeeded ([`antigravity_raw_error`] says).
///
/// # Errors
///
/// Returns an error if neither agy nor a build with its own sign-in is found, if agy's sign-in file
/// is missing, since agy would then wait a minute for an interactive sign-in,
/// or if agy cannot be started.
pub fn fetch_antigravity_raw() -> Result<String> {
    let agy = locate_agy().context("agy not found on PATH or at its install paths")?;
    // agy's own stderr goes to the terminal, so a sign-in prompt explains the
    // wait and a startup error is not swallowed.
    fetch_antigravity_raw_with(sign_in_file(&agy)?.as_deref(), || {
        agy_usage_command(&agy.path)
            .stderr(Stdio::inherit())
            .output()
            .map(|output| output.stdout)
    })
}

/// The injectable core of [`fetch_antigravity_raw`]: production passes the real
/// sign-in file (`None` for a build with its own sign-in) and agy runner, tests a temp file and a
/// fake.
fn fetch_antigravity_raw_with(
    token: Option<&Path>,
    run: impl FnOnce() -> io::Result<Vec<u8>>,
) -> Result<String> {
    if let Some(token) = token.filter(|token| !token.exists()) {
        bail!(
            "no Antigravity sign-in at {} ({ANTIGRAVITY_LOGIN_HINT})",
            token.display()
        );
    }
    let stdout = run().context("Failed to run agy")?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Why a raw `/usage` result carries no quota, or `None` when it does.
pub fn antigravity_raw_error(body: &str) -> Option<String> {
    match map_agy_usage(body.as_bytes(), 0) {
        AgyUsage::Data(_) => None,
        AgyUsage::SignedOut(e) => {
            Some(format!("agy /usage failed: {e} ({ANTIGRAVITY_LOGIN_HINT})"))
        }
        AgyUsage::Failed(e) => Some(format!("agy /usage failed: {e}")),
    }
}

/// Per-worker Antigravity state.
#[derive(Default)]
pub struct AntigravityState {
    /// The snapshot this worker last handed out, or the cached one it resumed
    /// from. A failed sign-in re-flags it rather than blanking it, and its
    /// `signed_out_mtime` is the hold.
    last: AntigravityQuotaSnapshot,
}

impl AntigravityState {
    /// Resumes from the cached snapshot, so a sign-in found dead before a
    /// restart is not run again after it.
    pub fn resume(cached: AntigravityQuotaSnapshot) -> Self {
        Self { last: cached }
    }

    /// One worker tick: run `/usage` unless agy is known to be signed out or
    /// cannot reach Google.
    pub fn resolve(&mut self) -> QuotaOutcome<AntigravityQuotaSnapshot> {
        let Some(agy) = locate_agy() else {
            log::warn!("antigravity quota: agy not found; nothing run");
            return QuotaOutcome::Transient;
        };
        let Ok(token) = sign_in_file(&agy) else {
            return QuotaOutcome::Transient;
        };
        self.resolve_with(
            token.as_deref(),
            chrono::Local::now().timestamp(),
            token_endpoint_reachable,
            || agy_usage_command(&agy.path).output(),
        )
    }

    /// The injectable core of [`Self::resolve`]: production passes the real
    /// sign-in file (`None` for a build with its own sign-in), reachability probe and agy runner,
    /// tests fakes. Without a sign-in file there is no hold, so a failed sign-in
    /// is tried again on the next tick.
    fn resolve_with(
        &mut self,
        token: Option<&Path>,
        now: i64,
        online: impl FnOnce() -> bool,
        run: impl FnOnce() -> io::Result<Output>,
    ) -> QuotaOutcome<AntigravityQuotaSnapshot> {
        if token.is_some_and(|token| !token.exists()) {
            return QuotaOutcome::NeedsLogin;
        }
        // Read before the run: a re-login during the minute a dead sign-in
        // keeps agy waiting must still count as a change afterwards.
        let mtime = token.and_then(file_mtime);
        if mtime.is_some() && self.last.signed_out_mtime == mtime {
            return QuotaOutcome::NeedsLogin;
        }
        if !online() {
            log::warn!("antigravity quota: Google's token endpoint is unreachable; agy not run");
            return QuotaOutcome::Transient;
        }
        let output = match run() {
            Ok(output) => output,
            Err(e) => {
                log::warn!("antigravity quota: failed to run agy: {e}");
                return QuotaOutcome::Transient;
            }
        };
        match map_agy_usage(&output.stdout, now) {
            AgyUsage::Data(snapshot) => {
                self.last = snapshot.clone();
                QuotaOutcome::Data(snapshot)
            }
            AgyUsage::SignedOut(e) => {
                log::warn!("antigravity quota: agy /usage failed: {e}");
                // Handed out as data, not `NeedsLogin`, so the worker saves the
                // hold to the cache file.
                self.last.needs_login = true;
                self.last.signed_out_mtime = mtime;
                QuotaOutcome::Data(self.last.clone())
            }
            AgyUsage::Failed(e) => {
                log::warn!(
                    "antigravity quota: agy /usage failed: {e}; agy stderr: {}",
                    stderr_tail(&output.stderr)
                );
                QuotaOutcome::Transient
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const USAGE: &str = include_str!("../../../../tests/fixtures/quota/agy_usage_response.json");
    const SIGNED_OUT: &str = r#"{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out","num_turns":0,"finished_reason":"error"}"#;

    /// What a finished agy run hands back, with `stdout` as printed.
    fn ran(stdout: &str) -> io::Result<Output> {
        Ok(Output {
            status: Default::default(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn snapshot(body: &str) -> AntigravityQuotaSnapshot {
        match map_agy_usage(body.as_bytes(), 1_000) {
            AgyUsage::Data(snapshot) => snapshot,
            _ => panic!("expected a snapshot"),
        }
    }

    #[test]
    fn maps_every_limit_of_every_pool_as_percent_used() {
        let snap = snapshot(USAGE);
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
    }

    #[test]
    fn a_spent_limit_flags_the_card() {
        let body = r#"{"status":"SUCCESS","command":{"data":{"groups":[{"name":"Gemini Models","buckets":[{"window":"5h","remaining_fraction":0}]}]}}}"#;
        let snap = snapshot(body);
        assert_eq!(snap.buckets[0].usage.used_percent, 100.0);
        assert!(snap.limit_reached);
    }

    #[test]
    fn a_limit_without_a_window_is_named_by_its_title_and_one_without_a_fraction_is_skipped() {
        let body = r#"{"status":"SUCCESS","command":{"data":{"groups":[{"name":"G","buckets":[{"name":"Daily","remaining_fraction":0.5},{"window":"5h"}]}]}}}"#;
        let snap = snapshot(body);
        assert_eq!(snap.buckets.len(), 1);
        assert_eq!(snap.buckets[0].window, "Daily");
        assert_eq!(snap.buckets[0].usage.used_percent, 50.0);
        assert_eq!(snap.buckets[0].usage.resets_at_unix, None);
    }

    #[test]
    fn a_failed_sign_in_is_told_apart_from_other_failures() {
        assert!(matches!(
            map_agy_usage(SIGNED_OUT.as_bytes(), 0),
            AgyUsage::SignedOut(e) if e == "authentication failed or timed out"
        ));
        assert!(matches!(
            map_agy_usage(
                br#"{"status":"ERROR","error":"quota service unavailable"}"#,
                0
            ),
            AgyUsage::Failed(_)
        ));
        assert!(matches!(map_agy_usage(b"", 0), AgyUsage::Failed(_)));
    }

    #[test]
    fn raw_error_names_the_login_command_only_for_a_failed_sign_in() {
        assert_eq!(antigravity_raw_error(USAGE), None);
        let signed_out = antigravity_raw_error(SIGNED_OUT).unwrap();
        assert!(signed_out.contains(ANTIGRAVITY_LOGIN_HINT), "{signed_out}");
        let other = antigravity_raw_error("not json").unwrap();
        assert!(!other.contains(ANTIGRAVITY_LOGIN_HINT), "{other}");
    }

    #[test]
    fn raw_fetch_without_a_sign_in_file_never_starts_agy() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        let err = fetch_antigravity_raw_with(Some(&token), || panic!("agy must not run"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(ANTIGRAVITY_LOGIN_HINT), "{err}");

        std::fs::write(&token, "{}").unwrap();
        let body =
            fetch_antigravity_raw_with(Some(&token), || Ok(USAGE.as_bytes().to_vec())).unwrap();
        assert_eq!(body, USAGE);
    }

    /// The reachability probe on a connected machine.
    fn online() -> bool {
        true
    }

    /// Whether `outcome` flags a failed sign-in and carries the hold, which is
    /// what the worker saves to the cache file.
    fn holds_sign_in(outcome: &QuotaOutcome<AntigravityQuotaSnapshot>) -> bool {
        matches!(outcome, QuotaOutcome::Data(s) if s.needs_login && s.signed_out_mtime.is_some())
    }

    #[test]
    fn resolve_without_a_sign_in_file_asks_for_login_without_starting_agy() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        let outcome = AntigravityState::default()
            .resolve_with(Some(&token), 0, online, || panic!("agy must not run"));
        assert!(matches!(outcome, QuotaOutcome::NeedsLogin));
    }

    #[test]
    fn a_failed_sign_in_is_not_retried_until_the_sign_in_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(&token, "{}").unwrap();
        let runs = Cell::new(0);
        let signed_out = || {
            runs.set(runs.get() + 1);
            ran(SIGNED_OUT)
        };
        let mut state = AntigravityState::default();

        assert!(holds_sign_in(&state.resolve_with(
            Some(&token),
            0,
            online,
            signed_out
        )));
        assert!(matches!(
            state.resolve_with(Some(&token), 0, online, signed_out),
            QuotaOutcome::NeedsLogin
        ));
        assert_eq!(runs.get(), 1, "a dead sign-in must not relaunch agy");

        // A re-login rewrites the file, which is what earns another run.
        touch_later(&token);
        let outcome = state.resolve_with(Some(&token), 5, online, || ran(USAGE));
        assert!(
            matches!(outcome, QuotaOutcome::Data(ref s) if s.buckets.len() == 4 && !s.needs_login)
        );
        assert!(state.last.signed_out_mtime.is_none());
    }

    #[test]
    fn a_failed_sign_in_keeps_the_gauges_and_stays_held_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(&token, "{}").unwrap();
        let mut state = AntigravityState::default();

        assert!(matches!(
            state.resolve_with(Some(&token), 5, online, || ran(USAGE)),
            QuotaOutcome::Data(_)
        ));
        let QuotaOutcome::Data(saved) =
            state.resolve_with(Some(&token), 9, online, || ran(SIGNED_OUT))
        else {
            panic!("a failed sign-in must hand out the hold");
        };
        assert!(saved.needs_login);
        assert_eq!(
            saved.buckets.len(),
            4,
            "a failed sign-in must not blank the gauges"
        );
        assert_eq!(saved.fetched_at, 5);

        // The next process resumes from what the worker wrote to the cache file.
        let cached = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        let outcome =
            AntigravityState::resume(cached)
                .resolve_with(Some(&token), 20, online, || panic!("agy must not run"));
        assert!(matches!(outcome, QuotaOutcome::NeedsLogin));
    }

    #[test]
    fn only_a_set_https_or_all_proxy_counts_as_a_proxy() {
        let with = |set: &'static str, value: &'static str| {
            move |name: &str| (name == set).then(|| OsString::from(value))
        };
        assert!(proxy_configured(with("HTTPS_PROXY", "http://proxy:3128")));
        assert!(proxy_configured(with("all_proxy", "socks5://proxy:1080")));
        assert!(!proxy_configured(with("HTTPS_PROXY", "")));
        assert!(!proxy_configured(with("HTTP_PROXY", "http://proxy:3128")));
    }

    #[test]
    fn agy_is_not_run_while_google_is_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(&token, "{}").unwrap();
        let mut state = AntigravityState::default();

        let outcome = state.resolve_with(Some(&token), 0, || false, || panic!("agy must not run"));
        assert!(matches!(outcome, QuotaOutcome::Transient));
        assert!(state.last.signed_out_mtime.is_none());
    }

    /// Moves the file's mtime forward, as a re-login rewriting it does.
    fn touch_later(path: &Path) {
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    #[test]
    fn a_re_login_while_a_dead_sign_in_waits_is_not_held_off() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(&token, "{}").unwrap();
        let mut state = AntigravityState::default();

        let outcome = state.resolve_with(Some(&token), 0, online, || {
            touch_later(&token);
            ran(SIGNED_OUT)
        });
        assert!(holds_sign_in(&outcome));
        let reran = Cell::new(false);
        let outcome = state.resolve_with(Some(&token), 5, online, || {
            reran.set(true);
            ran(USAGE)
        });
        assert!(reran.get(), "the new sign-in must get a run");
        assert!(matches!(outcome, QuotaOutcome::Data(_)));
    }

    #[test]
    fn other_failures_keep_the_last_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(&token, "{}").unwrap();
        let mut state = AntigravityState::default();

        let not_found = || Err(io::Error::from(io::ErrorKind::NotFound));
        assert!(matches!(
            state.resolve_with(Some(&token), 0, online, not_found),
            QuotaOutcome::Transient
        ));
        let broken = || ran("garbage");
        assert!(matches!(
            state.resolve_with(Some(&token), 0, online, broken),
            QuotaOutcome::Transient
        ));
        assert!(state.last.signed_out_mtime.is_none());
    }

    #[test]
    fn a_build_with_its_own_sign_in_has_no_file_to_wait_on_or_hold_with() {
        let mut state = AntigravityState::default();
        let runs = Cell::new(0);
        let signed_out = || {
            runs.set(runs.get() + 1);
            ran(SIGNED_OUT)
        };
        assert!(matches!(
            state.resolve_with(None, 0, online, signed_out),
            QuotaOutcome::Data(ref s) if s.needs_login && s.signed_out_mtime.is_none()
        ));
        assert!(matches!(
            state.resolve_with(None, 0, online, signed_out),
            QuotaOutcome::Data(_)
        ));
        assert_eq!(
            runs.get(),
            2,
            "with no sign-in file, every tick tries again"
        );
        assert!(matches!(
            state.resolve_with(None, 5, online, || ran(USAGE)),
            QuotaOutcome::Data(ref s) if !s.needs_login && s.buckets.len() == 4
        ));

        let body = fetch_antigravity_raw_with(None, || Ok(USAGE.as_bytes().to_vec())).unwrap();
        assert_eq!(body, USAGE);
    }

    #[test]
    fn each_os_tries_its_own_installs_in_order() {
        let home = Path::new("/home/u");
        let appdata = Path::new("/appdata");
        let paths = |os| {
            agy_install_paths(os, home, appdata)
                .into_iter()
                .map(|binary| (binary.path, binary.own_sign_in))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths("linux"),
            [
                (home.join(".local").join("bin").join("agy"), false),
                (
                    PathBuf::from("/google/bin/releases/jetski-devs/tools/cli"),
                    true
                ),
            ]
        );
        assert_eq!(
            paths("macos"),
            [(PathBuf::from("/usr/local/bin/jetski"), true)]
        );
        assert_eq!(
            paths("windows"),
            [(appdata.join("agy").join("bin").join("agy.exe"), false)]
        );
    }

    #[test]
    fn agy_on_path_wins_then_the_first_install_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let missing = AgyBinary {
            path: dir.path().join("missing"),
            own_sign_in: false,
        };
        let own = AgyBinary {
            path: dir.path().join("cli"),
            own_sign_in: true,
        };
        std::fs::write(&own.path, "").unwrap();
        let installs = || vec![missing.clone(), own.clone()];
        let path_var = || Some(bin.clone().into_os_string());

        assert_eq!(locate_agy_in(None, installs(), true), Some(own.clone()));
        assert_eq!(
            locate_agy_in(path_var(), installs(), true),
            Some(own.clone())
        );
        let on_path = AgyBinary {
            path: bin.join(if cfg!(windows) { "agy.exe" } else { "agy" }),
            own_sign_in: false,
        };
        std::fs::write(&on_path.path, "").unwrap();
        assert_eq!(
            locate_agy_in(path_var(), installs(), true),
            Some(on_path.clone())
        );
        assert_eq!(locate_agy_in(None, vec![missing.clone()], true), None);

        // An agy that has not signed in never hides a build with its own sign-in, and is still found
        // when it is the only build, so its missing sign-in can be reported.
        assert_eq!(
            locate_agy_in(path_var(), installs(), false),
            Some(own.clone())
        );
        assert_eq!(
            locate_agy_in(path_var(), vec![missing.clone()], false),
            Some(on_path)
        );
    }
}
