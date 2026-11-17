//! Antigravity quota fetcher.
//!
//! Runs the agy CLI's local `/usage` slash command in print mode and maps each
//! of its limits into a gauge. agy signs in, refreshes and stores its own OAuth
//! token, so vct never reads or writes that token and holds no client secret:
//! it only checks whether agy's sign-in entry exists (in Windows Credential
//! Manager, macOS Keychain, or the fallback file) and reads its modification
//! time, which with agy found is what decides whether the panel shows at all.
//! A build with its own sign-in answers the same command without those stores,
//! so finding it is enough.
//!
//! One run starts a whole agy process (a few seconds and ~260 MB), so the panel
//! polls no more often than [`ANTIGRAVITY_MIN_REFRESH_SECS`]. A signed-out agy
//! waits a minute for an interactive sign-in and may open a browser, so a run
//! that reports a failed sign-in is not repeated until the sign-in entry's
//! modification time changes, across restarts too. An offline agy whose token
//! has expired reports the same failed sign-in, so agy is not run while
//! Google's token endpoint is out of reach.

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
use std::time::{Duration, SystemTime};

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
    /// Whether this build signs in without agy's OAuth sign-in stores, so
    /// those stores say nothing about it.
    pub own_sign_in: bool,
}

/// State of an Antigravity build's sign-in, checked without starting agy or
/// reading its OAuth token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignIn {
    /// A build that signs in on its own, with no store for vct to check.
    Own,
    /// agy's sign-in entry exists, stamped with its modification time when
    /// available.
    Present(Option<SystemTime>),
    /// Neither agy's OS credential entry nor its fallback sign-in file exists.
    Absent,
}

/// Converts a Windows `FILETIME` (100-ns intervals since 1601-01-01 UTC) to a
/// post-`UNIX_EPOCH` [`SystemTime`]. Pre-1970 values return `None` so serde
/// can always serialize a cached hold.
#[cfg(any(windows, test))]
fn filetime_to_system_time(low: u32, high: u32) -> Option<SystemTime> {
    const INTERVALS_PER_SEC: u64 = 10_000_000;
    const INTERVALS_TO_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
    let ticks = (u64::from(high) << 32) | u64::from(low);
    let since_unix = ticks.checked_sub(INTERVALS_TO_UNIX_EPOCH)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(
        since_unix / INTERVALS_PER_SEC,
        ((since_unix % INTERVALS_PER_SEC) * 100) as u32,
    ))
}

/// Converts a Core Foundation `CFAbsoluteTime` (seconds since 2001-01-01 UTC)
/// to a post-`UNIX_EPOCH` [`SystemTime`]. Pre-1970 and non-finite values
/// return `None` so serde can always serialize a cached hold.
#[cfg(any(target_os = "macos", test))]
fn cf_absolute_time_to_system_time(abs_time: f64) -> Option<SystemTime> {
    const CF_EPOCH_UNIX_SECS: f64 = 978_307_200.0;
    let since_unix = Duration::try_from_secs_f64(abs_time + CF_EPOCH_UNIX_SECS).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(since_unix)
}

