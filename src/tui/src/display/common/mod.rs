//! Rendering helpers shared across display views.
//!
//! Groups per-provider totals ([`averages`], [`provider`]), frame layout and comfy-table/ratatui builders ([`table`]), the color palette ([`mod@theme`]), and terminal/event/refresh scaffolding ([`tui`]). Most items are re-exported at this module root.

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
