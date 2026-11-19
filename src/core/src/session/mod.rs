//! Session parsing layer that normalizes provider logs and databases into [`crate::CodeAnalysis`].
//!
//! File-backed providers (JSON, JSONL, and zstd-framed JSONL for DeepSeek Harness) expose `parse_*` entry points; SQLite-backed providers (OpenCode, Cursor, Hermes) expose `read_*` entry points.
pub(crate) mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod detector;
pub(crate) mod diagnostics;
pub(crate) mod dsh;
pub mod gemini;
pub mod grok;
pub mod hermes;
pub mod opencode;
pub mod parser;
pub(crate) mod sqlite;
pub mod state;

pub use cursor::{read_cursor_analysis, read_cursor_usage};
pub use detector::{classify_records, detect_extension_type};
pub use hermes::read_hermes_usage;
pub use opencode::{read_opencode_analysis, read_opencode_usage};
pub use parser::{
    SessionFileParseDiagnostics, parse_session_file_to_value, parse_session_file_typed,
    parse_session_file_typed_as, parse_session_file_typed_with_mode,
    parse_session_file_with_diagnostics,
};
pub use state::{ParseMode, SessionParseState};