/// Checks Windows Credential Manager for agy's generic credential
/// (`gemini:antigravity`), returning `Some(last_written)` when present.
#[cfg(windows)]
fn os_credential_mtime() -> Option<Option<SystemTime>> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    // `gemini:antigravity\0` in UTF-16, the `go-keyring` generic credential
    // target (`<service>:<account>`).
    const TARGET: &[u16] = &[
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
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `TARGET` is NUL-terminated UTF-16 and `cred` is a valid out-pointer.
    let ok = unsafe { CredReadW(TARGET.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
    if ok == 0 || cred.is_null() {
        return None;
    }
    // SAFETY: `CredReadW` succeeded, so `cred` points to a valid allocation
    // until `CredFree`. Only `LastWritten` is copied; `CredentialBlob` is not
    // read.
    let last_written = unsafe {
        let ft = (*cred).LastWritten;
        CredFree(cred.cast());
        ft
    };
    Some(filetime_to_system_time(
        last_written.dwLowDateTime,
        last_written.dwHighDateTime,
    ))
}

/// Checks macOS Keychain for agy's generic password (`service="gemini"`,
/// `account="antigravity"`) via attributes only (`load_data` stays `false`),
/// so Keychain neither decrypts the secret nor prompts for authorization.
#[cfg(target_os = "macos")]
fn os_credential_mtime() -> Option<Option<SystemTime>> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::date::CFDate;
    use core_foundation::string::CFStringRef;
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "kSecAttrModificationDate"]
        static SEC_ATTR_MODIFICATION_DATE: CFStringRef;
    }

    let results = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service("gemini")
        .account("antigravity")
        .load_attributes(true)
        .search()
        .ok()?;
    let first = results.first()?;
    let mtime = match first {
        SearchResult::Dict(dict) => {
            // SAFETY: `SEC_ATTR_MODIFICATION_DATE` is an immutable `CFStringRef`
            // constant exported by `Security.framework`.
            let key = unsafe { SEC_ATTR_MODIFICATION_DATE.cast() };
            dict.find(key)
                .map(|value| *value)
                .filter(|ptr| !ptr.is_null())
                .and_then(|ptr| {
                    // SAFETY: values in a `CFDictionary` returned by
                    // `SecItemCopyMatching` are valid Core Foundation objects
                    // borrowed from `dict` ("Get Rule"), and null was filtered
                    // above.
                    unsafe { CFType::wrap_under_get_rule(ptr) }.downcast_into::<CFDate>()
                })
                .and_then(|date| cf_absolute_time_to_system_time(date.abs_time()))
        }
        _ => None,
    };
    Some(mtime)
}

/// Other platforms store agy's sign-in in the fallback file.
#[cfg(not(any(windows, target_os = "macos")))]
fn os_credential_mtime() -> Option<Option<SystemTime>> {
    None
}

/// Checks the fallback sign-in file for existence and modification time,
/// without opening it.
fn file_sign_in(token: Option<&Path>) -> Option<Option<SystemTime>> {
    let token = token.filter(|token| token.exists())?;
    Some(file_mtime(token))
}

/// Merges the OS credential store and fallback file checks into one [`SignIn`].
/// When both exist, the newer modification time wins so re-signing into either
/// store advances the stamp past a `signed_out_mtime` hold.
fn combine_sign_in(cred: Option<Option<SystemTime>>, file: Option<Option<SystemTime>>) -> SignIn {
    match (cred, file) {
        (None, None) => SignIn::Absent,
        (Some(a), None) | (None, Some(a)) => SignIn::Present(a),
        (Some(a), Some(b)) => SignIn::Present(a.max(b)),
    }
}

/// Checks whether agy has a stored sign-in in the OS credential store or at its
/// fallback token file, without starting agy or reading the token.
fn probe_agy_sign_in() -> SignIn {
    let token = get_antigravity_token_path().ok();
    combine_sign_in(os_credential_mtime(), file_sign_in(token.as_deref()))
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
        "macos" => vec![
            agy(home.join(".local").join("bin").join("agy")),
            own_sign_in("/usr/local/bin/jetski"),
        ],
        _ => vec![
            agy(home.join(".local").join("bin").join("agy")),
            own_sign_in("/google/bin/releases/jetski-devs/tools/cli"),
        ],
    }
}

/// The first of `agy` on `path_var`, then `installs`, that exists and can sign
/// in: a build with its own sign-in, or agy when `agy_signed_in`. An agy without a
/// stored sign-in therefore never hides a build with its own sign-in; with no such
/// build, the first that exists, so the missing sign-in can be reported.
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

/// Resolves the paths [`locate_agy`] searches, given an already-probed `agy_signed_in`.
fn locate_agy_with_sign_in(agy_signed_in: bool) -> Option<AgyBinary> {
    let home = resolve_paths().ok()?.home_dir;
    let local_appdata = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join("AppData").join("Local"), PathBuf::from);
    locate_agy_in(
        std::env::var_os("PATH"),
        agy_install_paths(std::env::consts::OS, &home, &local_appdata),
        agy_signed_in,
    )
}

/// Finds agy: `agy` on `PATH`, then where agy or a build with its own sign-in installs on this OS,
/// preferring a build that can sign in.
pub fn locate_agy() -> Option<AgyBinary> {
    locate_agy_with_sign_in(matches!(probe_agy_sign_in(), SignIn::Present(_)))
}

