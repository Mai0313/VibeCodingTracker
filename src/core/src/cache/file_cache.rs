use crate::constants::capacity;
use crate::models::{
    CodeAnalysis, CodeAnalysisApplyDiffDetail, CodeAnalysisReadDetail, CodeAnalysisRecord,
    CodeAnalysisRunCommandDetail, CodeAnalysisWriteDetail,
};
use anyhow::Result;
use lru::LruCache;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Cached parse entry with its staleness fingerprint and insertion-time byte estimate.
#[derive(Debug, Clone)]
struct CachedFile {
    fingerprint: FileFingerprint,
    analysis: Arc<CodeAnalysis>,
    size_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GrokDependencyStamps {
    summary: Option<FileStamp>,
    updates: Option<FileStamp>,
    cwd: Option<FileStamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    primary: FileStamp,
    grok_dependencies: Option<GrokDependencyStamps>,
}

/// LRU cache of parsed [`CodeAnalysis`] records, keyed by file path and bounded by [`crate::constants::capacity::FILE_CACHE_SIZE`].
pub struct FileParseCache {
    cache: RwLock<LruCache<PathBuf, CachedFile>>,
}

impl FileParseCache {
    /// Returns an empty cache with capacity [`crate::constants::capacity::FILE_CACHE_SIZE`].
    pub fn new() -> Self {
        // FILE_CACHE_SIZE is a nonzero const, so this cannot fail.
        let cache_size = NonZeroUsize::new(capacity::FILE_CACHE_SIZE).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cache_size)),
        }
    }

    /// Returns the cached [`CodeAnalysis`] for `path`, reparsing with content-based provider detection on a miss or when the file fingerprint changes.
    ///
    /// When the provider is already known from the directory layout, use [`crate::session::parse_session_file_typed_as`] instead. A poisoned cache lock falls back to parsing without caching.
    ///
    /// # Errors
    ///
    /// Returns an error if file metadata cannot be read or if session parsing fails.
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<CodeAnalysis>> {
        let path = path.as_ref();
        let path_buf = path.to_path_buf();

        let primary = file_stamp(path)?;

        {
            if let Ok(cache_read) = self.cache.read() {
                // Use peek() under the read lock to check the fingerprint before taking a write lock.
                if let Some(cached) = cache_read.peek(&path_buf) {
                    let is_grok = cached.analysis.extension_name == "Grok";
                    let fingerprint = FileFingerprint {
                        primary,
                        grok_dependencies: is_grok.then(|| grok_dependency_stamps(path)),
                    };
                    if cached.fingerprint == fingerprint {
                        log::trace!("LRU cache hit for {}", path.display());
                        let result = Arc::clone(&cached.analysis);
                        // Still holding this guard would deadlock the write lock below.
                        drop(cache_read);

                        // Promote the hit to the front of the LRU.
                        if let Ok(mut cache_write) = self.cache.write() {
                            cache_write.get(&path_buf);
                        }

                        return Ok(result);
                    }
                }
            }
        }

        log::debug!("LRU cache miss for {}, parsing...", path.display());
        // Sampled before the parse: a stamp taken after it could be newer than what was actually parsed, and the entry would never go stale.
        let possible_grok_dependencies = grok_dependency_stamps(path);
        let analysis = crate::session::parse_session_file_typed(path)?;
        let arc_analysis = Arc::new(analysis);
        let size_bytes = estimate_analysis_bytes(arc_analysis.as_ref());

        if let Ok(mut cache_write) = self.cache.write() {
            let is_grok = arc_analysis.extension_name == "Grok";
            cache_write.put(
                path_buf,
                CachedFile {
                    fingerprint: FileFingerprint {
                        primary,
                        grok_dependencies: is_grok.then_some(possible_grok_dependencies),
                    },
                    analysis: Arc::clone(&arc_analysis),
                    size_bytes,
                },
            );
        }

        Ok(arc_analysis)
    }

    /// Clears all entries from the cache.
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// Removes cached entries whose paths no longer exist on disk.
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

    /// Returns current cache entry count and estimated heap usage.
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

    /// Removes a specific file from the cache.
    pub fn invalidate<P: AsRef<Path>>(&self, path: P) {
        if let Ok(mut cache) = self.cache.write() {
            cache.pop(&path.as_ref().to_path_buf());
        }
    }

    /// Returns all currently cached file paths.
    pub fn get_cached_paths(&self) -> Vec<PathBuf> {
        if let Ok(cache) = self.cache.write() {
            cache.iter().map(|(path, _)| path.clone()).collect()
        } else {
            Vec::new()
        }
    }
}

fn file_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

fn optional_file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: metadata.modified().ok()?,
        len: metadata.len(),
    })
}

fn grok_dependency_stamps(signals_path: &Path) -> GrokDependencyStamps {
    GrokDependencyStamps {
        summary: optional_file_stamp(&signals_path.with_file_name("summary.json")),
        updates: optional_file_stamp(&signals_path.with_file_name("updates.jsonl")),
        cwd: signals_path
            .parent()
            .and_then(Path::parent)
            .and_then(|workspace| optional_file_stamp(&workspace.join(".cwd"))),
    }
}

impl Default for FileParseCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache occupancy and estimated memory footprint.
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    /// Number of entries currently held in the cache.
    pub entry_count: usize,
    /// Estimated heap usage of cached entries, in KiB.
    pub estimated_memory_kb: usize,
}

/// Returns a best-effort heap-size estimate in bytes for `analysis`, summing reachable `Vec`/`String` capacities and approximating each `conversation_usage` value.
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

        for (k, _) in &record.conversation_usage {
            bytes += k.capacity();
            // Values are small token-count objects; approximate at 256 B each rather than walking Value trees on every insertion.
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
