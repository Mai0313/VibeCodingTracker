//! Ownership marker for binaries installed by the release installers.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const RELEASE_MANAGED_MARKER: &[u8] = b"vct-release-installer-v1\n";

/// Returns the marker path beside the canonical executable path.
///
/// Canonicalizing first makes an invocation through a symlink use the marker
/// owned by the real executable rather than a marker beside the symlink.
pub(crate) fn release_managed_marker_path(executable: &Path) -> io::Result<PathBuf> {
    let canonical_executable = executable.canonicalize()?;
    let mut marker = OsString::from(canonical_executable.as_os_str());
    marker.push(".vct-managed");
    Ok(PathBuf::from(marker))
}

/// Returns whether `executable` was installed by a VCT release installer.
///
/// The marker is intentionally strict. Any missing, unreadable, or malformed
/// marker means the executable is treated as not release-managed.
pub(crate) fn is_release_managed(executable: &Path) -> bool {
    let Ok(marker_path) = release_managed_marker_path(executable) else {
        return false;
    };

    matches!(fs::read(marker_path), Ok(contents) if contents == RELEASE_MANAGED_MARKER)
}

#[cfg(test)]
mod tests {
    use super::{RELEASE_MANAGED_MARKER, is_release_managed, release_managed_marker_path};
    use std::fs;
    use tempfile::tempdir;

    fn executable(dir: &std::path::Path) -> std::path::PathBuf {
        let executable = dir.join("vibe_coding_tracker");
        fs::write(&executable, b"binary").unwrap();
        executable
    }

    #[test]
    fn accepts_an_exact_release_installer_marker() {
        let dir = tempdir().unwrap();
        let executable = executable(dir.path());
        fs::write(
            release_managed_marker_path(&executable).unwrap(),
            RELEASE_MANAGED_MARKER,
        )
        .unwrap();

        assert!(is_release_managed(&executable));
    }

    #[test]
    fn rejects_an_absent_marker() {
        let dir = tempdir().unwrap();

        assert!(!is_release_managed(&executable(dir.path())));
    }

    #[test]
    fn rejects_a_malformed_marker() {
        let dir = tempdir().unwrap();
        let executable = executable(dir.path());
        fs::write(
            release_managed_marker_path(&executable).unwrap(),
            b"vct-release-installer-v1",
        )
        .unwrap();

        assert!(!is_release_managed(&executable));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_a_symlink_to_the_canonical_executable_marker() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let executable = executable(dir.path());
        let alias = dir.path().join("vct");
        symlink(&executable, &alias).unwrap();
        fs::write(
            release_managed_marker_path(&executable).unwrap(),
            RELEASE_MANAGED_MARKER,
        )
        .unwrap();

        assert!(is_release_managed(&alias));
        assert_eq!(
            release_managed_marker_path(&alias).unwrap(),
            release_managed_marker_path(&executable).unwrap()
        );
    }
}
