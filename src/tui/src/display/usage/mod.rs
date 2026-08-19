//! Renderers for the per-model token-usage + cost view.
//!
//! `averages` turns a [`UsageData`](vct_core::usage::UsageData) into the priced,
//! sorted [`UsageSummary`]; `interactive`, `table`, and `text` render that
//! summary as the auto-refreshing TUI, a static table, or one line per model
//! respectively. `usage --json` is priced in core and never reaches this
//! module.

mod averages;
mod interactive;
mod table;
mod text;

pub use averages::*;
pub use interactive::{
    UsageFrameBenchmark, display_usage_interactive, display_usage_interactive_with_pool,
};
pub use table::display_usage_table;
pub use text::display_usage_text;
