//! Archive extraction for downloaded release assets.
//!
//! Unpacks `.tar.gz` and `.zip` release archives into a destination directory and locates the `vibe_coding_tracker` or `vct` binary inside it. Every entry's target path is validated to remain within the extraction directory to prevent path traversal (Zip Slip).

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tar::Archive;
use zip::ZipArchive;

/// Extracts a `.tar.gz` archive into `extract_to` and returns the path to the extracted binary.
///
/// Refuses any entry whose path escapes `extract_to`, aborting extraction.
///
/// # Errors
///
/// Returns an error if the archive cannot be opened or read, if any entry escapes `extract_to`, if unpacking fails, or if neither `vibe_coding_tracker` nor `vct` exists directly under `extract_to`.
pub fn extract_targz(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    let tar_gz = File::open(archive_path).context("Failed to open archive file")?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

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

    find_binary_in_directory(extract_to, &["vibe_coding_tracker", "vct"])
}

/// Extracts a `.zip` archive into `extract_to` and returns the path to the extracted binary.
///
/// Validates every entry's path against `extract_to` before writing; an escaping entry aborts extraction.
///
/// # Errors
///
/// Returns an error if the archive cannot be opened or read, if any entry escapes `extract_to`, if creating or writing an extracted path fails, or if neither `vibe_coding_tracker.exe` nor `vct.exe` exists directly under `extract_to`.
pub fn extract_zip(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    let file = File::open(archive_path).context("Failed to open archive file")?;
    let mut archive = ZipArchive::new(file).context("Failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let file_path = file
            .name()
            .context("Failed to read zip entry name")?
            .to_string();
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

    find_binary_in_directory(extract_to, &["vibe_coding_tracker.exe", "vct.exe"])
}

/// Returns the first matching binary directly under `extract_to`, setting mode `0o755` on Unix.
fn find_binary_in_directory(extract_to: &Path, binary_names: &[&str]) -> Result<PathBuf> {
    for name in binary_names {
        let binary_path = extract_to.join(name);
        if binary_path.exists() {
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
