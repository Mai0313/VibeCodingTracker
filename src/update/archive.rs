use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::path::Path;
use tar::Archive;
use zip::ZipArchive;

/// Extract tar.gz archive and return the path to the binary
pub fn extract_targz(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    println!("📦 Extracting archive...");

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

/// Extract zip archive and return the path to the binary
pub fn extract_zip(archive_path: &Path, extract_to: &Path) -> Result<std::path::PathBuf> {
    println!("📦 Extracting archive...");

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

/// Find the binary in the extracted directory
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

            println!("✅ Extracted binary: {}", binary_path.display());
            return Ok(binary_path);
        }
    }

    anyhow::bail!("Binary not found in archive")
}
