//! Process-wide LRU cache of parsed session analyses.
//!
//! Wraps [`FileParseCache`] in a global singleton ([`GLOBAL_FILE_CACHE`]) so
//! every command shares one bounded, mtime-invalidated cache rather than
//! reparsing the same JSONL files.

mod file_cache;

pub use file_cache::{CacheStats, FileParseCache};

use once_cell::sync::Lazy;

/// Global singleton cache shared across all application commands.
///
/// Ensures consistent caching behavior and prevents duplicate memory usage.
pub static GLOBAL_FILE_CACHE: Lazy<FileParseCache> = Lazy::new(FileParseCache::new);

/// Returns a reference to the global file parse cache.
pub fn global_cache() -> &'static FileParseCache {
    &GLOBAL_FILE_CACHE
}

/// Clears the global cache (primarily for testing).
pub fn clear_global_cache() {
    GLOBAL_FILE_CACHE.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_exists() {
        // Test that global cache can be accessed
        let cache = global_cache();
        let _stats = cache.stats();

        // Stats should be accessible (entry_count is usize, always >= 0)
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_singleton() {
        // Test that global_cache returns the same instance
        let cache1 = global_cache();
        let cache2 = global_cache();

        // Should be the same instance (same memory address)
        assert!(std::ptr::eq(cache1, cache2));
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_clear() {
        // Test clearing global cache
        let cache = global_cache();

        // Add some entries to cache (if possible)
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");
        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"clear"}}}}"#
        )
        .unwrap();
        drop(file);

        // Try to cache it
        let _ = cache.get_or_parse(&file_path);

        // Clear cache
        clear_global_cache();

        // Cache should be cleared
        let stats_after = cache.stats();
        assert_eq!(stats_after.entry_count, 0);
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_persistence_across_calls() {
        // Test that cache persists across function calls
        let cache = global_cache();
        clear_global_cache();

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");
        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"persistent"}}}}"#
        )
        .unwrap();
        drop(file);

        // First access
        cache.get_or_parse(&file_path).unwrap();
        let stats1 = cache.stats();
        let count1 = stats1.entry_count;

        // Second access (should use cache)
        cache.get_or_parse(&file_path).unwrap();
        let stats2 = cache.stats();

        // Entry count should be the same (used cached value)
        assert_eq!(stats2.entry_count, count1);
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_stats() {
        // Test that cache stats are accessible
        clear_global_cache();
        let cache = global_cache();

        let stats = cache.stats();

        // Should have valid stats (entry_count is 0 after clear)
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    #[serial(global_cache)]
    fn test_clear_global_cache_multiple_times() {
        // Test that clearing cache multiple times works
        clear_global_cache();
        clear_global_cache();
        clear_global_cache();

        let cache = global_cache();
        let stats = cache.stats();

        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_thread_safety() {
        // Test that global cache can be accessed from multiple threads
        use std::thread;

        clear_global_cache();

        let handles: Vec<_> = (0..5)
            .map(|_| {
                thread::spawn(|| {
                    let cache = global_cache();
                    let _ = cache.stats();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Should still be accessible
        let cache = global_cache();
        let _ = cache.stats();
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_with_operations() {
        // Test global cache with actual operations
        clear_global_cache();
        let cache = global_cache();

        let dir = tempdir().unwrap();

        // Create test files
        let file1 = dir.path().join("test1.jsonl");
        let mut f = File::create(&file1).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"one"}}}}"#
        )
        .unwrap();
        drop(f);

        let file2 = dir.path().join("test2.jsonl");
        let mut f = File::create(&file2).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"two"}}}}"#
        )
        .unwrap();
        drop(f);

        // Parse files
        cache.get_or_parse(&file1).unwrap();
        cache.get_or_parse(&file2).unwrap();

        let stats = cache.stats();
        assert!(stats.entry_count >= 2);
    }
}
