//! Renderers for the per-model file-operation / tool-call `analysis` view.
//!
//! Re-exports the three output modes (interactive TUI / table / text) plus the
//! per-provider total helpers in `averages` shared across them.

mod averages;
mod interactive;
mod table;
mod text;

pub use averages::*;
pub(crate) use interactive::{AnalysisUiState, scan_analysis};
pub use table::display_analysis_table;
pub use text::display_analysis_text;
