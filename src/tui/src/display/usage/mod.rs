//! Renderers for the per-model token-usage and cost view.
//!
//! The priced, sorted [`UsageSummary`] comes from core; `averages` re-exports it and adds display-only provider-total rows. `interactive`, `table`, and `text` render that summary as the Usage tab, a static table, or one line per model; `usage --json` is priced in core and bypasses this module. `quota_tab` renders the Quota tab using the quota card builders in `interactive`.

mod averages;
mod interactive;
mod quota_tab;
mod table;
mod text;

pub use averages::*;
pub use interactive::UsageFrameBenchmark;
pub(crate) use interactive::{
    QuotaRuntime, QuotaSnapshots, QuotaView, UsageRefreshPayload, UsageScanner, UsageSort,
    UsageTableColumns, UsageUiState, build_usage_detail_sections, build_usage_table_row,
    layout_usage_columns, provider_digest, provider_share_bar, render_provider_pane,
    render_provider_panel,
};
#[cfg(test)]
pub(crate) use quota_tab::draw_quota_tab;
pub(crate) use quota_tab::draw_quota_tab_with_border;
pub use table::display_usage_table;
pub use text::display_usage_text;
