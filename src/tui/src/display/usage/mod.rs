//! Renderers for the per-model token-usage + cost view.
//!
//! The priced, sorted [`UsageSummary`] is built in core; `averages` re-exports
//! it and adds the display-only provider-total rows. `interactive`, `table` and
//! `text` render that summary as the app's Usage tab, a static table, or
//! one line per model respectively. `usage --json` is priced in core and never
//! reaches this module. `quota_tab` is the app's Quota tab, here because it
//! draws the usage view's quota cards.

mod averages;
mod interactive;
mod quota_tab;
mod table;
mod text;

pub use averages::*;
pub use interactive::UsageFrameBenchmark;
#[cfg(test)]
pub(crate) use interactive::UsageSort;
pub(crate) use interactive::{
    QuotaRuntime, QuotaSnapshots, QuotaView, UsageRefreshPayload, UsageScanner, UsageTableColumns,
    UsageUiState, build_usage_detail_sections, build_usage_table_row, layout_usage_columns,
    provider_digest, render_provider_pane, render_provider_panel,
};
pub(crate) use quota_tab::{draw_quota_tab, draw_quota_tab_with_border};
pub use table::display_usage_table;
pub use text::display_usage_text;
