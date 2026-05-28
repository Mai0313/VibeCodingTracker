//! Token-usage aggregation across provider session directories.
//!
//! Rolls parsed [`CodeAnalysis`](crate::models::CodeAnalysis) records up into
//! [`UsageData`] for the `usage` view. The single entry point is
//! [`get_usage_from_directories`]; both are re-exported at the crate root.

pub mod calculator;

pub use calculator::*;
