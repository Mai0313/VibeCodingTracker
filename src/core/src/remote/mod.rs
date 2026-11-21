//! Fetches other machines' session ledgers over ssh.
//!
//! Each host in `[remotes].hosts` is asked for `vct ledger export`, and what
//! it prints is stored as that host's snapshot for the scans to fold. Nothing
//! here reads a session: the remote's own vct did, and only its per-day totals
//! come back. A host that cannot be reached keeps the snapshot of its last
//! successful fetch.

use crate::config::{ProvidersConfig, RemotesConfig};
use crate::ledger::{SessionLedger, prune_remotes, store_export};
use crate::pricing::{ModelPricingMap, TierThresholds, fetch_model_pricing};
use crate::utils::{HelperPaths, network_disabled, resolve_paths};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Per-host timeout for synchronous [`sync`] calls; cutting the connection leaves any in-progress remote scan running to populate its ledger for later fetches.
const ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-host timeout for [`spawn_sync_loop`].
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(600);

/// The shortest pause between two fetches of the background loop.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Marker printed to stderr when no `vct` binary is found on the remote host,
/// followed by `<uname -s>:<uname -m>`.
const MISSING_MARKER: &str = "vct-missing:";

/// Remote shell command used when `[remotes].command` is unset.
///
/// Probes common install paths because non-interactive ssh gets a minimal `PATH`, prepends the matching `~/.nvm` bin directory so the npm Node script can find `node`, and emits [`MISSING_MARKER`] with `<uname -s>:<uname -m>` if no binary is found.
const LOCATE_AND_EXPORT: &str = concat!(
    "sh -c '",
    "for v in vct vibe_coding_tracker \"$HOME/.local/bin/vct\" \"$HOME/.local/bin/vibe_coding_tracker\" \"$HOME/.cargo/bin/vibe_coding_tracker\"; do ",
    "command -v \"$v\" >/dev/null 2>&1 && exec \"$v\" ledger export; done; ",
    "for v in \"$HOME\"/.nvm/versions/node/*/bin/vct; do [ -x \"$v\" ] && n=$v; done; ",
    "[ -n \"$n\" ] && PATH=\"${n%/*}:$PATH\" exec \"$n\" ledger export; ",
    "echo \"vct-missing:$(uname -s 2>/dev/null):$(uname -m 2>/dev/null)\" >&2; exit 127'",
);

/// Installs a gzipped `vibe_coding_tracker` binary streamed on stdin into
/// `~/.local/bin` (with a `vct` symlink) and immediately exports its ledger.
const INSTALL_AND_EXPORT: &str = concat!(
    "sh -c 'set -eu; ",
    "cleanup() { [ -n \"$stage\" ] && rm -rf \"$stage\" || true; }; ",
    "stage=\"\"; trap cleanup EXIT; ",
    "dir=\"$HOME/.local/bin\"; mkdir -p \"$dir\"; ",
    "dir=\"$(cd -P \"$dir\" && pwd)\"; ",
    "stage=\"$(mktemp -d \"$dir/.vibe_coding_tracker.XXXXXX\")\"; ",
    "gzip -dc > \"$stage/vibe_coding_tracker\"; ",
    "chmod 755 \"$stage/vibe_coding_tracker\"; ",
    "\"$stage/vibe_coding_tracker\" --version >/dev/null; ",
    "ln -s \"$dir/vibe_coding_tracker\" \"$stage/vct\"; ",
    "mv -f \"$stage/vibe_coding_tracker\" \"$dir/vibe_coding_tracker\"; ",
    "mv -f \"$stage/vct\" \"$dir/vct\"; ",
    "rm -rf \"$stage\"; stage=\"\"; trap - EXIT; ",
    "exec \"$dir/vct\" ledger export </dev/null'",
);

/// Refreshes this machine's session ledger and writes its JSON export to `writer`.
///
/// Updates local provider sessions without reloading remote snapshots; if pricing data cannot be fetched, usage is exported without tier classification.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved, the scan fails, or writing to `writer` fails.
pub fn export(
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
    writer: impl Write,
) -> Result<()> {
    let paths = resolve_paths()?;
    let pricing = fetch_model_pricing().unwrap_or_else(|error| {
        log::warn!("failed to fetch pricing data: {error}; exporting unclassified usage");
        ModelPricingMap::new(HashMap::new())
    });
    let tiers = pricing.tier_thresholds();
    export_from_paths(&paths, providers, pool, Some(&tiers), writer)
}

