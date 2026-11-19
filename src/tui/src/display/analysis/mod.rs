//! Renderers and TUI state for the per-model file-operation and tool-call `analysis` view.
//!
//! Re-exports static table and plain-text renderers, shared per-provider total helpers from `averages`, and crate-internal TUI state from `interactive`.

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
