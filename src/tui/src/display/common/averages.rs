//! Per-provider totals container shared by the `usage` and `analysis` views.
//!
//! The container itself is neutral data and now lives in [`vibe_coding_tracker::models`]; it
//! is re-exported here so display code keeps reaching it as
//! `crate::display::common::ProviderTotals`.

pub use vibe_coding_tracker::models::ProviderTotals;
