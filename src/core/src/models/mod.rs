//! Provider session serde models and normalized cross-provider output types.
//!
//! Only JSON/JSONL providers (`claude`, `codex`, `copilot`, `gemini`, `grok`) define wire models here; SQLite providers, DeepSeek Harness, and Antigravity decode directly in `session`. All submodules are re-exported at the module root.

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
