//! Per-provider totals container shared by the `usage` and `analysis` views.
//!
//! The container itself is neutral data and now lives in [`crate::models`]; it
//! is re-exported here so display code keeps reaching it as
//! `crate::display::common::ProviderTotals`.

pub use crate::models::ProviderTotals;
