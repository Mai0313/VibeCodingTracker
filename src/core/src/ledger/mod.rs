//! Persistent per-session ledger backing the `usage` and `analysis` scans.
//!
//! One file per provider under `~/.vct/sessions/` stores per-day token buckets, file-operation counters, and read stamps for every scanned session. Scans re-read only sources whose stamps changed, fold entries within the requested date range, and persist updates. Sessions whose source files are later deleted remain in the ledger and continue contributing to totals.
//!
//! Ledger files are never deleted: unreadable files are renamed aside, and files written by a newer schema version are left read-only. Only stale temporary files left by interrupted saves are removed. `analysis --json` and `analysis FILE` perform full parses without consulting the ledger, so retained sessions are omitted from those two views.

pub(crate) mod format;
pub(crate) mod key;
pub(crate) mod remote;
pub(crate) mod stamp;
pub(crate) mod tiny_map;

use crate::constants::{FastHashMap, FastHashSet};
use crate::models::ExtensionType;
use crate::utils::file::{ATOMIC_TEMP_PREFIX, write_json_atomic_metadata};
use crate::utils::now_rfc3339_utc_nanos;
use anyhow::Result;
pub(crate) use format::{
    DayEntry, HalfStamps, ScanFeature, ScanStamp, SessionEntry, merge_analysis_rows,
};
use format::{LedgerFile, LedgerHeader, SCHEMA_VERSION};
pub use remote::{EXPORT_LINE_PREFIX, REMOTES_DIR_NAME, prune_remotes, remote_dir, store_export};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
pub(crate) use tiny_map::TinyMap;

/// The directory under `~/.vct` that holds the per-provider ledger files.
pub const LEDGER_DIR_NAME: &str = "sessions";

/// Minimum age before an atomic-save temp file in the ledger directory is treated as abandoned.
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// Minimum file size worth spawning a worker thread for during [`SessionLedger::open`].
const PARALLEL_LOAD_MIN_BYTES: u64 = 64 * 1024;

/// Observable ledger statistics used by tests and diagnostic logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerStats {
    /// Session entries currently held, across every provider.
    pub entries: usize,
    /// Sources read during the most recent scan.
    pub parsed_sources: usize,
    /// Sources read since this ledger was opened.
    pub total_parsed_sources: usize,
    /// Sessions folded during the most recent scan whose source is gone.
    pub retained: usize,
}

/// Per-provider session ledger read and updated by scans.
///
/// [`SessionLedger::new`] creates an in-memory-only ledger; [`SessionLedger::open`] loads and persists files on disk.
pub struct SessionLedger {
    dir: Option<PathBuf>,
    providers: FastHashMap<ExtensionType, ProviderLedger>,
    /// Other machines' snapshots, reloaded at the start of every scan.
    remotes: Vec<remote::RemoteSnapshot>,
    parsed_sources: usize,
    total_parsed_sources: usize,
    retained: usize,
    last_saved: Option<Instant>,
}

impl Default for SessionLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionLedger {
    /// Creates an empty in-memory ledger that never touches disk.
    pub fn new() -> Self {
        Self {
            dir: None,
            providers: FastHashMap::default(),
            remotes: Vec::new(),
            parsed_sources: 0,
            total_parsed_sources: 0,
            retained: 0,
            last_saved: None,
        }
    }

