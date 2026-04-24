//! Aggregation of already-parsed session files into per-model metrics.
//!
//! The actual file-parsing logic lives in [`crate::session`] — this module
//! only consumes [`crate::models::CodeAnalysis`] records and rolls them up
//! into the tables the CLI renders.
pub mod batch_analyzer;

pub use batch_analyzer::*;
