//! Output renderers for the `analysis` and `usage` views.
//!
//! Each view has its own submodule ([`analysis`], [`usage`]) holding its three
//! terminal output modes (TUI / table / text); the CLI serializes the JSON mode
//! itself. [`quota`] renders the one-shot `vct quota` response, and [`common`]
//! gathers the rendering glue both views share.

pub mod analysis;
pub mod common;
pub mod quota;
pub mod usage;
