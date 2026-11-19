//! Renderers for the per-model file-operation / tool-call `analysis` view.
//!
//! Re-exports the three output modes (interactive TUI / table / text) plus the
//! per-provider total helpers in `averages` shared across them.

mod averages;
mod interactive;
mod table;
mod text;

pub use averages::*;
pub(crate) use interactive::{
    AnalysisSort, AnalysisTableColumns, AnalysisUiState, build_analysis_detail_sections,
    build_analysis_table_row, layout_analysis_columns, scan_analysis,
};
pub use table::display_analysis_table;
pub use text::display_analysis_text;
