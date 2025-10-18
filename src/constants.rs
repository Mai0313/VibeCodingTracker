pub type FastHashMap<K, V> = ahash::AHashMap<K, V>;

/// Pre-allocated capacity constants for data structures to minimize reallocation overhead
pub mod capacity {
    /// Expected number of AI models per conversation session
    pub const MODELS_PER_SESSION: usize = 3;

    /// Expected number of unique dates in usage tracking
    pub const DATES_IN_USAGE: usize = 30;

    /// Expected number of date-model combinations in batch analysis
    pub const DATE_MODEL_COMBINATIONS: usize = 100;

    /// Expected number of session files per directory
    pub const SESSION_FILES: usize = 50;

    /// Maximum number of parsed files to cache in LRU cache
    /// Reduced from 15 to 5 to minimize memory usage in TUI mode
    pub const FILE_CACHE_SIZE: usize = 5;

    /// Expected number of token fields per usage entry
    pub const TOKEN_FIELDS: usize = 8;
}

/// Buffer size constants for optimized I/O operations
pub mod buffer {
    /// File read buffer size in bytes (128KB optimized for throughput)
    pub const FILE_READ_BUFFER: usize = 128 * 1024;

    /// Estimated average size per line in JSONL files for capacity pre-allocation
    pub const AVG_JSONL_LINE_SIZE: usize = 500;
}
