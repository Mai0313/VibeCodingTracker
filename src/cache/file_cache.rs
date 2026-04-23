use crate::constants::capacity;
use anyhow::Result;
use lru::LruCache;
use serde_json::Value;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Cached file entry with modification time tracking for invalidation.
///
/// `size_bytes` is captured once at insertion via [`estimate_value_bytes`]
/// so the `stats()` path can report a realistic memory footprint without
/// walking the JSON tree on every call.
#[derive(Debug, Clone)]
struct CachedFile {
    modified: SystemTime,
    analysis: Arc<Value>,
    size_bytes: usize,
}

/// Thread-safe LRU cache for parsed session files with automatic eviction
///
/// This cache:
/// - Eliminates redundant file I/O and JSON parsing across commands
/// - Uses LRU eviction to maintain bounded memory usage (max 100 entries)
/// - Tracks file modification times for automatic invalidation
/// - Shares cached results via Arc for zero-cost cloning
pub struct FileParseCache {
    cache: RwLock<LruCache<PathBuf, CachedFile>>,
}

impl FileParseCache {
    /// Creates a new LRU cache with capacity from `constants::capacity::FILE_CACHE_SIZE`
    pub fn new() -> Self {
        // SAFETY: FILE_CACHE_SIZE is a const > 0
        let cache_size = NonZeroUsize::new(capacity::FILE_CACHE_SIZE).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cache_size)),
        }
    }

    /// Retrieves cached analysis or parses the file if needed
    ///
    /// Workflow:
    /// 1. Check cache hit with read-only peek (no lock contention)
    /// 2. If valid, promote entry to front with write lock
    /// 3. If miss/stale, parse file and cache result (may evict LRU entry)
    ///
    /// Optimized to minimize write lock contention in parallel workloads.
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<Value>> {
        let path = path.as_ref();
        let path_buf = path.to_path_buf();

        // Get file metadata (modification time)
        let metadata = fs::metadata(path)?;
        let modified = metadata.modified()?;

        // Fast path: Check cache with read lock (no contention)
        {
            if let Ok(cache_read) = self.cache.read() {
                // Use peek() instead of get() to avoid requiring write lock
                if let Some(cached) = cache_read.peek(&path_buf) {
                    // Check if the cached version is still valid
                    if cached.modified >= modified {
                        log::trace!("LRU cache hit for {}", path.display());
                        let result = Arc::clone(&cached.analysis);
                        // Release read lock before acquiring write lock
                        drop(cache_read);

                        // Promote entry to front (requires write lock but quick operation)
                        if let Ok(mut cache_write) = self.cache.write() {
                            cache_write.get(&path_buf); // Updates LRU position
                        }

                        return Ok(result);
                    }
                }
            }
        }

        // Cache miss or outdated - need to parse
        log::debug!("LRU cache miss for {}, parsing...", path.display());
        let analysis = crate::analysis::analyze_jsonl_file(path)?;
        let arc_analysis = Arc::new(analysis);
        let size_bytes = estimate_value_bytes(arc_analysis.as_ref());

        // Update cache (write lock) - LRU will auto-evict if at capacity
        if let Ok(mut cache_write) = self.cache.write() {
            cache_write.put(
                path_buf,
                CachedFile {
                    modified,
                    analysis: Arc::clone(&arc_analysis),
                    size_bytes,
                },
            );
        }

        Ok(arc_analysis)
    }

    /// Clears all entries from the cache
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// Removes entries for non-existent files (manual cleanup)
    ///
    /// With LRU eviction, stale entries are naturally removed over time, so this
    /// is typically not needed in production.
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

    /// Returns cache statistics for monitoring and debugging.
    ///
    /// `estimated_memory_kb` is a real sum of per-entry sizes captured by
    /// [`estimate_value_bytes`] at insertion time — previously this was a
    /// fixed 200 KB/entry constant that badly under-reported long sessions
    /// (a Claude Code log with file-write content can exceed several MB).
    pub fn stats(&self) -> CacheStats {
        if let Ok(cache) = self.cache.write() {
            let total_bytes: usize = cache.iter().map(|(_, c)| c.size_bytes).sum();
            CacheStats {
                entry_count: cache.len(),
                estimated_memory_kb: total_bytes / 1024,
            }
        } else {
            CacheStats::default()
        }
    }

    /// Removes a specific file from the cache
    pub fn invalidate<P: AsRef<Path>>(&self, path: P) {
        if let Ok(mut cache) = self.cache.write() {
            cache.pop(&path.as_ref().to_path_buf());
        }
    }

    /// Returns all currently cached file paths
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

/// Cache usage statistics for monitoring
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    pub entry_count: usize,
    pub estimated_memory_kb: usize,
}

/// Walk a `serde_json::Value` tree and return a best-effort byte estimate of
/// its heap footprint. This is intentionally a heuristic — `Value` nodes are
/// variable-sized enums with hidden allocator overhead — but it tracks real
/// payload growth far better than a flat per-entry constant.
fn estimate_value_bytes(v: &Value) -> usize {
    use std::mem::size_of;

    const NODE_OVERHEAD: usize = size_of::<Value>();

    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) => NODE_OVERHEAD,
        Value::String(s) => NODE_OVERHEAD + s.capacity(),
        Value::Array(arr) => {
            NODE_OVERHEAD
                + arr.capacity() * size_of::<Value>()
                + arr.iter().map(estimate_value_bytes).sum::<usize>()
        }
        Value::Object(map) => {
            // `serde_json::Map` is backed by `BTreeMap` or `IndexMap` depending
            // on features; both allocate the key `String` inline plus per-node
            // overhead. Approximating as key capacity + value estimate is
            // close enough for a diagnostic display.
            NODE_OVERHEAD
                + map
                    .iter()
                    .map(|(k, val)| k.capacity() + estimate_value_bytes(val))
                    .sum::<usize>()
        }
    }
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
