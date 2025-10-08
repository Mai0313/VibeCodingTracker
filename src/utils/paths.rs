use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Helper paths for application directories
#[derive(Debug, Clone)]
pub struct HelperPaths {
    pub home_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub codex_session_dir: PathBuf,
    pub claude_dir: PathBuf,
    pub claude_session_dir: PathBuf,
    pub gemini_dir: PathBuf,
    pub gemini_session_dir: PathBuf,
    pub cache_dir: PathBuf,
}

/// Resolve application paths
pub fn resolve_paths() -> Result<HelperPaths> {
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;

    let codex_dir = home_dir.join(".codex");
    let codex_session_dir = codex_dir.join("sessions");
    let claude_dir = home_dir.join(".claude");
    let claude_session_dir = claude_dir.join("projects");
    let gemini_dir = home_dir.join(".gemini");
    let gemini_session_dir = gemini_dir.join("tmp");
    let cache_dir = home_dir.join(".vibe_coding_tracker");

    Ok(HelperPaths {
        home_dir,
        codex_dir,
        codex_session_dir,
        claude_dir,
        claude_session_dir,
        gemini_dir,
        gemini_session_dir,
        cache_dir,
    })
}

/// Get current user name
pub fn get_current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

// Cache for machine ID (initialized once)
static MACHINE_ID_CACHE: OnceLock<String> = OnceLock::new();

/// Get home directory with error handling
fn get_home_dir() -> Result<PathBuf> {
    home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))
}

/// Get machine ID (cached after first call)
pub fn get_machine_id() -> String {
    MACHINE_ID_CACHE
        .get_or_init(|| {
            // Try to read /etc/machine-id on Linux
            if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
                return id.trim().to_string();
            }

            // Fallback to hostname
            if let Ok(hostname) = hostname::get() {
                if let Some(hostname_str) = hostname.to_str() {
                    return hostname_str.to_string();
                }
            }

            "unknown-machine-id".to_string()
        })
        .clone()
}

/// Get cache directory path (creates it if it doesn't exist)
pub fn get_cache_dir() -> Result<PathBuf> {
    let home_dir = get_home_dir()?;
    let cache_dir = home_dir.join(".vibe_coding_tracker");

    // Create directory if it doesn't exist
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Get pricing cache file path for a specific date
/// Returns: ~/.vibe_coding_tracker/model_pricing_YYYY-MM-DD.json
pub fn get_pricing_cache_path(date: &str) -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    Ok(cache_dir.join(format!("model_pricing_{}.json", date)))
}

/// Find pricing cache file for a specific date
/// Returns Some(path) if the cache file exists, None otherwise
pub fn find_pricing_cache_for_date(date: &str) -> Option<PathBuf> {
    let cache_path = get_pricing_cache_path(date).ok()?;
    if cache_path.exists() {
        Some(cache_path)
    } else {
        None
    }
}

/// List all pricing cache files in the cache directory
/// Returns vector of (filename, PathBuf) tuples
pub fn list_pricing_cache_files() -> Result<Vec<(String, PathBuf)>> {
    let cache_dir = get_cache_dir()?;
    let mut cache_files = Vec::new();

    if let Ok(entries) = fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                // Match pattern: model_pricing_YYYY-MM-DD.json
                if filename.starts_with("model_pricing_") && filename.ends_with(".json") {
                    cache_files.push((filename.to_string(), path));
                }
            }
        }
    }

    Ok(cache_files)
}