/// Testable [`export`] entry point rooted at explicit `paths` and `tiers`.
#[doc(hidden)]
pub fn export_from_paths(
    paths: &HelperPaths,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
    tiers: Option<&TierThresholds>,
    writer: impl Write,
) -> Result<()> {
    let mut ledger = SessionLedger::open(&paths.cache_dir);
    pool.install(|| crate::scan::refresh_ledger_for_export(paths, providers, &mut ledger, tiers))?;
    if let Err(error) = ledger.save() {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    ledger.write_export(writer)
}

/// Fetches every configured host's ledger and returns `(host, error)` pairs for hosts that failed.
///
/// Each host has 30 seconds to answer; see [`spawn_sync_loop`] for background polling with a longer timeout. Does nothing and returns an empty list when `VCT_OFFLINE` is set.
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

/// Removes stored snapshots for hosts no longer listed in `config`.
///
/// Call only with a successfully parsed config: an unparseable config yields an empty host list and would delete every stored snapshot.
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

/// Spawns a detached thread that polls configured hosts on `config.refresh_interval` (clamped to at least 60 s) until process exit.
///
/// Does nothing if `config.hosts` is empty. The first fetch runs immediately; failures are written to the diagnostic log.
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
    fetch_all_from(
        cache_dir,
        config,
        program,
        timeout,
        crate::update::GITHUB_API_RELEASES_URL,
    )
}

fn fetch_all_from(
    cache_dir: &Path,
    config: &RemotesConfig,
    program: &OsStr,
    timeout: Duration,
    release_url: &str,
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
                    fetch_one(
                        cache_dir,
                        host,
                        &config.command,
                        program,
                        timeout,
                        release_url,
                    )
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
    release_url: &str,
) -> Result<()> {
    // A leading dash would reach ssh as an option rather than a destination.
    if host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace) {
        bail!("not an ssh destination");
    }
    let started = Instant::now();
    let default_command = command.trim().is_empty();
    let remote_command = if default_command {
        LOCATE_AND_EXPORT.to_string()
    } else {
        format!("{} ledger export", command.trim())
    };
    let output = match run(
        ssh_command(program, host, &remote_command),
        Stdio::null(),
        timeout,
    ) {
        Ok(output) => output,
        Err(RunError::MissingRemoteVct { os, arch }) if default_command => {
            bootstrap_and_export(program, host, &os, &arch, release_url, timeout, started)
                .context("vct is not installed on the remote and automatic install failed")?
        }
        Err(error) => return Err(error.into_anyhow()),
    };
    store_export(cache_dir, &snapshot_name(host), &output)
}

fn ssh_command(program: &OsStr, host: &str, remote_command: &str) -> Command {
    let mut ssh = Command::new(program);
    // BatchMode makes ssh fail instead of asking for a password or a host key,
    // which would otherwise wait on a terminal the TUI owns; `-C` compresses
    // the repetitive JSON export over WAN/VPN links.
    ssh.args(["-C", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(host)
        .arg(remote_command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        ssh.creation_flags(CREATE_NO_WINDOW);
    }
    ssh
}

fn resolve_remote_platform(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    let os = if os.starts_with("Linux") {
        "linux"
    } else if os.starts_with("Darwin") {
        "macos"
    } else {
        bail!(
            "automatic install supports Linux and macOS remotes only (remote reported OS '{os}')"
        );
    };
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => bail!("automatic install does not support remote architecture '{other}'"),
    };
    Ok((os, arch))
}

fn bootstrap_and_export(
    program: &OsStr,
    host: &str,
    raw_os: &str,
    raw_arch: &str,
    release_url: &str,
    timeout: Duration,
    started: Instant,
) -> Result<String> {
    let (os, arch) = resolve_remote_platform(raw_os, raw_arch)?;
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        bail!("timed out before downloading release binary for remote");
    }
    let gz_binary =
        crate::update::prepare_remote_binary_gzip_from(release_url, os, arch, remaining)?;
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining < Duration::from_secs(1) {
        bail!(
            "no answer within {} s; a first export can take longer, so the next fetch may succeed",
            timeout.as_secs()
        );
    }
    run(
        ssh_command(program, host, INSTALL_AND_EXPORT),
        Stdio::from(gz_binary),
        remaining,
    )
    .map_err(RunError::into_anyhow)
}

