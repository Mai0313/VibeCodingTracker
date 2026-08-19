//! Per-provider totals container shared by the `usage` and `analysis` views.
//!
//! The container is neutral data, so core owns it; it is re-exported here so
//! display code reaches it as `crate::display::common::ProviderTotals`.

pub use vct_core::models::ProviderTotals;
