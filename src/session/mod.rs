//! Shared session-file parsing layer.
//!
//! Every supported provider (Claude Code, Codex, Copilot CLI, Gemini CLI)
//! writes its session history to disk in a provider-specific JSONL shape.
//! This module owns the "turn raw bytes into a typed
//! [`crate::CodeAnalysis`]" boundary so both of the features that consume
//! session files — [`crate::analysis`] (aggregated tool-call metrics) and
//! [`crate::usage`] (aggregated token counts) — share the same parsers
//! and intermediate shape instead of one feature reaching into the other.
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod detector;
pub mod gemini;
pub mod parser;
pub mod state;

pub use detector::{classify_records, detect_extension_type};
pub use parser::{
    parse_session_file, parse_session_file_as, parse_session_file_typed,
    parse_session_file_typed_with_mode,
};
pub use state::{ParseMode, SessionParseState};
