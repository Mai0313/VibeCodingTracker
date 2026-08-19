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

/// One cached parse, checked for staleness against `fingerprint`.
///
/// `size_bytes` is a snapshot taken by [`estimate_analysis_bytes`] at insertion,
/// so reporting the cache's footprint never walks the analysis again.
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

/// Thread-safe LRU cache of parsed session analyses, bounded by
/// `constants::capacity::FILE_CACHE_SIZE`.
///
/// Entries are handed out as `Arc<CodeAnalysis>`, the typed struct rather than a
/// `serde_json::Value`: `serde_json::to_value` deep-clones every string and adds
/// per-node enum overhead, which roughly doubles the working set on a long
/// session. A caller that wants a `Value` serializes on demand instead.
pub struct FileParseCache {
    cache: RwLock<LruCache<PathBuf, CachedFile>>,
}

impl FileParseCache {
    /// Creates a new LRU cache with capacity from `constants::capacity::FILE_CACHE_SIZE`.
    pub fn new() -> Self {
        // FILE_CACHE_SIZE is a nonzero const, so this cannot fail.
        let cache_size = NonZeroUsize::new(capacity::FILE_CACHE_SIZE).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cache_size)),
        }
    }

    /// Retrieves cached analysis or parses the file if needed, auto-detecting
    /// the provider from file contents.
    ///
    /// Detection is content-based, so this suits an ad-hoc single-file path. A
    /// caller walking one provider's session directory already knows the
    /// provider and should parse through
    /// [`crate::session::parse_session_file_typed_as`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the file's metadata cannot be read or if parsing the
    /// session file fails (malformed data, unreadable contents, etc.). A
    /// poisoned cache lock does not error — it simply forces a reparse.
    pub fn get_or_parse<P: AsRef<Path>>(&self, path: P) -> Result<Arc<CodeAnalysis>> {
        let path = path.as_ref();
        let path_buf = path.to_path_buf();

        let primary = file_stamp(path)?;

        {
            if let Ok(cache_read) = self.cache.read() {
                // peek() rather than get(), so a hit needs no write lock.
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
        // Sampled before the parse: a stamp taken after it could be newer than
        // what was actually parsed, and the entry would never go stale.
        let possible_grok_dependencies = grok_dependency_stamps(path);
        let analysis = crate::session::parse_session_file_typed(path)?;
        let arc_analysis = Arc::new(analysis);
        let size_bytes = estimate_analysis_bytes(arc_analysis.as_ref());

        // At capacity the LRU evicts its oldest entry to make room.
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

    /// Removes entries for non-existent files (manual cleanup).
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
    /// `estimate_analysis_bytes` at insertion time.
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

/// Cache usage statistics for monitoring.
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    /// Number of entries currently held in the cache.
    pub entry_count: usize,
    /// Summed per-entry heap estimate in KiB (see `estimate_analysis_bytes`).
    pub estimated_memory_kb: usize,
}

/// Best-effort byte estimate of a [`CodeAnalysis`]'s heap footprint.
///
/// Counts the inline struct + every owned `String` capacity reachable from
/// the records. Ignores allocator padding and `HashMap` bucket overhead, and
/// approximates each `conversation_usage` value rather than walking it — still
/// more honest than a flat constant and vastly cheaper than
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
