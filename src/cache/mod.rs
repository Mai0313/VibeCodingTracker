mod file_cache;

pub use file_cache::{CacheStats, FileParseCache};

use once_cell::sync::Lazy;

/// Global singleton cache shared across all application commands
///
/// Ensures consistent caching behavior and prevents duplicate memory usage.
pub static GLOBAL_FILE_CACHE: Lazy<FileParseCache> = Lazy::new(FileParseCache::new);

/// Returns a reference to the global file parse cache
pub fn global_cache() -> &'static FileParseCache {
    &GLOBAL_FILE_CACHE
}

/// Clears the global cache (primarily for testing)
pub fn clear_global_cache() {
    GLOBAL_FILE_CACHE.clear();
}
