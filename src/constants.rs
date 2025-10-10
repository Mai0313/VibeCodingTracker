pub type FastHashMap<K, V> = ahash::AHashMap<K, V>;

/// Capacity hints for various data structures
pub mod capacity {
    /// Typical number of models per conversation session (Claude/Codex/Gemini)
    pub const MODELS_PER_SESSION: usize = 3;

    /// Typical number of date entries in usage tracking
    pub const DATES_IN_USAGE: usize = 30;

    /// Typical number of date-model combinations in analysis
    pub const DATE_MODEL_COMBINATIONS: usize = 100;

    /// Typical number of session files per directory
    pub const SESSION_FILES: usize = 50;

    /// LRU cache size for parsed files (number of entries)
    pub const FILE_CACHE_SIZE: usize = 100;

    /// Initial capacity for token fields in usage data
    pub const TOKEN_FIELDS: usize = 8;
}

/// Buffer sizes for I/O operations
pub mod buffer {
    /// Buffer size for file reading (128KB for better throughput)
    pub const FILE_READ_BUFFER: usize = 128 * 1024;

    /// Estimated average line size for JSONL files
    pub const AVG_JSONL_LINE_SIZE: usize = 500;
}
