//! Archive extraction for downloaded release assets.
//!
//! Unpacks tar.gz (Unix/macOS) and zip (Windows) archives into a destination
//! directory, then locates the `vibe_coding_tracker` / `vct` binary inside it.
//! Every entry's destination is validated to stay within the target directory,
//! guarding against path-traversal (Zip Slip) in a malicious archive.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
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
        let path = entry
            .path()
            .context("Failed to get entry path")?
            .into_owned();
        if !entry
            .unpack_in(extract_to)
            .context("Failed to unpack entry")?
        {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {:?}",
                path
            );
        }
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
        let file_path = file.name().to_string();
        let Some(enclosed_path) = file.enclosed_name() else {
            anyhow::bail!(
                "Archive contains invalid path that attempts to escape extraction directory: {}",
                file_path
            );
        };
        let full_path = extract_to.join(enclosed_path);

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
fn find_binary_in_directory(extract_to: &Path) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    #[test]
    fn extract_targz_rejects_parent_directory_entries() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let archive_path = temp_dir.path().join("malicious.tar.gz");
        let extract_dir = temp_dir.path().join("extract");
        fs::create_dir(&extract_dir)?;

        let data = b"outside";
        let mut encoder = GzEncoder::new(File::create(&archive_path)?, Compression::default());
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.as_mut_bytes()[..7].copy_from_slice(b"../evil");
        header.set_cksum();

        encoder.write_all(header.as_bytes())?;
        encoder.write_all(data)?;
        encoder.write_all(&[0; 512][..512 - data.len()])?;
        encoder.write_all(&[0; 1024])?;
        encoder.finish()?;

        let err =
            extract_targz(&archive_path, &extract_dir).expect_err("path traversal should fail");

        assert!(err.to_string().contains("invalid path"));
        assert!(!temp_dir.path().join("evil").exists());
        Ok(())
    }

    #[test]
    fn extract_zip_rejects_parent_directory_entries() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let archive_path = temp_dir.path().join("malicious.zip");
        let extract_dir = temp_dir.path().join("extract");
        fs::create_dir(&extract_dir)?;

        let file = File::create(&archive_path)?;
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file("../evil", SimpleFileOptions::default())?;
        writer.write_all(b"outside")?;
        writer.finish()?;

        let err = extract_zip(&archive_path, &extract_dir).expect_err("path traversal should fail");

        assert!(err.to_string().contains("invalid path"));
        assert!(!temp_dir.path().join("evil").exists());
        Ok(())
    }
}