enum RunError {
    MissingRemoteVct { os: String, arch: String },
    Failed(anyhow::Error),
}

impl RunError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::MissingRemoteVct { os, arch } => {
                anyhow::anyhow!("vct was not found on remote ({os}/{arch})")
            }
            Self::Failed(error) => error,
        }
    }
}

/// Runs `command` to completion and returns its stdout, or the reason it
/// failed: a timeout, a missing-vct bootstrap marker, or its exit status with
/// the last line of its stderr.
fn run(
    mut command: Command,
    stdin: Stdio,
    timeout: Duration,
) -> std::result::Result<String, RunError> {
    command
        .stdin(stdin)
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
    let mut child = command
        .spawn()
        .context("cannot start ssh")
        .map_err(RunError::Failed)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("cannot wait for ssh")
            .map_err(RunError::Failed)?
        {
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
            return Err(RunError::Failed(anyhow::anyhow!(
                "no answer within {} s; a first export can take longer, so the next fetch may succeed",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    // clap's last line is a pointer to `--help`; say what is wrong instead.
    if stderr.contains("unrecognized subcommand") {
        return Err(RunError::Failed(anyhow::anyhow!(
            "the vct installed there has no `ledger export`; update it"
        )));
    }
    if let Some(rest) = stderr
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(MISSING_MARKER))
        && let Some((os, arch)) = rest.split_once(':')
    {
        return Err(RunError::MissingRemoteVct {
            os: os.to_string(),
            arch: arch.to_string(),
        });
    }
    let reason = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no error output");
    Err(RunError::Failed(anyhow::anyhow!("{status}: {reason}")))
}

/// Reads a child's pipe to its end on a thread of its own, so a child that
/// fills one pipe while the other is being read cannot stall.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
    })
}

