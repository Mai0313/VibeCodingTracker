//! Collection and projection of normalized analysis sessions.
//!
//! Provider-specific parsing stays in [`crate::session`]. This module collects
//! those [`crate::models::CodeAnalysis`] values into the canonical batch JSON
//! dataset, then projects the same values into the compact summaries rendered
//! by the TUI, text, and table views.
pub mod aggregator;

pub use aggregator::*;
