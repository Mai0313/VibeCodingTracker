//! Leaf helpers shared across the crate: directory walking, JSON/JSONL file
//! IO, number/date formatting, git remote lookup, glibc heap tuning, path
//! resolution, ISO timestamp parsing, and token-count extraction.
//!
//! The most frequently used items are re-exported at this module's root so
//! callers can write `utils::format_number` instead of reaching into the
//! per-concern submodules.

pub mod directory;
pub mod file;
pub mod format;
pub mod git;
pub mod heap;
pub mod paths;
pub mod time;
pub mod token_extractor;
pub mod usage_processor;

// Public API exports (commonly used across modules)
pub use directory::{
    COPILOT_SESSION_MAX_DEPTH, collect_files_with_dates, collect_files_with_max_depth,
    is_claude_session_file, is_codex_session_file, is_copilot_session_file, is_gemini_session_file,
};
pub use file::{count_lines, read_json, read_jsonl, save_json_pretty};
pub use format::{format_number, get_current_date};
pub use git::get_git_remote_url;
pub use heap::{release_freed_heap, tune_system_allocator};
pub use paths::{
    find_pricing_cache_for_date, get_cache_dir, get_current_user, get_machine_id,
    get_pricing_cache_path, list_pricing_cache_files, resolve_paths,
};
pub use time::parse_iso_timestamp;
pub use token_extractor::extract_token_counts;
pub use usage_processor::{
    accumulate_i64_fields, accumulate_nested_object, process_claude_usage, process_codex_usage,
    process_gemini_usage,
};