    /// Loads all provider ledger files under `<cache_dir>/sessions/`.
    ///
    /// Missing files start empty, unreadable files are renamed aside and logged, and files with a newer schema version are left untouched and marked read-only.
    pub fn open(cache_dir: &Path) -> Self {
        let dir = cache_dir.join(LEDGER_DIR_NAME);
        remove_stale_temp_files(&dir);
        let mut large = Vec::new();
        let mut small = Vec::with_capacity(ALL_PROVIDERS.len());
        for provider in ALL_PROVIDERS {
            let path = provider_path(&dir, provider);
            if std::fs::metadata(&path)
                .is_ok_and(|metadata| metadata.len() >= PARALLEL_LOAD_MIN_BYTES)
            {
                large.push((provider, path));
            } else {
                small.push((provider, path));
            }
        }
        let mut providers = FastHashMap::default();
        if large.is_empty() {
            for (provider, path) in small {
                providers.insert(provider, load_provider(&path));
            }
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = large
                    .into_iter()
                    .map(|(provider, path)| scope.spawn(move || (provider, load_provider(&path))))
                    .collect();
                for (provider, path) in small {
                    providers.insert(provider, load_provider(&path));
                }
                for handle in handles {
                    let (provider, book) = handle.join().expect("ledger loader thread panicked");
                    providers.insert(provider, book);
                }
            });
        }
        Self {
            dir: Some(dir),
            providers,
            ..Self::new()
        }
    }

    /// Returns the directory holding this ledger's files, or `None` if in-memory only.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Writes every provider whose entries changed since the last save.
    ///
    /// Each file is replaced atomically. If a provider file changed on disk since it was loaded, unseen non-empty sessions from disk are adopted before writing; read-only providers (unreadable or newer schema version) are skipped.
    ///
    /// # Errors
    ///
    /// Returns the first write failure encountered after attempting all dirty providers.
    pub fn save(&mut self) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let mut first_error = None;
        for (provider, book) in &mut self.providers {
            if !book.dirty || book.read_only {
                continue;
            }
            let path = provider_path(dir, *provider);
            let unchanged = book.disk_state != DiskState::Unknown
                && current_disk_state(&path) == Some(book.disk_state);
            if !unchanged {
                match adopt_unseen_sessions(&path, &mut book.sessions) {
                    Adoption::Writable => {}
                    Adoption::Skip => continue,
                    Adoption::Freeze => {
                        book.read_only = true;
                        continue;
                    }
                }
            }
            let file = LedgerFile {
                schema_version: SCHEMA_VERSION,
                provider: provider_file_stem(*provider).to_string(),
                source: book.source.clone(),
                sessions: std::mem::take(&mut book.sessions),
            };
            let written = write_json_atomic_metadata(&path, &file);
            book.sessions = file.sessions;
            match written {
                Ok(metadata) => {
                    book.dirty = false;
                    book.disk_state = metadata.as_ref().map_or(DiskState::Unknown, |meta| {
                        DiskState::Present(DiskFileStamp::from_metadata(meta))
                    });
                }
                Err(error) => {
                    log::warn!(
                        "failed to write the {} session ledger: {error:#}",
                        file.provider
                    );
                    first_error.get_or_insert(error);
                }
            }
        }
        self.last_saved = Some(Instant::now());
        first_error.map_or(Ok(()), Err)
    }

    /// Calls [`Self::save`] if any provider is dirty and at least `min_interval` has elapsed since the previous save.
    ///
    /// The first save on a ledger runs immediately.
    ///
    /// # Errors
    ///
    /// Returns whatever [`Self::save`] returns.
    pub fn save_if_due(&mut self, min_interval: Duration) -> Result<()> {
        if !self.providers.values().any(|book| book.dirty) {
            return Ok(());
        }
        if self
            .last_saved
            .is_some_and(|saved| saved.elapsed() < min_interval)
        {
            return Ok(());
        }
        self.save()
    }

    /// Returns current and cumulative ledger statistics.
    pub fn stats(&self) -> LedgerStats {
        LedgerStats {
            entries: self
                .providers
                .values()
                .map(|book| book.sessions.len())
                .sum(),
            parsed_sources: self.parsed_sources,
            total_parsed_sources: self.total_parsed_sources,
            retained: self.retained,
        }
    }

    /// Resets per-scan counters without reloading remote snapshots.
    pub(crate) fn begin_local_scan(&mut self) {
        self.parsed_sources = 0;
        self.retained = 0;
    }

    /// Starts one scan and resets its per-scan counters.
    pub(crate) fn begin_scan(&mut self) {
        self.begin_local_scan();
        self.reload_remotes();
    }

    /// Records that `count` sources were read during this scan.
    pub(crate) fn record_parses(&mut self, count: usize) {
        self.parsed_sources += count;
        self.total_parsed_sources += count;
    }

    /// Records that `count` sessions with no source were folded this scan.
    pub(crate) fn record_retained(&mut self, count: usize) {
        self.retained += count;
    }

    /// Removes and returns `provider`'s book for scanning; pair with [`Self::put_provider`].
    pub(crate) fn take_provider(&mut self, provider: ExtensionType) -> ProviderLedger {
        self.providers.remove(&provider).unwrap_or_default()
    }

    pub(crate) fn put_provider(&mut self, provider: ExtensionType, book: ProviderLedger) {
        self.providers.insert(provider, book);
    }
}

