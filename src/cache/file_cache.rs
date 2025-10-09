use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
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
/// This is the single source of truth for all file parsing operations
pub struct FileParseCache {
    /// Map: file_path -> cached_data
    cache: RwLock<HashMap<PathBuf, CachedFile>>,
}

impl FileParseCache {
    /// Create a new empty cache
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::with_capacity(100)),
        }
    }

    /// Get or parse a file with caching based on modification time
    ///
    /// This function:
    /// 1. Checks if the file exists in cache and hasn't been modified
    /// 2. If yes, returns the cached Arc<Value> (zero-cost clone)
    /// 3. If no, parses the file and caches the result
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<Value>> {
        let path = path.as_ref();

        // Get file metadata (modification time)
        let metadata = fs::metadata(path)?;
        let modified = metadata.modified()?;

        // Try to get from cache (read lock)
        {
            if let Ok(cache_read) = self.cache.read() {
                if let Some(cached) = cache_read.get(path) {
                    // Check if the cached version is still valid
                    if cached.modified >= modified {
                        log::trace!("Cache hit for {}", path.display());
                        return Ok(Arc::clone(&cached.analysis));
                    }
                }
            }
        }

        // Cache miss or outdated - need to parse
        log::debug!("Cache miss for {}, parsing...", path.display());
        let analysis = crate::analysis::analyze_jsonl_file(path)?;
        let arc_analysis = Arc::new(analysis);

        // Update cache (write lock)
        if let Ok(mut cache_write) = self.cache.write() {
            cache_write.insert(
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
    pub fn cleanup_stale(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.retain(|path, _| path.exists());
        }
    }

    /// Get cache statistics (for monitoring)
    pub fn stats(&self) -> CacheStats {
        if let Ok(cache) = self.cache.read() {
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
            cache.remove(path.as_ref());
        }
    }

    /// Get all cached file paths
    pub fn get_cached_paths(&self) -> Vec<PathBuf> {
        if let Ok(cache) = self.cache.read() {
            cache.keys().cloned().collect()
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
