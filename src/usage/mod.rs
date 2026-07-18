//! Token-usage aggregation across provider session directories.
//!
//! Rolls parsed [`CodeAnalysis`](crate::models::CodeAnalysis) records up into
//! [`UsageData`] for the `usage` view. The single entry point is
//! [`get_usage_from_directories`]; both are re-exported at the crate root.

pub mod calculator;
pub mod pipeline;
pub mod priced;
pub mod summary;

pub use calculator::*;
pub use pipeline::{PricedUsageScan, scan_usage_priced};
pub use priced::{PricedUsageRow, price_usage_data};
// Shared merged-cost resolver used by both the JSON payload and the display
// summaries.
pub(crate) use priced::resolve_merged_model_cost;
// The token-merge helpers moved to `utils`; keep the historical
// `usage::normalize_usage_value` path working for the CLI and library callers.
pub use crate::utils::normalize_usage_value;
