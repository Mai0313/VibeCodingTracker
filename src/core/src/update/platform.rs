//! Platform-specific pieces of the self-updater.
//!
//! Derives the release asset file name for the host `(os, arch)` and performs
//! the OS-specific binary swap. Unix can rename over the running executable;
//! Windows cannot, so it stages the new binary and launches a quiet helper that
//! finishes the replacement after the process exits.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Path;

#[cfg(windows)]
use std::io::Write;
#[cfg(windows)]
use std::process::{Command, Stdio};

/// Builds the release asset file name for the current platform and `version`.
///
/// Maps the running OS and `env::consts::ARCH` to the release naming scheme
/// (`x86_64` → `x64`, `aarch64` → `arm64`; Linux assets are `-gnu.tar.gz`,
/// macOS `.tar.gz`, Windows `.zip`).
///
/// # Errors
///
/// Returns an error if the host OS is not one of linux, macos, or windows.
pub fn get_asset_pattern(version: &str) -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    // Map Rust arch names to release asset arch names
    let arch_name = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };

    let pattern = match os {
        "linux" => format!(
            "vibe_coding_tracker-v{}-linux-{}-gnu.tar.gz",
            version, arch_name
        ),
        "macos" => format!(
            "vibe_coding_tracker-v{}-macos-{}.tar.gz",
            version, arch_name
        ),
        "windows" => format!("vibe_coding_tracker-v{}-windows-{}.zip", version, arch_name),
        _ => {
            anyhow::bail!("Unsupported platform: {}-{}", os, arch);
        }
    };

    Ok(pattern)
}

/// Copies `new_binary` into an executable sibling staging file on Unix.
///
/// The downloaded candidate may live on a `noexec` temporary filesystem, so
/// validation must run from this sibling file before it is committed.
///
/// # Errors
///
/// Returns an error if the sibling staging file cannot be created, copied,
/// assigned the candidate permissions, or synced.
#[cfg(unix)]
pub fn stage_update_unix(current_exe: &Path, new_binary: &Path) -> Result<tempfile::TempPath> {
    let parent = current_exe
        .parent()
        .context("Current executable has no parent directory")?;
    let staged = tempfile::Builder::new()
        .prefix(".vct-update-")
        .tempfile_in(parent)
        .context("Failed to create sibling update file")?;

    fs::copy(new_binary, staged.path()).context("Failed to stage new binary")?;
    fs::set_permissions(staged.path(), fs::metadata(new_binary)?.permissions())
        .context("Failed to preserve new binary permissions")?;
    staged
        .as_file()
        .sync_all()
        .context("Failed to sync new binary")?;

    Ok(staged.into_temp_path())
}

/// Atomically commits a validated Unix sibling staging file.
///
/// # Errors
///
/// Returns an error if the single rename over `current_exe` fails.
#[cfg(unix)]
pub fn commit_update_unix(current_exe: &Path, staged: tempfile::TempPath) -> Result<()> {
    fs::rename(&staged, current_exe)
        .context("Failed to atomically replace binary with new version")?;

    Ok(())
}

/// Stages `new_binary` and starts a quiet helper to finish the swap on Windows.
///
/// Windows will not let a running executable be replaced. The PowerShell helper
/// acquires the updater's exclusive lock, waits for this process to exit,
/// refuses to replace a newer installed version, retries the replacement while
/// another process still has the executable open, and then deletes itself. The
/// command already in progress is not relaunched.
///
/// # Errors
///
/// Returns an error if the sibling staging/helper files cannot be created or if
/// the helper process cannot be started.
#[cfg(windows)]
pub fn perform_update_windows(
    current_exe: &Path,
    new_binary: &Path,
    expected_version: &str,
    lock_path: &Path,
) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let parent = current_exe
        .parent()
        .context("Current executable has no parent directory")?;
    let mut staged = tempfile::Builder::new()
        .prefix(".vct-update-")
        .suffix(".new")
        .tempfile_in(parent)
        .context("Failed to create sibling update file")?;
    let mut source = fs::File::open(new_binary).context("Failed to open new binary")?;
    std::io::copy(&mut source, staged.as_file_mut()).context("Failed to stage new binary")?;
    staged
        .as_file()
        .sync_all()
        .context("Failed to sync new binary")?;
    let staged_path = staged.path().to_path_buf();

    let mut helper = tempfile::Builder::new()
        .prefix(".vct-update-")
        .suffix(".ps1")
        .tempfile_in(parent)
        .context("Failed to create update helper")?;
    helper
        .write_all(
            windows_update_script(
                current_exe,
                &staged_path,
                expected_version,
                lock_path,
                std::process::id(),
            )
            .as_bytes(),
        )
        .context("Failed to write update helper")?;
    helper
        .as_file()
        .sync_all()
        .context("Failed to sync update helper")?;
    let (_, staged_path) = staged.keep().context("Failed to retain staged binary")?;
    let (_, helper_path) = match helper.keep() {
        Ok(kept) => kept,
        Err(error) => {
            let _ = fs::remove_file(&staged_path);
            return Err(error).context("Failed to retain update helper");
        }
    };

    let spawn = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if let Err(error) = spawn {
        let _ = fs::remove_file(&staged_path);
        let _ = fs::remove_file(&helper_path);
        return Err(error).context("Failed to start update helper");
    }

    Ok(())
}

