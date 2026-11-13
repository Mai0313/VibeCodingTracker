//! Fetches other machines' session ledgers over ssh.
//!
//! Each host in `[remotes].hosts` is asked for `vct ledger export`, and what
//! it prints is stored as that host's snapshot for the scans to fold. Nothing
//! here reads a session: the remote's own vct did, and only its per-day totals
//! come back. A host that cannot be reached keeps the snapshot of its last
//! successful fetch.

use crate::analysis::aggregate_sessions_by_model_from_paths_with_cache;
use crate::config::{ProvidersConfig, RemotesConfig};
use crate::ledger::{SessionLedger, prune_remotes, store_export};
use crate::models::TimeRange;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::usage::{UsageScanOptions, aggregate_usage_from_paths_with_cache_opts};
use crate::utils::{network_disabled, resolve_paths};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one host gets to connect, scan and answer while a one-shot
/// command waits for it before printing anything.
///
/// A first export parses every session the remote has ever kept, which can
/// take minutes; cutting the connection does not stop that scan, so a later
/// fetch finds the remote's ledger built and returns in seconds.
const ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// The same limit for the background loop, which nothing waits on.
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(600);

/// The shortest pause between two fetches of the background loop.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Runs vct on a remote whose config names no command.
///
/// A non-interactive ssh session gets a bare `PATH`, without what a login
/// profile or a version manager adds, so the usual install locations are
/// tried in turn. The npm package's `vct` is a Node script, hence the `PATH`
/// entry for the copy under `~/.nvm`.
const LOCATE_AND_EXPORT: &str = concat!(
    "sh -c '",
    "for v in vct \"$HOME/.local/bin/vct\" \"$HOME/.cargo/bin/vibe_coding_tracker\"; do ",
    "command -v \"$v\" >/dev/null 2>&1 && exec \"$v\" ledger export; done; ",
    "for v in \"$HOME\"/.nvm/versions/node/*/bin/vct; do [ -x \"$v\" ] && n=$v; done; ",
    "[ -n \"$n\" ] && PATH=\"${n%/*}:$PATH\" exec \"$n\" ledger export; ",
    "echo \"vct was not found on PATH, in ~/.local/bin, ~/.cargo/bin or under ~/.nvm; ",
    "set [remotes] command\" >&2; exit 127'",
);

