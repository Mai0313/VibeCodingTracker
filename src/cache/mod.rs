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
        let cache = global_cache();
        let _stats = cache.stats();
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_singleton() {
        let cache1 = global_cache();
        let cache2 = global_cache();

        assert!(std::ptr::eq(cache1, cache2));
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_clear() {
        let cache = global_cache();

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, r#"{{"key": "value"}}"#).unwrap();
        drop(file);

        let _ = cache.get_or_parse(&file_path);

        clear_global_cache();

        let stats_after = cache.stats();
        assert_eq!(stats_after.entry_count, 0);
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_persistence_across_calls() {
        let cache = global_cache();
        clear_global_cache();

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, r#"{{"key": "value"}}"#).unwrap();
        drop(file);

        let _ = cache.get_or_parse(&file_path);
        let stats1 = cache.stats();
        let count1 = stats1.entry_count;

        let _ = cache.get_or_parse(&file_path);
        let stats2 = cache.stats();

        assert_eq!(stats2.entry_count, count1);
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_stats() {
        clear_global_cache();
        let cache = global_cache();

        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    #[serial(global_cache)]
    fn test_clear_global_cache_multiple_times() {
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

        let cache = global_cache();
        let _ = cache.stats();
    }

    #[test]
    #[serial(global_cache)]
    fn test_global_cache_with_operations() {
        clear_global_cache();
        let cache = global_cache();

        let dir = tempdir().unwrap();

        let file1 = dir.path().join("test1.jsonl");
        let mut f = File::create(&file1).unwrap();
        writeln!(f, r#"{{"a": 1}}"#).unwrap();
        drop(f);

        let file2 = dir.path().join("test2.jsonl");
        let mut f = File::create(&file2).unwrap();
        writeln!(f, r#"{{"b": 2}}"#).unwrap();
        drop(f);

        let _ = cache.get_or_parse(&file1);
        let _ = cache.get_or_parse(&file2);

        let stats = cache.stats();
        assert!(stats.entry_count >= 2);
    }
}
