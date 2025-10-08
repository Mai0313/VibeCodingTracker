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
    let cache_dir = home_dir.join(".vibe-coding-tracker");

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
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;
    let cache_dir = home_dir.join(".vibe-coding-tracker");

    // Create directory if it doesn't exist
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}
