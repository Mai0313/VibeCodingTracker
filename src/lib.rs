// Main library modules
pub mod analysis;
pub mod cli;
pub mod models;
pub mod usage;
pub mod utils;

// Re-export commonly used types
pub use analysis::analyzer::analyze_jsonl_file;
pub use models::*;
pub use usage::calculator::get_usage_from_directories;

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub const PKG_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

/// Get version info struct
pub fn get_version_info() -> VersionInfo {
    VersionInfo {
        version: VERSION.to_string(),
        build_time: option_env!("BUILD_TIME").unwrap_or("unknown").to_string(),
        git_commit: option_env!("GIT_COMMIT").unwrap_or("unknown").to_string(),
    }
}

/// Version information structure
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub build_time: String,
    pub git_commit: String,
}