/// One provider's sessions and, for shared-database providers, its whole-database read stamps.
#[derive(Default)]
pub(crate) struct ProviderLedger {
    pub(crate) source: HalfStamps,
    pub(crate) sessions: BTreeMap<String, SessionEntry>,
    pub(crate) dirty: bool,
    read_only: bool,
    disk_state: DiskState,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DiskState {
    #[default]
    Unknown,
    Absent,
    Present(DiskFileStamp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskFileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl DiskFileStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
        }
    }
}

fn current_disk_state(path: &Path) -> Option<DiskState> {
    match std::fs::metadata(path) {
        Ok(metadata) => Some(DiskState::Present(DiskFileStamp::from_metadata(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(DiskState::Absent),
        Err(_) => None,
    }
}

impl ProviderLedger {
    /// Marks unseen sessions missing as of `today` and drops unseen empty entries when `mark` is true.
    ///
    /// Pass `mark = false` after partial discovery so unreadable directories are not treated as deleted sources; missing entries have their retained failure diagnostics cleared.
    pub(crate) fn settle_unseen(&mut self, seen: &FastHashSet<String>, mark: bool, today: &str) {
        if !mark {
            return;
        }
        let mut changed = false;
        self.sessions.retain(|key, entry| {
            if seen.contains(key) || entry.missing_since.is_some() {
                return true;
            }
            changed = true;
            if entry.days.is_empty() {
                return false;
            }
            entry.missing_since = Some(today.to_string());
            entry.scanned.clear_failures();
            true
        });
        if changed {
            self.dirty = true;
        }
    }

    /// Marks every session missing, for a provider whose root is gone.
    pub(crate) fn settle_all_missing(&mut self, today: &str) {
        self.settle_unseen(&FastHashSet::default(), true, today);
    }

    /// Clears a session's missing mark when its source has come back.
    pub(crate) fn mark_present(&mut self, key: &str) {
        if let Some(entry) = self.sessions.get_mut(key)
            && entry.missing_since.take().is_some()
        {
            self.dirty = true;
        }
    }
}

pub(crate) const ALL_PROVIDERS: [ExtensionType; 10] = [
    ExtensionType::ClaudeCode,
    ExtensionType::Codex,
    ExtensionType::Copilot,
    ExtensionType::Gemini,
    ExtensionType::Grok,
    ExtensionType::DeepSeek,
    ExtensionType::Antigravity,
    ExtensionType::OpenCode,
    ExtensionType::Cursor,
    ExtensionType::Hermes,
];

/// Returns the ledger file stem for `provider`, matching its `[providers]` key in `config.toml`.
pub(crate) fn provider_file_stem(provider: ExtensionType) -> &'static str {
    match provider {
        ExtensionType::ClaudeCode => "claude",
        ExtensionType::Codex => "codex",
        ExtensionType::Copilot => "copilot",
        ExtensionType::Gemini => "gemini",
        ExtensionType::Grok => "grok",
        ExtensionType::DeepSeek => "dsh",
        ExtensionType::Antigravity => "antigravity",
        ExtensionType::OpenCode => "opencode",
        ExtensionType::Cursor => "cursor",
        ExtensionType::Hermes => "hermes",
    }
}

/// Returns the ledger file path for `provider` inside `dir` (`<dir>/<provider>.json`).
pub fn provider_path(dir: &Path, provider: ExtensionType) -> PathBuf {
    dir.join(format!("{}.json", provider_file_stem(provider)))
}

/// Returns today's local date (`YYYY-MM-DD`) as used for day keys and missing marks.
pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Whether a provider file may be written over after re-reading it.
enum Adoption {
    Writable,
    /// The file could not be read right now; try again at the next save.
    Skip,
    /// A newer vct has written the file since it was loaded.
    Freeze,
}

/// Merges non-empty sessions present on disk at `path` but absent from `sessions`, keeping in-memory entries on conflict.
///
/// Empty disk entries are ignored so settled blank/failed sessions are not resurrected.
fn adopt_unseen_sessions(path: &Path, sessions: &mut BTreeMap<String, SessionEntry>) -> Adoption {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Adoption::Writable,
        Err(error) => {
            log::warn!("cannot re-read session ledger {}: {error}", path.display());
            return Adoption::Skip;
        }
    };
    match serde_json::from_slice::<LedgerFile>(&body) {
        Ok(file) if file.schema_version == SCHEMA_VERSION => {
            for (key, entry) in file.sessions {
                if !entry.days.is_empty() {
                    sessions.entry(key).or_insert(entry);
                }
            }
            Adoption::Writable
        }
        Ok(file) if file.schema_version > SCHEMA_VERSION => Adoption::Freeze,
        // An older or unreadable file: what this ledger holds was loaded from it or parsed since, so writing over it loses nothing.
        _ => Adoption::Writable,
    }
}

/// Removes abandoned atomic-save temp files older than [`STALE_TEMP_AGE`] from `dir`.
fn remove_stale_temp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(ATOMIC_TEMP_PREFIX)
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_TEMP_AGE);
        if stale && let Err(error) = std::fs::remove_file(entry.path()) {
            log::warn!(
                "cannot remove stale ledger temp file {}: {error}",
                entry.path().display()
            );
        }
    }
}

