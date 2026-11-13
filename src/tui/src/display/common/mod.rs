//! Rendering helpers shared by the `analysis` and `usage` views.
//!
//! Groups the per-provider totals containers ([`averages`], [`provider`]), the
//! frame layout and comfy-table / ratatui cell and table builders ([`table`]),
//! the palette ([`mod@theme`]), and the TUI scaffolding ([`tui`]: terminal setup, the input event loop, and
//! refresh / row-highlight state). All items are re-exported at this module's
//! root so callers reach them as `crate::display::common::<item>`.

pub mod averages;
pub mod provider;
pub mod table;
pub mod theme;
pub mod tui;

pub use averages::*;
pub use provider::*;
pub use table::*;
pub use theme::{Theme, theme};
pub use tui::*;
