//! Terminal presentation layer for the `vct` CLI.
//!
//! Renders [`vct_core`] roll-ups as an interactive ratatui TUI, static comfy-table output, plain text, or quota panels, keeping terminal dependencies out of `vct_core`.

#[doc(hidden)]
pub mod bench_support;
pub mod display;
pub(crate) mod layouts;
