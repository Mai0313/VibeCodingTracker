//! Shared session-file parsing layer.
//!
//! Every supported provider writes its session history to disk in a
//! provider-specific JSON, JSONL, or SQLite shape.
//! This module owns the "turn raw bytes into a typed
//! [`crate::CodeAnalysis`]" boundary so both of the features that consume
//! session files — [`crate::analysis`] (aggregated tool-call metrics) and
//! [`crate::usage`] (aggregated token counts) — share the same parsers
//! and intermediate shape instead of one feature reaching into the other.
//!
//! Naming convention: the file-backed providers expose `parse_*` entry points
//! (`parse_session_file_*`), while the SQLite-backed providers (OpenCode /
//! Cursor / Hermes) expose `read_*` entry points, since they query a database
//! rather than parse a byte stream.
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod detector;
pub(crate) mod diagnostics;
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
