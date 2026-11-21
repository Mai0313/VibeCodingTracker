//! Token-usage aggregation, pricing, and summary projection across all providers.
//!
//! Rolls parsed [`CodeAnalysis`](crate::models::CodeAnalysis) records into [`UsageData`] via [`aggregate_usage_from_home`] or [`aggregate_usage_from_paths`]. [`scan_usage_priced`] combines pricing lookup with scanning, [`price_usage_data`] builds the priced JSON payload, and [`summary`] builds the sorted table and per-provider totals.

pub mod aggregator;
pub mod pipeline;
pub mod priced;
pub mod summary;

pub use aggregator::*;
pub use pipeline::{
    PricedDailyUsageScan, PricedUsageScan, scan_daily_usage_priced, scan_usage_priced,
};
pub(crate) use priced::resolve_merged_model_breakdown;
pub use priced::{
    DailyPricedUsageRow, DailyUsageDay, DailyUsageOutput, PricedUsageRow, merge_priced_usage_rows,
    price_daily_usage_data, price_usage_data,
};
// Re-exported from `utils` to preserve the `usage::normalize_usage_value` public path.
pub use crate::utils::normalize_usage_value;
