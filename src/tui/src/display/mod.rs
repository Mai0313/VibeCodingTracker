//! Terminal output renderers and the interactive TUI app.
//!
//! [`app`] runs the tabbed interactive TUI, [`analysis`] and [`usage`] provide each view's TUI state plus static table and plain-text renderers (JSON is serialized by the CLI), [`quota`] renders one-shot `vct quota` responses, and [`common`] holds shared rendering utilities.

pub mod analysis;
pub mod app;
pub mod common;
pub mod quota;
pub(crate) mod settings;
pub mod usage;