#[cfg(any(windows, test))]
fn windows_update_script(
    current_exe: &Path,
    staged: &Path,
    expected_version: &str,
    lock_path: &Path,
    parent_pid: u32,
) -> String {
    const TEMPLATE: &str = r#"$ErrorActionPreference = 'Stop'
$target = '__TARGET__'
$staged = '__STAGED__'
$lockPath = '__LOCK_PATH__'
$expected = [Version]'__EXPECTED_VERSION__'
$parentPid = __PARENT_PID__
$lock = $null
$replaced = $false

try {
  for ($attempt = 0; $attempt -lt 1200 -and $null -eq $lock; $attempt++) {
    try {
      $lock = [System.IO.File]::Open(
        $lockPath,
        [System.IO.FileMode]::OpenOrCreate,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::None
      )
    } catch [System.IO.IOException] {
      Start-Sleep -Milliseconds 100
    }
  }
  if ($null -eq $lock) {
    throw 'Timed out while waiting for the update lock'
  }

  while (Get-Process -Id $parentPid -ErrorAction SilentlyContinue) {
    Start-Sleep -Milliseconds 250
  }

  $installed = $null
  try {
    $reported = & $target --version 2>$null | Select-Object -First 1
    if ($LASTEXITCODE -eq 0 -and $null -ne $reported) {
      $baseVersion = ([string]$reported).Trim().TrimStart('v').Split('-')[0]
      $installed = [Version]::Parse($baseVersion)
    }
  } catch {
    $installed = $null
  }

  if ($null -eq $installed -or $installed -le $expected) {
    for ($attempt = 0; $attempt -lt 120 -and -not $replaced; $attempt++) {
      try {
        Move-Item -LiteralPath $staged -Destination $target -Force
        $replaced = $true
      } catch [System.IO.IOException] {
        Start-Sleep -Seconds 1
      } catch [System.UnauthorizedAccessException] {
        Start-Sleep -Seconds 1
      }
    }
  }
} catch {
  $replaced = $false
} finally {
  if (-not $replaced) {
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
  }
  if ($null -ne $lock) {
    $lock.Dispose()
  }
  Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
}
"#;

    TEMPLATE
        .replace("__TARGET__", &powershell_literal(current_exe))
        .replace("__STAGED__", &powershell_literal(staged))
        .replace("__LOCK_PATH__", &powershell_literal(lock_path))
        .replace("__EXPECTED_VERSION__", expected_version)
        .replace("__PARENT_PID__", &parent_pid.to_string())
}

#[cfg(any(windows, test))]
fn powershell_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_replacement_is_atomic_and_leaves_no_fixed_backup() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("vibe_coding_tracker");
        let candidate = dir.path().join("candidate");
        fs::write(&current, b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();

        let staged = stage_update_unix(&current, &candidate).unwrap();
        assert_eq!(staged.parent(), current.parent());
        commit_update_unix(&current, staged).unwrap();

        assert_eq!(fs::read(&current).unwrap(), b"new");
        assert!(!current.with_extension("old").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_staging_failure_keeps_the_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("vibe_coding_tracker");
        fs::write(&current, b"old").unwrap();

        assert!(stage_update_unix(&current, &dir.path().join("missing")).is_err());
        assert_eq!(fs::read(&current).unwrap(), b"old");
    }

    #[cfg(unix)]
    #[test]
    fn unix_rename_failure_keeps_the_old_target() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("vibe_coding_tracker");
        let candidate = dir.path().join("candidate");
        fs::create_dir(&current).unwrap();
        fs::write(current.join("sentinel"), b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();

        let staged = stage_update_unix(&current, &candidate).unwrap();
        assert!(commit_update_unix(&current, staged).is_err());
        assert_eq!(fs::read(current.join("sentinel")).unwrap(), b"old");
    }

    #[test]
    fn windows_helper_holds_the_lock_and_prevents_downgrades() {
        let script = windows_update_script(
            Path::new(r"C:\Program Files\VCT\vibe_coding_tracker.exe"),
            Path::new(r"C:\Program Files\VCT\candidate.new"),
            "2.5.0",
            Path::new(r"C:\Program Files\VCT\.vct-update.lock"),
            4242,
        );

        assert!(script.contains("$parentPid = 4242"));
        assert!(script.contains("[System.IO.FileShare]::None"));
        assert!(script.contains("Start-Sleep -Milliseconds 250"));
        assert!(script.contains("$installed -le $expected"));
        assert!(script.contains("$attempt -lt 120"));
        assert!(script.contains("$target = 'C:\\Program Files\\VCT\\vibe_coding_tracker.exe'"));
        assert!(script.contains("$staged = 'C:\\Program Files\\VCT\\candidate.new'"));
        assert!(script.contains("$lockPath = 'C:\\Program Files\\VCT\\.vct-update.lock'"));
        assert!(script.contains("$expected = [Version]'2.5.0'"));
        assert!(!script.contains("timeout /T"));
    }

    #[test]
    fn windows_helper_escapes_single_quotes_in_paths() {
        let script = windows_update_script(
            Path::new(r"C:\Wei's Tools\vibe_coding_tracker.exe"),
            Path::new(r"C:\Wei's Tools\candidate.new"),
            "2.5.0",
            Path::new(r"C:\Wei's Tools\.vct-update.lock"),
            4242,
        );

        assert!(script.contains(r"$target = 'C:\Wei''s Tools\vibe_coding_tracker.exe'"));
        assert!(script.contains(r"$staged = 'C:\Wei''s Tools\candidate.new'"));
    }
}
