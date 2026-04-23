use crate::analysis::AnalysisMode;
use crate::constants::capacity;
use crate::models::{
    CodeAnalysis, CodeAnalysisApplyDiffDetail, CodeAnalysisReadDetail, CodeAnalysisRecord,
    CodeAnalysisRunCommandDetail, CodeAnalysisWriteDetail, ExtensionType,
};
use anyhow::Result;
use lru::LruCache;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Cached file entry with modification time tracking for invalidation.
///
/// `size_bytes` is captured once at insertion via [`estimate_analysis_bytes`]
/// so the `stats()` path can report a realistic memory footprint without
/// walking the analysis on every call.
#[derive(Debug, Clone)]
struct CachedFile {
    modified: SystemTime,
    analysis: Arc<CodeAnalysis>,
    size_bytes: usize,
}

/// Thread-safe LRU cache for parsed session analyses with automatic eviction.
///
/// Entries are held as `Arc<CodeAnalysis>` (the typed struct). We deliberately
/// avoid caching `Arc<serde_json::Value>` because `serde_json::to_value` deep-
/// clones every string and adds per-node `Value` enum overhead on top — for
/// long Claude sessions that roughly doubles the working set. Callers that
/// need a `Value` (CLI single-file dump) serialise on demand from the typed
/// form, which only happens once per request rather than once per cache entry.
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

    /// Retrieves cached analysis or parses the file if needed, auto-detecting
    /// the provider from file contents.
    ///
    /// Prefer [`Self::get_or_parse_as`] whenever the caller already knows which
    /// provider the file belongs to (e.g. walking a specific session
    /// directory). Content detection is only safe for ad-hoc single-file paths.
    ///
    /// Workflow:
    /// 1. Check cache hit with read-only peek (no lock contention)
    /// 2. If valid, promote entry to front with write lock
    /// 3. If miss/stale, parse file and cache result (may evict LRU entry)
    ///
    /// Optimized to minimize write lock contention in parallel workloads.
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<CodeAnalysis>> {
        self.get_or_parse_inner(path.as_ref(), None)
    }

    /// Same as [`Self::get_or_parse`] but the caller specifies which provider
    /// the file belongs to — the analyzer skips content-based detection, so
    /// metadata sentinels at the top of a Claude session (`permission-mode`,
    /// `file-history-snapshot`) cannot cause the file to be mis-filed under a
    /// different provider.
    pub fn get_or_parse_as<P: AsRef<Path>>(
        &self,
        path: P,
        provider: ExtensionType,
    ) -> Result<Arc<CodeAnalysis>> {
        self.get_or_parse_inner(path.as_ref(), Some(provider))
    }

    fn get_or_parse_inner(
        &self,
        path: &Path,
        provider: Option<ExtensionType>,
    ) -> Result<Arc<CodeAnalysis>> {
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

        // Cache miss or outdated - need to parse.
        log::debug!("LRU cache miss for {}, parsing...", path.display());
        let analysis = match provider {
            Some(p) => crate::analysis::analyze_session_file_typed_as(path, p, AnalysisMode::Full)?,
            None => crate::analysis::analyze_jsonl_file_typed(path)?,
        };
        let arc_analysis = Arc::new(analysis);
        let size_bytes = estimate_analysis_bytes(arc_analysis.as_ref());

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
    /// [`estimate_analysis_bytes`] at insertion time.
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

/// Best-effort byte estimate of a [`CodeAnalysis`]'s heap footprint.
///
/// Counts the inline struct + every owned `String` capacity reachable from
/// the records. Ignores allocator padding, `HashMap` bucket overhead, and the
/// `serde_json::Value` payload inside `conversation_usage` (we only count the
/// keys) — still more honest than a flat constant and vastly cheaper than
/// `serde_json::to_vec(...).len()`.
fn estimate_analysis_bytes(analysis: &CodeAnalysis) -> usize {
    use std::mem::size_of;

    let mut bytes = size_of::<CodeAnalysis>();
    bytes += analysis.user.capacity();
    bytes += analysis.extension_name.capacity();
    bytes += analysis.insights_version.capacity();
    bytes += analysis.machine_id.capacity();
    bytes += analysis.records.capacity() * size_of::<CodeAnalysisRecord>();

    for record in &analysis.records {
        bytes += record.task_id.capacity();
        bytes += record.folder_path.capacity();
        bytes += record.git_remote_url.capacity();

        bytes += record.write_file_details.capacity() * size_of::<CodeAnalysisWriteDetail>();
        for detail in &record.write_file_details {
            bytes += detail.base.file_path.capacity();
            bytes += detail.content.capacity();
        }

        bytes += record.read_file_details.capacity() * size_of::<CodeAnalysisReadDetail>();
        for detail in &record.read_file_details {
            bytes += detail.base.file_path.capacity();
        }

        bytes += record.edit_file_details.capacity() * size_of::<CodeAnalysisApplyDiffDetail>();
        for detail in &record.edit_file_details {
            bytes += detail.base.file_path.capacity();
            bytes += detail.old_string.capacity();
            bytes += detail.new_string.capacity();
        }

        bytes += record.run_command_details.capacity() * size_of::<CodeAnalysisRunCommandDetail>();
        for detail in &record.run_command_details {
            bytes += detail.base.file_path.capacity();
            bytes += detail.command.capacity();
            bytes += detail.description.capacity();
        }

        // conversation_usage: FastHashMap<String, serde_json::Value>
        for (k, _) in &record.conversation_usage {
            bytes += k.capacity();
            // Values are small token-count objects; approximate at 256 B each
            // rather than walking Value trees on every insertion.
            bytes += 256;
        }
    }

    bytes
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