/// Scans this machine through its ledger and writes the export another
/// machine fetches: `vct ledger export`.
///
/// Both halves of every session are brought up to date first. The usage scan
/// goes first because its one parse of a file fills both halves, where an
/// analysis parse leaves the usage half unclassified for the usage scan to
/// read again. A pricing fetch that fails classifies nothing, as in `vct
/// usage`.
///
/// # Errors
///
/// Returns an error if the provider paths cannot be resolved, a scan fails
/// outright, or `writer` cannot be written.
pub fn export(
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
    writer: impl Write,
) -> Result<()> {
    let paths = resolve_paths()?;
    let mut ledger = SessionLedger::open(&paths.cache_dir);
    let pricing = fetch_model_pricing().unwrap_or_else(|error| {
        log::warn!("failed to fetch pricing data: {error}; exporting unclassified usage");
        ModelPricingMap::new(HashMap::new())
    });
    let options = UsageScanOptions {
        tiers: Some(Arc::new(pricing.tier_thresholds())),
    };
    pool.install(|| -> Result<()> {
        aggregate_usage_from_paths_with_cache_opts(
            &paths,
            TimeRange::All,
            providers,
            &mut ledger,
            &options,
        )?;
        aggregate_sessions_by_model_from_paths_with_cache(
            &paths,
            TimeRange::All,
            providers,
            &mut ledger,
        )?;
        Ok(())
    })?;
    if let Err(error) = ledger.save() {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    ledger.write_export(writer)
}

/// Fetches every configured host's ledger and returns the hosts that failed,
/// each with the reason.
///
/// With `VCT_OFFLINE` set nothing is fetched and the stored snapshots stand.
/// The caller is waiting, so a host that does not answer within half a minute
/// is given up on; [`spawn_sync_loop`] is the patient variant.
pub fn sync(cache_dir: &Path, config: &RemotesConfig) -> Vec<(String, String)> {
    sync_within(cache_dir, config, ONE_SHOT_TIMEOUT)
}

fn sync_within(
    cache_dir: &Path,
    config: &RemotesConfig,
    timeout: Duration,
) -> Vec<(String, String)> {
    if network_disabled() {
        return Vec::new();
    }
    fetch_all(cache_dir, config, OsStr::new("ssh"), timeout)
}

/// Removes the snapshots of hosts that are no longer in `config`.
///
/// Call it only with a config known to have parsed: a broken file reads as no
/// hosts at all, and pruning on that would drop every snapshot, including
/// those of hosts that cannot be fetched again right now.
pub fn prune(cache_dir: &Path, config: &RemotesConfig) {
    prune_remotes(cache_dir, &snapshot_names(config));
}

fn snapshot_names(config: &RemotesConfig) -> Vec<String> {
    config
        .hosts
        .iter()
        .map(|host| snapshot_name(host))
        .collect()
}

/// Keeps the snapshots current for as long as the process lives.
///
/// The first fetch starts at once on a detached thread, so a TUI opens on the
/// snapshots already stored and picks up fresh ones at its next refresh.
/// Failures go to the diagnostic log.
pub fn spawn_sync_loop(cache_dir: PathBuf, config: RemotesConfig) {
    if config.hosts.is_empty() {
        return;
    }
    let interval = Duration::from_secs(config.refresh_interval).max(MIN_REFRESH_INTERVAL);
    std::thread::spawn(move || {
        loop {
            for (host, error) in sync_within(&cache_dir, &config, BACKGROUND_TIMEOUT) {
                log::warn!("failed to fetch the ledger of remote {host}: {error}");
            }
            std::thread::sleep(interval);
        }
    });
}

fn fetch_all(
    cache_dir: &Path,
    config: &RemotesConfig,
    program: &OsStr,
    timeout: Duration,
) -> Vec<(String, String)> {
    let names = snapshot_names(config);
    let names = &names;
    std::thread::scope(|scope| {
        let fetches: Vec<_> = config
            .hosts
            .iter()
            .enumerate()
            .map(|(index, host)| {
                scope.spawn(move || {
                    // Two hosts under one name would overwrite each other's files.
                    if names[..index].contains(&names[index]) {
                        bail!("shares its snapshot name with an earlier host; use an ssh alias");
                    }
                    fetch_one(cache_dir, host, &config.command, program, timeout)
                })
            })
            .collect();
        config
            .hosts
            .iter()
            .zip(fetches)
            .filter_map(|(host, fetch)| {
                let result = fetch
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("the fetch panicked")));
                result
                    .err()
                    .map(|error| (host.clone(), format!("{error:#}")))
            })
            .collect()
    })
}

fn fetch_one(
    cache_dir: &Path,
    host: &str,
    command: &str,
    program: &OsStr,
    timeout: Duration,
) -> Result<()> {
    // A leading dash would reach ssh as an option rather than a destination.
    if host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace) {
        bail!("not an ssh destination");
    }
    let remote_command = if command.trim().is_empty() {
        LOCATE_AND_EXPORT.to_string()
    } else {
        format!("{} ledger export", command.trim())
    };
    let mut ssh = Command::new(program);
    // BatchMode makes ssh fail instead of asking for a password or a host key,
    // which would otherwise wait on a terminal the TUI owns.
    ssh.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(host)
        .arg(remote_command);
    let output = run(ssh, timeout)?;
    store_export(cache_dir, &snapshot_name(host), &output)
}

/// Runs `command` to completion and returns its stdout, or the reason it
/// failed: a timeout, or its exit status with the last line of its stderr.
fn run(mut command: Command, timeout: Duration) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own leaves ssh, and the ssh a ProxyJump starts
        // without BatchMode, no terminal to prompt on, and makes the whole
        // family one process group to kill on a timeout.
        // SAFETY: `setsid` is async-signal-safe and touches no memory.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = command.spawn().context("cannot start ssh")?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("cannot wait for ssh")? {
            break status;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            // SAFETY: signals the process group `setsid` created above.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            // The pipes are left to their threads: a descendant that outlives
            // the kill keeps them open, and joining would wait on it.
            bail!(
                "no answer within {} s; a first export can take longer, so the next fetch may succeed",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    // clap's last line is a pointer to `--help`; say what is wrong instead.
    if stderr.contains("unrecognized subcommand") {
        bail!("the vct installed there has no `ledger export`; update it");
    }
    let reason = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no error output");
    bail!("{status}: {reason}")
}

