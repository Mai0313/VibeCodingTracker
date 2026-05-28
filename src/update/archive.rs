//! Archive extraction for downloaded release assets.
//!
//! Unpacks tar.gz (Unix/macOS) and zip (Windows) archives into a destination
//! directory, then locates the `vibe_coding_tracker` / `vct` binary inside it.
//! Every entry's destination is validated to stay within the target directory,
//! guarding against path-traversal (Zip Slip) in a malicious archive.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::path::Path;
use tar::Archive;
use zip::ZipArchive;

/// Extracts a tar.gz archive into `extract_to` and returns the binary path.
///
/// Each entry is unpacked only after confirming its joined path stays under
/// `extract_to`; an entry that would escape aborts the whole extraction.
///
/// # Errors
///
/// Returns an error if the archive cannot be opened or read, if any entry
/// attempts to escape `extract_to`, if unpacking an entry fails, or if no
/// known binary name is found afterward (see `find_binary_in_directory`).
pub fn extract_targz(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    let tar_gz = File::open(archive_path).context("Failed to open archive file")?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

    // Manually extract with path validation to prevent path traversal attacks
    for entry in archive
        .entries()
        .context("Failed to read archive entries")?
    {
        let mut entry = entry.context("Failed to read archive entry")?;
        let path = entry.path().context("Failed to get entry path")?;

        // Validate that the extracted path stays within extract_to directory
        let full_path = extract_to.join(&path);
        if !full_path.starts_with(extract_to) {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {:?}",
                path
            );
        }

        entry.unpack(&full_path).context("Failed to unpack entry")?;
    }

    find_binary_in_directory(extract_to)
}

/// Extracts a zip archive into `extract_to` and returns the binary path.
///
/// Recreates directory entries and writes file entries, validating each
/// destination against `extract_to` before touching the filesystem.
///
/// # Errors
///
/// Returns an error if the archive cannot be opened or read, if any entry
/// attempts to escape `extract_to`, if a directory or file cannot be created
/// or written, or if no known binary name is found afterward (see
/// `find_binary_in_directory`).
pub fn extract_zip(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    let file = File::open(archive_path).context("Failed to open archive file")?;
    let mut archive = ZipArchive::new(file).context("Failed to read zip archive")?;

    // Manually extract with path validation to prevent path traversal attacks
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let file_path = file.name();

        // Validate that the extracted path stays within extract_to directory
        let full_path = extract_to.join(file_path);
        if !full_path.starts_with(extract_to) {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {}",
                file_path
            );
        }

        if file.is_dir() {
            fs::create_dir_all(&full_path).context("Failed to create directory")?;
        } else {
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            let mut outfile = File::create(&full_path).context("Failed to create file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to write file")?;
        }
    }

    find_binary_in_directory(extract_to)
}

/// Locates the extracted binary by name and marks it executable on Unix.
///
/// Probes the platform-specific candidate names (`vibe_coding_tracker` / `vct`,
/// with `.exe` on Windows) directly under `extract_to` and returns the first
/// that exists. On Unix the found binary is `chmod`-ed to `0o755` so it can be
/// run after the swap.
///
/// # Errors
///
/// Returns an error if no candidate name is found, or on Unix if reading or
/// setting the binary's permissions fails.
fn find_binary_in_directory(extract_to: &Path) -> Result<std::path::PathBuf> {
    // Find the binary in the extracted files
    #[cfg(unix)]
    let binary_names = ["vibe_coding_tracker", "vct"];

    #[cfg(windows)]
    let binary_names = ["vibe_coding_tracker.exe", "vct.exe"];

    for name in &binary_names {
        let binary_path = extract_to.join(name);
        if binary_path.exists() {
            // Make executable on Unix-like systems
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&binary_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&binary_path, perms)?;
            }

            return Ok(binary_path);
        }
    }

    anyhow::bail!("Binary not found in archive")
}