fn load_provider(path: &Path) -> ProviderLedger {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ProviderLedger {
                disk_state: DiskState::Absent,
                ..Default::default()
            };
        }
        Err(error) => {
            // A file that exists but cannot be read must not be overwritten by a ledger rebuilt from scratch.
            log::warn!("cannot read session ledger {}: {error}", path.display());
            return ProviderLedger {
                read_only: true,
                ..Default::default()
            };
        }
    };
    let metadata = file.metadata().ok();
    let mut body = Vec::with_capacity(metadata.as_ref().map_or(0, |meta| meta.len() as usize));
    if let Err(error) = file.read_to_end(&mut body) {
        log::warn!("cannot read session ledger {}: {error}", path.display());
        return ProviderLedger {
            read_only: true,
            ..Default::default()
        };
    }
    match serde_json::from_slice::<LedgerFile>(&body) {
        Ok(parsed) if parsed.schema_version == SCHEMA_VERSION => {
            return ProviderLedger {
                source: parsed.source,
                sessions: parsed.sessions,
                dirty: false,
                read_only: false,
                disk_state: metadata.as_ref().map_or(DiskState::Unknown, |meta| {
                    DiskState::Present(DiskFileStamp::from_metadata(meta))
                }),
            };
        }
        Ok(parsed) => return set_aside_or_freeze(path, parsed.schema_version),
        Err(error) => {
            if let Ok(header) = serde_json::from_slice::<LedgerHeader>(&body)
                && header.schema_version != SCHEMA_VERSION
            {
                return set_aside_or_freeze(path, header.schema_version);
            }
            log::warn!("session ledger {} is unreadable: {error}", path.display());
        }
    }
    set_aside(path)
}

/// Freezes a provider whose file has a newer schema version, or sets aside one with an older unmigratable version.
fn set_aside_or_freeze(path: &Path, version: u32) -> ProviderLedger {
    if version > SCHEMA_VERSION {
        log::warn!(
            "session ledger {} was written by a newer vct (schema v{version}); leaving it untouched",
            path.display()
        );
        return ProviderLedger {
            read_only: true,
            ..Default::default()
        };
    }
    log::warn!(
        "session ledger {} carries schema v{version}, which this build cannot migrate",
        path.display()
    );
    set_aside(path)
}

