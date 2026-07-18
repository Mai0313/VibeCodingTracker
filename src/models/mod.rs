//! Serde data models mirroring the supported assistants' on-disk session
//! formats plus the analyzer's own aggregated result types.
//!
//! Each JSON/JSONL provider submodule (`claude`, `codex`, `copilot`, `gemini`,
//! `grok`) defines the minimal subset of fields the analyzer reads from that
//! provider's session logs; the SQLite providers (OpenCode / Cursor / Hermes)
//! deserialize inline in their `session` readers and have no submodule here.
//! `analysis` and `usage` hold the normalized, cross-provider output shapes;
//! `provider` carries the [`Provider`] discriminator, `filter` the
//! [`TimeRange`] session filter, and `aggregate` the per-provider totals
//! container. All items are re-exported at the module root for convenience.

pub mod aggregate;
pub mod analysis;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod filter;
pub mod gemini;
pub mod grok;
pub mod provider;
pub mod quota;
pub mod usage;

pub use self::aggregate::*;
pub use self::analysis::*;
pub use self::claude::*;
pub use self::codex::*;
pub use self::copilot::*;
pub use self::filter::*;
pub use self::gemini::*;
pub use self::grok::*;
pub use self::provider::*;
pub use self::quota::*;
pub use self::usage::*;
