mod file_cache;

pub use file_cache::{CacheStats, FileParseCache};

use once_cell::sync::Lazy;

/// Global singleton file parse cache
/// This ensures all parts of the application share the same cache
pub static GLOBAL_FILE_CACHE: Lazy<FileParseCache> = Lazy::new(FileParseCache::new);

/// Get a reference to the global file parse cache
pub fn global_cache() -> &'static FileParseCache {
    &GLOBAL_FILE_CACHE
}

/// Clear the global cache (useful for testing)
pub fn clear_global_cache() {
    GLOBAL_FILE_CACHE.clear();
}