/// Renames an unreadable ledger file to a timestamped `.unreadable-*.json` sibling, or marks the provider read-only if renaming fails.
fn set_aside(path: &Path) -> ProviderLedger {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let aside = path.with_file_name(format!(
        "{stem}.unreadable-{}.json",
        now_rfc3339_utc_nanos().replace(':', "-")
    ));
    match std::fs::rename(path, &aside) {
        Ok(()) => {
            log::warn!("set aside unreadable session ledger as {}", aside.display());
            ProviderLedger::default()
        }
        // Another process set it aside first; nothing is left to protect.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProviderLedger::default(),
        Err(error) => {
            let error = anyhow::Error::from(error).context(format!(
                "renaming {} to {}",
                path.display(),
                aside.display()
            ));
            log::warn!("{error:#}; leaving the session ledger untouched");
            ProviderLedger {
                read_only: true,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_day(date: &str) -> SessionEntry {
        let mut entry = SessionEntry::default();
        let mut day = DayEntry::default();
        day.active.analysis = true;
        entry.days.insert(date.to_string(), day);
        entry
    }

    #[test]
    fn open_removes_only_stale_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join(LEDGER_DIR_NAME);
        std::fs::create_dir_all(&sessions).unwrap();
        let stale = sessions.join(".tmpStale1");
        let fresh = sessions.join(".tmpFresh1");
        let ledger_file = sessions.join("kept.json");
        let old = std::time::SystemTime::now() - STALE_TEMP_AGE;
        for path in [&stale, &fresh, &ledger_file] {
            std::fs::write(path, "{").unwrap();
        }
        for path in [&stale, &ledger_file] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        SessionLedger::open(dir.path());

        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(ledger_file.exists());
    }

    #[test]
    fn round_trips_through_the_provider_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::open(dir.path());
        let mut book = ledger.take_provider(ExtensionType::ClaudeCode);
        book.sessions
            .insert("proj/a.jsonl".to_string(), entry_with_day("2026-09-08"));
        book.dirty = true;
        ledger.put_provider(ExtensionType::ClaudeCode, book);
        ledger.save().unwrap();

        let reopened = SessionLedger::open(dir.path());
        assert_eq!(reopened.stats().entries, 1);
        let path = provider_path(&dir.path().join(LEDGER_DIR_NAME), ExtensionType::ClaudeCode);
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(body["schema_version"], SCHEMA_VERSION);
        assert_eq!(body["provider"], "claude");
        assert_eq!(
            body["sessions"]["proj/a.jsonl"]["days"]["2026-09-08"]["active"]["analysis"],
            true
        );
    }

    #[test]
    fn a_newer_file_is_left_untouched_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join(LEDGER_DIR_NAME);
        std::fs::create_dir_all(&sessions).unwrap();
        let path = provider_path(&sessions, ExtensionType::Codex);
        let newer = format!(
            r#"{{"schema_version":{},"provider":"codex","future":1}}"#,
            SCHEMA_VERSION + 1
        );
        std::fs::write(&path, &newer).unwrap();

        let mut ledger = SessionLedger::open(dir.path());
        assert_eq!(ledger.stats().entries, 0);
        let mut book = ledger.take_provider(ExtensionType::Codex);
        book.sessions
            .insert("x".to_string(), entry_with_day("2026-09-08"));
        book.dirty = true;
        ledger.put_provider(ExtensionType::Codex, book);
        ledger.save().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
        assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1);
    }

    #[test]
    fn a_corrupt_file_is_set_aside_rather_than_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join(LEDGER_DIR_NAME);
        std::fs::create_dir_all(&sessions).unwrap();
        let path = provider_path(&sessions, ExtensionType::Gemini);
        std::fs::write(&path, "{not json").unwrap();

        let ledger = SessionLedger::open(dir.path());
        assert_eq!(ledger.stats().entries, 0);
        assert!(!path.exists());
        let aside: Vec<_> = std::fs::read_dir(&sessions)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].starts_with("gemini.unreadable-"));
        assert_eq!(
            std::fs::read_to_string(sessions.join(&aside[0])).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn settling_unseen_marks_sessions_with_days_and_drops_empty_ones() {
        let mut book = ProviderLedger::default();
        book.sessions
            .insert("kept".into(), entry_with_day("2026-09-01"));
        book.sessions
            .insert("gone".into(), entry_with_day("2026-09-01"));
        book.sessions
            .insert("blank".into(), SessionEntry::default());
        let seen: FastHashSet<String> = ["kept".to_string()].into_iter().collect();

        book.settle_unseen(&seen, false, "2026-09-08");
        assert!(!book.dirty);
        assert_eq!(book.sessions.len(), 3);

        book.settle_unseen(&seen, true, "2026-09-08");
        assert!(book.dirty);
        assert_eq!(book.sessions.len(), 2);
        assert_eq!(
            book.sessions["gone"].missing_since.as_deref(),
            Some("2026-09-08")
        );
        assert!(book.sessions["kept"].missing_since.is_none());

        book.dirty = false;
        book.mark_present("gone");
        assert!(book.dirty);
        assert!(book.sessions["gone"].missing_since.is_none());
    }

    #[test]
    fn saving_adopts_a_session_another_process_wrote_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = SessionLedger::open(dir.path());
        let mut second = SessionLedger::open(dir.path());

        let mut book = second.take_provider(ExtensionType::Copilot);
        book.sessions
            .insert("theirs".into(), entry_with_day("2026-09-08"));
        book.dirty = true;
        second.put_provider(ExtensionType::Copilot, book);
        second.save().unwrap();

        let mut book = first.take_provider(ExtensionType::Copilot);
        book.sessions
            .insert("mine".into(), entry_with_day("2026-09-08"));
        book.dirty = true;
        first.put_provider(ExtensionType::Copilot, book);
        first.save().unwrap();

        let reopened = SessionLedger::open(dir.path());
        assert_eq!(reopened.stats().entries, 2);
    }

    #[test]
    fn save_if_due_writes_immediately_once_then_waits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::open(dir.path());
        let mut book = ledger.take_provider(ExtensionType::Grok);
        book.sessions
            .insert("a".into(), entry_with_day("2026-09-08"));
        book.dirty = true;
        ledger.put_provider(ExtensionType::Grok, book);
        ledger.save_if_due(Duration::from_secs(3600)).unwrap();
        assert_eq!(SessionLedger::open(dir.path()).stats().entries, 1);

        let mut book = ledger.take_provider(ExtensionType::Grok);
        book.sessions
            .insert("b".into(), entry_with_day("2026-09-08"));
        book.dirty = true;
        ledger.put_provider(ExtensionType::Grok, book);
        ledger.save_if_due(Duration::from_secs(3600)).unwrap();
        assert_eq!(SessionLedger::open(dir.path()).stats().entries, 1);
        ledger.save_if_due(Duration::ZERO).unwrap();
        assert_eq!(SessionLedger::open(dir.path()).stats().entries, 2);
    }

    #[test]
    fn saving_without_external_changes_drops_removed_sessions_and_writes_compact_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::open(dir.path());
        let mut book = ledger.take_provider(ExtensionType::ClaudeCode);
        book.sessions
            .insert("first".into(), entry_with_day("2026-09-08"));
        book.sessions
            .insert("second".into(), entry_with_day("2026-09-08"));
        book.dirty = true;
        ledger.put_provider(ExtensionType::ClaudeCode, book);
        ledger.save().unwrap();

        // Removing an entry in the owning ledger must not resurrect it from disk when no external writer touched the file since the previous save.
        let mut book = ledger.take_provider(ExtensionType::ClaudeCode);
        book.sessions.remove("second");
        book.dirty = true;
        ledger.put_provider(ExtensionType::ClaudeCode, book);
        ledger.save().unwrap();

        let path = provider_path(&dir.path().join(LEDGER_DIR_NAME), ExtensionType::ClaudeCode);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 1);
        assert_eq!(SessionLedger::open(dir.path()).stats().entries, 1);
    }
}
