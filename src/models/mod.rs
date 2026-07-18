//! Serde data models mirroring the supported assistants' on-disk session
//! formats plus the analyzer's own aggregated result types.
//!
//! Each provider submodule defines the
//! minimal subset of fields the analyzer reads from that provider's session
//! logs; `analysis` and `usage` hold the normalized, cross-provider output
//! shapes; `provider` carries the [`Provider`] discriminator. All items are
//! re-exported at the module root for convenience.

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
