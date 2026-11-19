//! Collection and projection of normalized analysis sessions.
//!
//! Provider-specific parsing stays in [`crate::session`]. This module collects [`crate::models::CodeAnalysis`] records into the canonical batch JSON dataset and projects them into compact per-model and per-provider summaries.
pub mod aggregator;
pub mod summary;

pub use aggregator::*;
pub use summary::*;
