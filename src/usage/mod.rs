//! Token-usage aggregation across provider session directories.
//!
//! Rolls parsed [`CodeAnalysis`](crate::models::CodeAnalysis) records up into
//! [`UsageData`] for the `usage` view. The single entry point is
//! [`get_usage_from_directories`]; both are re-exported at the crate root.

pub mod calculator;
pub mod priced;

pub use calculator::*;
pub use priced::{PricedUsageRow, price_usage_data};
// The token-merge helpers moved to `utils`; keep the historical
// `usage::normalize_usage_value` path working for the CLI and library callers.
pub use crate::utils::normalize_usage_value;
