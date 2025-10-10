pub mod analysis;
pub mod cache;
pub mod cli;
pub mod constants;
pub mod display;
pub mod models;
pub mod pricing;
pub mod update;
pub mod usage;
pub mod utils;

pub use analysis::analyzer::analyze_jsonl_file;
pub use models::*;
pub use usage::calculator::get_usage_from_directories;

pub const VERSION: &str = env!("BUILD_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub const PKG_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
pub const RUST_VERSION: &str = env!("BUILD_RUST_VERSION");
pub const CARGO_VERSION: &str = env!("BUILD_CARGO_VERSION");

/// Returns the version information including binary version, Rust toolchain, and Cargo version
pub fn get_version_info() -> VersionInfo {
    VersionInfo {
        version: VERSION.to_string(),
        rust_version: RUST_VERSION.to_string(),
        cargo_version: CARGO_VERSION.to_string(),
    }
}

/// Version information structure containing build metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub rust_version: String,
    pub cargo_version: String,
}