/// Whether the Antigravity panel has something to run: a build with its own sign-in, or agy with a
/// stored sign-in. Checking costs no process, where asking agy would start one.
pub fn antigravity_quota_present() -> bool {
    let agy_signed_in = matches!(probe_agy_sign_in(), SignIn::Present(_));
    match locate_agy_with_sign_in(agy_signed_in) {
        Some(binary) if binary.own_sign_in => true,
        Some(_) => agy_signed_in,
        None => false,
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

/// Error message when agy has no stored sign-in on `os` (a
/// [`std::env::consts::OS`] value).
fn missing_sign_in_error(os: &str, token: Option<&Path>) -> String {
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
        format!("no Antigravity sign-in ({ANTIGRAVITY_LOGIN_HINT})")
    } else {
        format!("no Antigravity sign-in {store} ({ANTIGRAVITY_LOGIN_HINT})")
    }
}

/// Runs `agy -p /usage` once for `vct quota antigravity` and returns its raw
/// stdout, whether or not the run succeeded ([`antigravity_raw_error`] says).
///
/// # Errors
///
/// Returns an error if neither agy nor a build with its own sign-in is found,
/// if agy has no stored sign-in, since agy would then wait a minute for an
/// interactive sign-in, or if agy cannot be started.
pub fn fetch_antigravity_raw() -> Result<String> {
    let sign_in = probe_agy_sign_in();
    let agy = locate_agy_with_sign_in(matches!(sign_in, SignIn::Present(_)))
        .context("agy not found on PATH or at its install paths")?;
    let sign_in = if agy.own_sign_in {
        SignIn::Own
    } else {
        sign_in
    };
    let token = get_antigravity_token_path().ok();
    // agy's own stderr goes to the terminal, so a sign-in prompt explains the
    // wait and a startup error is not swallowed.
    fetch_antigravity_raw_with(sign_in, std::env::consts::OS, token.as_deref(), || {
        agy_usage_command(&agy.path)
            .stderr(Stdio::inherit())
            .output()
            .map(|output| output.stdout)
    })
}

/// The injectable core of [`fetch_antigravity_raw`]: production passes the
/// probed [`SignIn`], OS name, fallback token path and agy runner; tests pass
/// fakes.
fn fetch_antigravity_raw_with(
    sign_in: SignIn,
    os: &str,
    token: Option<&Path>,
    run: impl FnOnce() -> io::Result<Vec<u8>>,
) -> Result<String> {
    if matches!(sign_in, SignIn::Absent) {
        bail!("{}", missing_sign_in_error(os, token));
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
        // Probed before `run` so a re-login during the minute a dead sign-in
        // keeps agy waiting still counts as a change on the next tick.
        let sign_in = probe_agy_sign_in();
        let Some(agy) = locate_agy_with_sign_in(matches!(sign_in, SignIn::Present(_))) else {
            log::warn!("antigravity quota: agy not found; nothing run");
            return QuotaOutcome::Transient;
        };
        let sign_in = if agy.own_sign_in {
            SignIn::Own
        } else {
            sign_in
        };
        self.resolve_with(
            sign_in,
            chrono::Local::now().timestamp(),
            token_endpoint_reachable,
            || agy_usage_command(&agy.path).output(),
        )
    }

    /// The injectable core of [`Self::resolve`]: production passes the probed
    /// [`SignIn`], reachability probe and agy runner; tests pass fakes. Without
    /// a modification timestamp (`SignIn::Own` or `SignIn::Present(None)`) there
    /// is no hold, so a failed sign-in is tried again on the next tick.
    fn resolve_with(
        &mut self,
        sign_in: SignIn,
        now: i64,
        online: impl FnOnce() -> bool,
        run: impl FnOnce() -> io::Result<Output>,
    ) -> QuotaOutcome<AntigravityQuotaSnapshot> {
        let mtime = match sign_in {
            SignIn::Absent => return QuotaOutcome::NeedsLogin,
            SignIn::Own => None,
            SignIn::Present(mtime) => mtime,
        };
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

    fn from_file(token: &Path) -> SignIn {
        combine_sign_in(None, file_sign_in(Some(token)))
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
    fn raw_fetch_without_a_sign_in_never_starts_agy_and_names_the_os_store() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        for (os, expected) in [
            ("linux", token.display().to_string()),
            ("windows", "gemini:antigravity".to_string()),
            ("macos", "Keychain (gemini / antigravity)".to_string()),
        ] {
            let err = fetch_antigravity_raw_with(from_file(&token), os, Some(&token), || {
                panic!("agy must not run")
            })
            .unwrap_err()
            .to_string();
            assert!(err.contains(&expected), "{os}: {err}");
            assert!(err.contains(ANTIGRAVITY_LOGIN_HINT), "{os}: {err}");
        }

        // An OS credential entry suffices even when the fallback file is absent.
        let body = fetch_antigravity_raw_with(
            SignIn::Present(Some(SystemTime::UNIX_EPOCH)),
            "windows",
            Some(&token),
            || Ok(USAGE.as_bytes().to_vec()),
        )
        .unwrap();
        assert_eq!(body, USAGE);

        std::fs::write(&token, "{}").unwrap();
        let body = fetch_antigravity_raw_with(from_file(&token), "linux", Some(&token), || {
            Ok(USAGE.as_bytes().to_vec())
        })
        .unwrap();
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
    fn resolve_without_a_sign_in_asks_for_login_without_starting_agy() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("antigravity-oauth-token");
        let outcome =
            AntigravityState::default()
                .resolve_with(from_file(&token), 0, online, || panic!("agy must not run"));
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
            from_file(&token),
            0,
            online,
            signed_out
        )));
        assert!(matches!(
            state.resolve_with(from_file(&token), 0, online, signed_out),
            QuotaOutcome::NeedsLogin
        ));
        assert_eq!(runs.get(), 1, "a dead sign-in must not relaunch agy");

        // A re-login rewrites the file, which is what earns another run.
        touch_later(&token);
        let outcome = state.resolve_with(from_file(&token), 5, online, || ran(USAGE));
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
            state.resolve_with(from_file(&token), 5, online, || ran(USAGE)),
            QuotaOutcome::Data(_)
        ));
        let QuotaOutcome::Data(saved) =
            state.resolve_with(from_file(&token), 9, online, || ran(SIGNED_OUT))
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
                .resolve_with(from_file(&token), 20, online, || panic!("agy must not run"));
        assert!(matches!(outcome, QuotaOutcome::NeedsLogin));
    }

    #[test]
    fn an_os_credential_sign_in_holds_across_restarts_until_the_credential_updates() {
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let t2 = t1 + Duration::from_secs(60);
        let mut state = AntigravityState::default();

        assert!(matches!(
            state.resolve_with(SignIn::Present(Some(t1)), 5, online, || ran(USAGE)),
            QuotaOutcome::Data(_)
        ));
        let QuotaOutcome::Data(saved) =
            state.resolve_with(SignIn::Present(Some(t1)), 9, online, || ran(SIGNED_OUT))
        else {
            panic!("a failed sign-in must hand out the hold");
        };
        assert!(saved.needs_login);
        assert_eq!(saved.signed_out_mtime, Some(t1));
        assert_eq!(
            saved.buckets.len(),
            4,
            "a failed sign-in must not blank the gauges"
        );
        assert_eq!(saved.fetched_at, 5);

        // Resuming from the persisted cache keeps holding while the OS credential
        // entry carries the same timestamp, and retries once re-login updates it.
        let cached = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        let mut resumed = AntigravityState::resume(cached);
        assert!(matches!(
            resumed.resolve_with(SignIn::Present(Some(t1)), 20, online, || {
                panic!("agy must not run")
            }),
            QuotaOutcome::NeedsLogin
        ));
        let outcome = resumed.resolve_with(SignIn::Present(Some(t2)), 25, online, || ran(USAGE));
        assert!(matches!(
            outcome,
            QuotaOutcome::Data(ref s) if !s.needs_login && s.signed_out_mtime.is_none()
        ));
    }

    #[test]
    fn combine_sign_in_detects_either_store_and_picks_the_newer_timestamp() {
        let older = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let newer = SystemTime::UNIX_EPOCH + Duration::from_secs(200);

        assert_eq!(combine_sign_in(None, None), SignIn::Absent);
        assert_eq!(
            combine_sign_in(Some(Some(older)), None),
            SignIn::Present(Some(older))
        );
        assert_eq!(combine_sign_in(Some(None), None), SignIn::Present(None));
        assert_eq!(
            combine_sign_in(None, Some(Some(older))),
            SignIn::Present(Some(older))
        );
        assert_eq!(
            combine_sign_in(Some(Some(older)), Some(Some(newer))),
            SignIn::Present(Some(newer))
        );
        assert_eq!(
            combine_sign_in(Some(Some(newer)), Some(Some(older))),
            SignIn::Present(Some(newer))
        );
        assert_eq!(
            combine_sign_in(Some(None), Some(Some(older))),
            SignIn::Present(Some(older))
        );
    }

    #[test]
    fn os_credential_timestamps_convert_post_epoch_values_and_reject_pre_epoch() {
        // FILETIME: 1970-01-01T00:00:00Z is 116_444_736_000_000_000 ticks.
        let unix_epoch_ticks: u64 = 116_444_736_000_000_000;
        assert_eq!(filetime_to_system_time(0, 0), None);
        let before = unix_epoch_ticks - 1;
        assert_eq!(
            filetime_to_system_time(before as u32, (before >> 32) as u32),
            None
        );
        assert_eq!(
            filetime_to_system_time(unix_epoch_ticks as u32, (unix_epoch_ticks >> 32) as u32),
            Some(SystemTime::UNIX_EPOCH)
        );
        // +1_700_000_000.5 s in 100-ns intervals.
        let sample = unix_epoch_ticks + 1_700_000_000 * 10_000_000 + 5_000_000;
        assert_eq!(
            filetime_to_system_time(sample as u32, (sample >> 32) as u32),
            Some(SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 500_000_000))
        );

        // CFAbsoluteTime: 0.0 is 2001-01-01T00:00:00Z (UNIX_EPOCH + 978_307_200 s).
        assert_eq!(
            cf_absolute_time_to_system_time(0.0),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(978_307_200))
        );
        assert_eq!(
            cf_absolute_time_to_system_time(-978_307_200.0),
            Some(SystemTime::UNIX_EPOCH)
        );
        assert_eq!(
            cf_absolute_time_to_system_time(100.5),
            Some(SystemTime::UNIX_EPOCH + Duration::new(978_307_300, 500_000_000))
        );
        assert_eq!(cf_absolute_time_to_system_time(-978_307_201.0), None);
        assert_eq!(cf_absolute_time_to_system_time(f64::NAN), None);
        assert_eq!(cf_absolute_time_to_system_time(f64::INFINITY), None);
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

        let outcome = state.resolve_with(
            from_file(&token),
            0,
            || false,
            || panic!("agy must not run"),
        );
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

        // `resolve` probes before `run`, so capture `from_file` beforehand too.
        let before = from_file(&token);
        let outcome = state.resolve_with(before, 0, online, || {
            touch_later(&token);
            ran(SIGNED_OUT)
        });
        assert!(holds_sign_in(&outcome));
        let reran = Cell::new(false);
        let outcome = state.resolve_with(from_file(&token), 5, online, || {
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
            state.resolve_with(from_file(&token), 0, online, not_found),
            QuotaOutcome::Transient
        ));
        let broken = || ran("garbage");
        assert!(matches!(
            state.resolve_with(from_file(&token), 0, online, broken),
            QuotaOutcome::Transient
        ));
        assert!(state.last.signed_out_mtime.is_none());
    }

    #[test]
    fn a_sign_in_without_a_timestamp_retries_each_tick() {
        for sign_in in [SignIn::Own, SignIn::Present(None)] {
            let mut state = AntigravityState::default();
            let runs = Cell::new(0);
            let signed_out = || {
                runs.set(runs.get() + 1);
                ran(SIGNED_OUT)
            };
            assert!(matches!(
                state.resolve_with(sign_in, 0, online, signed_out),
                QuotaOutcome::Data(ref s) if s.needs_login && s.signed_out_mtime.is_none()
            ));
            assert!(matches!(
                state.resolve_with(sign_in, 0, online, signed_out),
                QuotaOutcome::Data(_)
            ));
            assert_eq!(runs.get(), 2, "{sign_in:?}: without a timestamp, retry");
            assert!(matches!(
                state.resolve_with(sign_in, 5, online, || ran(USAGE)),
                QuotaOutcome::Data(ref s) if !s.needs_login && s.buckets.len() == 4
            ));
        }

        let body = fetch_antigravity_raw_with(SignIn::Own, "linux", None, || {
            Ok(USAGE.as_bytes().to_vec())
        })
        .unwrap();
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
            [
                (home.join(".local").join("bin").join("agy"), false),
                (PathBuf::from("/usr/local/bin/jetski"), true),
            ]
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
