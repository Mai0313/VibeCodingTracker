//! Renderers for the per-model token-usage + cost view.
//!
//! `averages` turns a [`UsageData`](crate::usage::UsageData) into the priced,
//! sorted [`UsageSummary`] shared by all output modes;
//! `interactive`, `table`, and `text` render that summary as the
//! auto-refreshing TUI, a static table, or one line per model respectively.

mod averages;
mod interactive;
mod table;
mod text;

pub use averages::*;
pub use interactive::display_usage_interactive;
pub use table::display_usage_table;
pub use text::display_usage_text;
