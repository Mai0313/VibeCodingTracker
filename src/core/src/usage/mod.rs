//! Token-usage aggregation across provider session directories.
//!
//! Rolls parsed [`CodeAnalysis`](crate::models::CodeAnalysis) records up into
//! [`UsageData`] for the `usage` view. [`aggregate_usage_from_home`] is the
//! home-resolved entry point and [`aggregate_usage_from_paths`] its
//! test/injection twin; [`scan_usage_priced`] wraps the pricing-then-scan
//! pipeline, [`price_usage_data`] builds the priced JSON payload, and
//! [`summary`] builds the aggregated view the display renders.

pub mod aggregator;
pub mod pipeline;
pub mod priced;
pub mod summary;

pub use aggregator::*;
pub use pipeline::{PricedUsageScan, scan_usage_priced};
pub use priced::{PricedUsageRow, price_usage_data};
// Shared merged-cost resolver used by both the JSON payload and the display
// summaries.
pub(crate) use priced::resolve_merged_model_cost;
// The token-merge helpers moved to `utils`; keep the historical
// `usage::normalize_usage_value` path working for the CLI and library callers.
pub use crate::utils::normalize_usage_value;
