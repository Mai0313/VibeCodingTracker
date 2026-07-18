//! Compile-time sizing knobs: pre-allocation capacities and I/O buffer sizes.
//!
//! These are best-effort hints to size collections and buffers up front so
//! the hot paths reallocate less; they are not hard limits except where noted
//! (e.g. [`capacity::FILE_CACHE_SIZE`], which bounds the LRU).

/// Hash map backed by `ahash` for fast, non-cryptographic hashing.
///
/// Used in place of `std::collections::HashMap` on hot aggregation paths; the
/// keys here are not attacker-controlled, so DoS resistance is not required.
pub type FastHashMap<K, V> = ahash::AHashMap<K, V>;

/// Hash set backed by `ahash` for fast, non-cryptographic hashing.
///
/// Used in place of `std::collections::HashSet` on the incremental scan path,
/// where the keys are process-local (not attacker-controlled).
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
    /// Deliberately small (reduced from 15 to 5) to bound RSS in TUI mode,
    /// where the cache is refreshed repeatedly.
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
    /// Lightweight CPU/memory sampling + redraw cadence for the summary bar,
    /// decoupled from the heavier session-aggregation refresh. Reading our own
    /// process stats and repainting cached rows is nearly free, so this can run
    /// far more often than the data refresh without noticeable overhead.
    pub const METRICS_REFRESH_MS: u64 = 2000;
}