/// Returns a sanitized directory name for `host`, replacing a leading `.` and any character outside ASCII alphanumerics and `._@-` with `_`.
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
    #[serial_test::serial(update_spawn)]
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
    #[serial_test::serial(update_spawn)]
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
    #[serial_test::serial(update_spawn)]
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
    #[serial_test::serial(update_spawn)]
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
            "-C\n-o\nBatchMode=yes\n-o\nConnectTimeout=10\nwei@devbox\n/opt/vct ledger export\n"
        );
    }

    #[cfg(unix)]
    fn hermetic_posix_bin_dir(root: &Path) -> PathBuf {
        let bin_dir = root.join("posix-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        for tool in [
            "sh", "uname", "mkdir", "pwd", "mktemp", "gzip", "chmod", "ln", "mv", "rm", "printf",
        ] {
            let source = ["/bin", "/usr/bin"]
                .into_iter()
                .map(|dir| Path::new(dir).join(tool))
                .find(|candidate| candidate.exists())
                .unwrap_or_else(|| panic!("missing standard POSIX utility {tool}"));
            std::os::unix::fs::symlink(source, bin_dir.join(tool)).unwrap();
        }
        bin_dir
    }

    #[cfg(unix)]
    fn release_tarball(contents: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};

        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "vibe_coding_tracker", contents)
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(update_spawn)]
    fn a_missing_remote_vct_is_bootstrapped_over_ssh_and_reused_afterwards() {
        use httpmock::prelude::*;
        use serde_json::json;

        let remote_bin = concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--version\" ]; then echo 9.9.9; exit 0; fi\n",
            "if [ \"$1\" = \"ledger\" ] && [ \"$2\" = \"export\" ]; then\n",
            "  printf '%s\\n' '{\"vct_ledger_export\":1,\"providers\":{\"claude\":{\"schema_version\":1,\"provider\":\"claude\",\"sessions\":{}}}}'\n",
            "  exit 0\n",
            "fi\n",
            "exit 2\n",
        )
        .as_bytes();
        let archive_bytes = release_tarball(remote_bin);

        let asset_name =
            crate::update::get_asset_pattern("9.9.9").expect("supported host platform");
        let server = MockServer::start();
        let release_mock = server.mock(|when, then| {
            when.method(GET).path("/releases/latest");
            then.status(200).json_body(json!({
                "tag_name": "v9.9.9",
                "name": "v9.9.9",
                "body": null,
                "assets": [{
                    "name": asset_name,
                    "browser_download_url": server.url("/asset.tar.gz"),
                    "size": archive_bytes.len()
                }]
            }));
        });
        let asset_mock = server.mock(|when, then| {
            when.method(GET).path("/asset.tar.gz");
            then.status(200).body(archive_bytes);
        });

        let remote_home = tempfile::tempdir().unwrap();
        let local_cache = tempfile::tempdir().unwrap();
        let posix_bin = hermetic_posix_bin_dir(local_cache.path());
        let env_file = local_cache.path().join("remote-env.sh");
        std::fs::write(
            &env_file,
            format!(
                "export HOME='{}'\nexport PATH='{}'\n",
                remote_home.path().to_string_lossy().replace('\'', "'\\''"),
                posix_bin.to_string_lossy().replace('\'', "'\\''"),
            ),
        )
        .unwrap();
        let ssh = fake_ssh(
            local_cache.path(),
            &format!(
                ". '{}'\nshift 6\nexec sh -c \"$1\"",
                env_file.to_string_lossy().replace('\'', "'\\''"),
            ),
        );

        let failures = fetch_all_from(
            local_cache.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(10),
            &server.url("/releases/latest"),
        );
        assert_eq!(failures, Vec::new());
        release_mock.assert_calls(1);
        asset_mock.assert_calls(1);
        assert!(remote_home.path().join(".local/bin/vct").exists());
        assert!(
            local_cache
                .path()
                .join("remotes/devbox/sessions/claude.json")
                .exists()
        );

        // Next sync finds `~/.local/bin/vct` on the first ssh call without
        // contacting GitHub again.
        let second = fetch_all_from(
            local_cache.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(10),
            "http://127.0.0.1:9/unreachable",
        );
        assert_eq!(second, Vec::new());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(update_spawn)]
    fn an_unrunnable_remote_candidate_never_lands_in_local_bin() {
        use httpmock::prelude::*;
        use serde_json::json;

        let broken_bin = b"#!/bin/sh\necho 'GLIBC_2.99 not found' >&2\nexit 127\n";
        let archive_bytes = release_tarball(broken_bin);
        let asset_name =
            crate::update::get_asset_pattern("9.9.9").expect("supported host platform");
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/releases/latest");
            then.status(200).json_body(json!({
                "tag_name": "v9.9.9",
                "name": "v9.9.9",
                "body": null,
                "assets": [{
                    "name": asset_name,
                    "browser_download_url": server.url("/asset.tar.gz"),
                    "size": archive_bytes.len()
                }]
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/asset.tar.gz");
            then.status(200).body(archive_bytes);
        });

        let remote_home = tempfile::tempdir().unwrap();
        let local_cache = tempfile::tempdir().unwrap();
        let posix_bin = hermetic_posix_bin_dir(local_cache.path());
        let env_file = local_cache.path().join("remote-env.sh");
        std::fs::write(
            &env_file,
            format!(
                "export HOME='{}'\nexport PATH='{}'\n",
                remote_home.path().to_string_lossy().replace('\'', "'\\''"),
                posix_bin.to_string_lossy().replace('\'', "'\\''"),
            ),
        )
        .unwrap();
        let ssh = fake_ssh(
            local_cache.path(),
            &format!(
                ". '{}'\nshift 6\nexec sh -c \"$1\"",
                env_file.to_string_lossy().replace('\'', "'\\''"),
            ),
        );

        let failures = fetch_all_from(
            local_cache.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(10),
            &server.url("/releases/latest"),
        );
        assert_eq!(failures.len(), 1);
        assert!(failures[0].1.contains("GLIBC_2.99"), "{failures:?}");
        assert!(!remote_home.path().join(".local/bin/vct").exists());
        assert!(
            !remote_home
                .path()
                .join(".local/bin/vibe_coding_tracker")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(update_spawn)]
    fn an_unsupported_remote_os_reports_why_bootstrap_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "echo 'vct-missing:FreeBSD:x86_64' >&2\nexit 127",
        );
        let failures = fetch_all_from(
            dir.path(),
            &config_for("devbox"),
            ssh.as_os_str(),
            Duration::from_secs(5),
            "http://127.0.0.1:9/unused",
        );
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0].1.contains("automatic install failed") && failures[0].1.contains("FreeBSD"),
            "{failures:?}"
        );
    }
}
