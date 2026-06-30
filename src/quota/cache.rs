//! On-disk cache for the latest Codex quota snapshot
//! (`~/.vibe_coding_tracker/codex_usage.json`).
//!
//! A single last-known-good file (not dated like the pricing cache, since we
//! always want the latest value). A fresh `vct usage` launch seeds the panel
//! from here instantly while the background worker refreshes it.

use crate::models::CodexQuotaSnapshot;
use crate::utils::{get_codex_usage_cache_path, write_json_atomic};
use anyhow::Result;

/// Loads the last-known Codex quota snapshot, or `None` if absent/corrupt.
///
/// A parse failure returns `None` (the caller recomputes) rather than erroring.
pub fn load_codex_cache() -> Option<CodexQuotaSnapshot> {
    let path = get_codex_usage_cache_path().ok()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persists the Codex quota snapshot atomically.
///
/// # Errors
///
/// Returns an error if the cache path cannot be resolved or the write fails.
pub fn save_codex_cache(snap: &CodexQuotaSnapshot) -> Result<()> {
    let path = get_codex_usage_cache_path()?;
    write_json_atomic(&path, snap)
}