/// Reads a child's pipe to its end on a thread of its own, so a child that
/// fills one pipe while the other is being read cannot stall.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The directory name a host's snapshot is stored under.
///
/// Anything but letters, digits and `._@-` becomes `_`, and so does a leading
/// dot, so no host can name a path outside the remotes directory.
fn snapshot_name(host: &str) -> String {
    host.chars()
        .enumerate()
        .map(|(index, c)| {
            let keep = c.is_ascii_alphanumeric()
                || matches!(c, '_' | '@' | '-')
                || (c == '.' && index > 0);
            if keep { c } else { '_' }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_names_stay_inside_the_remotes_directory() {
        assert_eq!(snapshot_name("wei@dev.box"), "wei@dev.box");
        assert_eq!(snapshot_name(".."), "_.");
        assert_eq!(snapshot_name("a/../b"), "a_.._b");
        assert_eq!(snapshot_name("[::1]:22"), "___1__22");
    }

    #[test]
    fn a_host_that_is_not_a_destination_is_refused_without_running_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let config = RemotesConfig {
            hosts: vec!["-oProxyCommand=x".to_string(), String::new()],
            ..RemotesConfig::default()
        };
        let failures = fetch_all(
            dir.path(),
            &config,
            OsStr::new("/nonexistent/ssh"),
            Duration::from_secs(1),
        );
        assert_eq!(failures.len(), 2);
        assert!(
            failures
                .iter()
                .all(|(_, error)| error == "not an ssh destination")
        );
    }

    #[test]
    fn hosts_sharing_a_snapshot_name_are_not_both_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let config = RemotesConfig {
            hosts: vec!["box:22".to_string(), "box_22".to_string()],
            ..RemotesConfig::default()
        };
        let failures = fetch_all(
            dir.path(),
            &config,
            OsStr::new("/nonexistent/ssh"),
            Duration::from_secs(1),
        );
        assert_eq!(failures.len(), 2);
        assert!(failures[0].1.starts_with("cannot start ssh"));
        assert!(failures[1].1.starts_with("shares its snapshot name"));
    }

    #[cfg(unix)]
    fn fake_ssh(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn config_for(host: &str) -> RemotesConfig {
        RemotesConfig {
            hosts: vec![host.to_string()],
            ..RemotesConfig::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_fetch_stores_what_the_remote_printed_after_its_banner() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            r#"echo "Welcome to devbox"
echo '{"vct_ledger_export":1,"providers":{"claude":{"schema_version":1,"provider":"claude","sessions":{}}}}'"#,
        );
        let failures = fetch_all(
            dir.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(10),
        );
        assert_eq!(failures, Vec::new());
        assert!(
            dir.path()
                .join("remotes/devbox/sessions/claude.json")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_fetch_reports_the_last_stderr_line_and_keeps_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("remotes/devbox/sessions/claude.json");
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        std::fs::write(&snapshot, "kept").unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "echo noise >&2\necho 'Permission denied (publickey).' >&2\nexit 255",
        );
        let failures = fetch_all(
            dir.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(10),
        );
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0].1.ends_with("Permission denied (publickey)."),
            "{failures:?}"
        );
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), "kept");
    }

    #[cfg(unix)]
    #[test]
    fn a_fetch_that_never_answers_is_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        // No `exec`: the sleep is a grandchild holding both pipes open.
        let ssh = fake_ssh(dir.path(), "sleep 30");
        let started = Instant::now();
        let failures = fetch_all(
            dir.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_millis(200),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            failures[0].1.starts_with("no answer within"),
            "{failures:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ssh_gets_batch_mode_the_host_and_the_configured_command() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        let ssh = fake_ssh(
            dir.path(),
            &format!("printf '%s\\n' \"$@\" > {}\nexit 1", args.display()),
        );
        let config = RemotesConfig {
            command: "/opt/vct".to_string(),
            ..config_for("wei@devbox")
        };
        fetch_all(
            dir.path(),
            &config,
            ssh.as_os_str(),
            Duration::from_secs(10),
        );
        assert_eq!(
            std::fs::read_to_string(args).unwrap(),
            "-o\nBatchMode=yes\n-o\nConnectTimeout=10\nwei@devbox\n/opt/vct ledger export\n"
        );
    }
}
