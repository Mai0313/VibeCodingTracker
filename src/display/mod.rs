//! Output renderers for the `analysis` and `usage` views.
//!
//! Each view has its own submodule ([`analysis`], [`usage`]) holding the four
//! output modes (TUI / table / text / JSON), while [`common`] gathers the
//! rendering glue both views share.

pub mod analysis;
pub mod common;
pub mod fetch;
pub mod usage;
