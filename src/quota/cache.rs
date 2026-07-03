//! On-disk caches for the latest per-provider quota snapshots
//! (`~/.vibe_coding_tracker/{claude,codex}_usage.json`).
//!
//! Each is a single last-known-good file (not dated like the pricing cache,
//! since we always want the latest value). A fresh `vct usage` launch seeds
//! the panels from here instantly while the background workers refresh them.

use crate::models::{ClaudeQuotaSnapshot, CodexQuotaSnapshot};
use crate::utils::{get_claude_usage_cache_path, get_codex_usage_cache_path, write_json_atomic};
use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::PathBuf;

/// Loads and parses a cache file, returning `None` on any error.
fn load_cache<T: DeserializeOwned>(path: Result<PathBuf>) -> Option<T> {
    let body = std::fs::read_to_string(path.ok()?).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persists a snapshot atomically to `path`.
fn save_cache<T: Serialize>(path: Result<PathBuf>, snap: &T) -> Result<()> {
    write_json_atomic(path?, snap)
}

/// Loads the last-known Claude quota snapshot, or `None` if absent/corrupt.
pub fn load_claude_cache() -> Option<ClaudeQuotaSnapshot> {
    load_cache(get_claude_usage_cache_path())
}

/// Persists the Claude quota snapshot atomically.
pub fn save_claude_cache(snap: &ClaudeQuotaSnapshot) -> Result<()> {
    save_cache(get_claude_usage_cache_path(), snap)
}

/// Loads the last-known Codex quota snapshot, or `None` if absent/corrupt.
pub fn load_codex_cache() -> Option<CodexQuotaSnapshot> {
    load_cache(get_codex_usage_cache_path())
}

/// Persists the Codex quota snapshot atomically.
pub fn save_codex_cache(snap: &CodexQuotaSnapshot) -> Result<()> {
    save_cache(get_codex_usage_cache_path(), snap)
}
