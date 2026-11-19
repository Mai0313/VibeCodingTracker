//! Compile-time sizing hints, I/O buffer sizes, refresh cadence, and non-cryptographic hash aliases.
//!
//! Values in [`capacity`] and [`buffer`] are pre-allocation hints rather than hard limits, except [`capacity::FILE_CACHE_SIZE`], which bounds the LRU cache.

/// Non-cryptographic hash map backed by [`ahash::AHashMap`].
///
/// Intended only for trusted, process-local keys; do not use for untrusted input that requires HashDoS resistance.
pub type FastHashMap<K, V> = ahash::AHashMap<K, V>;

/// Non-cryptographic hash set backed by [`ahash::AHashSet`]; see [`FastHashMap`] for key-trust assumptions.
pub type FastHashSet<T> = ahash::AHashSet<T>;

/// Pre-allocation capacity hints to minimize reallocation overhead.
pub mod capacity {
    /// Expected number of AI models per conversation session.
    pub const MODELS_PER_SESSION: usize = 3;

    /// Expected number of unique dates in usage tracking.
    pub const DATES_IN_USAGE: usize = 30;

    /// Expected number of unique models in batch analysis.
    pub const MODEL_COMBINATIONS: usize = 20;

    /// Expected number of session files per directory.
    pub const SESSION_FILES: usize = 50;

    /// Maximum number of parsed files held in [`crate::cache::FileParseCache`].
    pub const FILE_CACHE_SIZE: usize = 5;

    /// Expected number of token fields per usage entry.
    pub const TOKEN_FIELDS: usize = 8;
}

/// Buffer sizes for I/O operations.
pub mod buffer {
    /// File read buffer size in bytes (128 KiB, tuned for throughput).
    pub const FILE_READ_BUFFER: usize = 128 * 1024;

    /// Estimated average JSONL line size in bytes, used to pre-size line
    /// capacity when reading sessions.
    pub const AVG_JSONL_LINE_SIZE: usize = 500;
}

/// TUI refresh cadences.
pub mod refresh {
    /// Process CPU/memory sampling and summary-bar redraw interval, in milliseconds.
    pub const METRICS_REFRESH_MS: u64 = 2000;
}
