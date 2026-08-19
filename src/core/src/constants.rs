//! Compile-time sizing knobs: pre-allocation capacities, I/O buffer sizes, and
//! the TUI metrics cadence.
//!
//! The capacities and buffer sizes are best-effort hints to size collections
//! and buffers up front so the hot paths reallocate less; they are not hard
//! limits except for [`capacity::FILE_CACHE_SIZE`], which bounds the LRU.

/// Hash map backed by `ahash` for fast, non-cryptographic hashing.
///
/// Used in place of `std::collections::HashMap` on the hot aggregation paths;
/// every key is process-local rather than attacker-controlled, so DoS
/// resistance is not required.
pub type FastHashMap<K, V> = ahash::AHashMap<K, V>;

/// Hash set backed by `ahash`, replacing `std::collections::HashSet` on the
/// same paths and the same grounds as [`FastHashMap`].
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

    /// Maximum number of parsed files held in the LRU file cache.
    ///
    /// Deliberately small: every entry holds an `Arc<CodeAnalysis>`, and only
    /// library callers reach this cache; the CLI and TUI scan paths use the
    /// compact [`crate::summary_cache::SummaryScanCache`] instead.
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
    /// CPU/memory sampling + redraw cadence for the summary bar.
    ///
    /// Sampling our own process stats and repainting cached rows is nearly
    /// free, so this runs far more often than the session-aggregation refresh
    /// it is decoupled from.
    pub const METRICS_REFRESH_MS: u64 = 2000;
}
