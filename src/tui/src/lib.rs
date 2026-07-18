//! Terminal presentation layer for the `vct` CLI.
//!
//! Renders the [`vibe_coding_tracker`] core roll-ups as an interactive
//! ratatui TUI, a static comfy-table, plain text, or the quota panels. Depends
//! only on the core crate, so the core stays free of any terminal dependency
//! and a future GUI can reuse the core without pulling ratatui in.

pub mod display;
