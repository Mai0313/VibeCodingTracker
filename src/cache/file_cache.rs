use crate::constants::capacity;
use anyhow::Result;
use lru::LruCache;
use serde_json::Value;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Cached parsed file with metadata
#[derive(Debug, Clone)]
struct CachedFile {
    /// Last modified time of the file when it was parsed
    modified: SystemTime,
    /// Parsed analysis result (shared via Arc for zero-cost cloning)
    analysis: Arc<Value>,
}

/// Global file parsing cache to avoid redundant I/O and JSON parsing
/// Uses LRU eviction to prevent unbounded memory growth
/// This is the single source of truth for all file parsing operations
pub struct FileParseCache {
    /// LRU cache: file_path -> cached_data
    /// Automatically evicts least recently used entries when capacity is reached
    cache: RwLock<LruCache<PathBuf, CachedFile>>,
}

impl FileParseCache {
    /// Create a new empty LRU cache with bounded capacity
    pub fn new() -> Self {
        // SAFETY: FILE_CACHE_SIZE is a const > 0
        let cache_size = NonZeroUsize::new(capacity::FILE_CACHE_SIZE).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cache_size)),
        }
    }

    /// Get or parse a file with caching based on modification time
    ///
    /// This function:
    /// 1. Checks if the file exists in LRU cache and hasn't been modified
    /// 2. If yes, returns the cached Arc<Value> (zero-cost clone) and promotes entry
    /// 3. If no, parses the file and caches the result (may evict LRU entry)
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<Value>> {
        let path = path.as_ref();

        // Get file metadata (modification time)
        let metadata = fs::metadata(path)?;
        let modified = metadata.modified()?;

        // Try to get from cache (read lock first for concurrent reads)
        {
            if let Ok(mut cache_write) = self.cache.write() {
                // LRU.get() requires &mut self (promotes entry to front)
                if let Some(cached) = cache_write.get(&path.to_path_buf()) {
                    // Check if the cached version is still valid
                    if cached.modified >= modified {
                        log::trace!("LRU cache hit for {}", path.display());
                        return Ok(Arc::clone(&cached.analysis));
                    }
                }
            }
        }

        // Cache miss or outdated - need to parse
        log::debug!("LRU cache miss for {}, parsing...", path.display());
        let analysis = crate::analysis::analyze_jsonl_file(path)?;
        let arc_analysis = Arc::new(analysis);

        // Update cache (write lock) - LRU will auto-evict if at capacity
        if let Ok(mut cache_write) = self.cache.write() {
            cache_write.put(
                path.to_path_buf(),
                CachedFile {
                    modified,
                    analysis: Arc::clone(&arc_analysis),
                },
            );
        }

        Ok(arc_analysis)
    }

    /// Clear all cached entries
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// Remove stale entries (files that no longer exist)
    /// Note: With LRU cache, stale entries will naturally be evicted over time
    pub fn cleanup_stale(&self) {
        if let Ok(mut cache) = self.cache.write() {
            // LRU cache doesn't have retain(), so we collect keys first
            let stale_keys: Vec<PathBuf> = cache
                .iter()
                .filter(|(path, _)| !path.exists())
                .map(|(path, _)| path.clone())
                .collect();

            for key in stale_keys {
                cache.pop(&key);
            }
        }
    }

    /// Get cache statistics (for monitoring)
    pub fn stats(&self) -> CacheStats {
        if let Ok(cache) = self.cache.write() {
            CacheStats {
                entry_count: cache.len(),
                estimated_memory_kb: cache.len() * 50, // Rough estimate: ~50KB per entry
            }
        } else {
            CacheStats::default()
        }
    }

    /// Invalidate a specific file in the cache
    pub fn invalidate<P: AsRef<Path>>(&self, path: P) {
        if let Ok(mut cache) = self.cache.write() {
            cache.pop(&path.as_ref().to_path_buf());
        }
    }

    /// Get all cached file paths
    pub fn get_cached_paths(&self) -> Vec<PathBuf> {
        if let Ok(cache) = self.cache.write() {
            cache.iter().map(|(path, _)| path.clone()).collect()
        } else {
            Vec::new()
        }
    }
}

impl Default for FileParseCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about cache usage
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    pub entry_count: usize,
    pub estimated_memory_kb: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let cache = FileParseCache::new();
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    fn test_cache_clear() {
        let cache = FileParseCache::new();
        cache.clear();
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
    }
}
