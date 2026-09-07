//! The persistent per-session ledger behind the `usage` and `analysis` scans.
//!
//! One file per provider under `~/.vct/sessions/` holds, for every session
//! the scans have ever read, its per-day token buckets, file-operation
//! counters and the stamps of the read that produced them. A scan loads the
//! ledger, re-reads only the sources whose stamps changed, folds every entry
//! whose day falls in the requested range, and writes the ledger back. A
//! session whose source is gone keeps its entry and keeps contributing, which
//! is what makes an `--all` total survive the assistants pruning their own
//! history. vct never deletes a ledger file: an unreadable one is set aside
//! under a new name and a newer one is left alone.
//!
//! `analysis --json` and `analysis FILE` are full-parse paths that never touch
//! the ledger, so retained sessions appear in every view but those two.

pub(crate) mod format;
pub(crate) mod key;
pub(crate) mod stamp;

use crate::constants::{FastHashMap, FastHashSet};
use crate::models::ExtensionType;
use crate::utils::{now_rfc3339_utc_nanos, write_json_atomic_pretty};
use anyhow::Result;
pub(crate) use format::{
    DayEntry, HalfStamps, ScanFeature, ScanStamp, SessionEntry, merge_analysis_rows,
};
use format::{LedgerFile, LedgerHeader, SCHEMA_VERSION};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The directory under `~/.vct` that holds the per-provider ledger files.
pub const LEDGER_DIR_NAME: &str = "sessions";

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

/// The ledger a scan reads from and writes to.
///
/// [`SessionLedger::new`] is in-memory only, for embedders and tests that want
/// the incremental scan without touching the user's home;
/// [`SessionLedger::open`] is what the CLI and the TUIs use.
pub struct SessionLedger {
    dir: Option<PathBuf>,
    providers: FastHashMap<ExtensionType, ProviderLedger>,
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
    /// An empty ledger that never touches the disk.
    pub fn new() -> Self {
        Self {
            dir: None,
            providers: FastHashMap::default(),
            parsed_sources: 0,
            total_parsed_sources: 0,
            retained: 0,
            last_saved: None,
        }
    }

    /// Loads every provider's ledger file under `<cache_dir>/sessions/`.
    ///
    /// Never fails: a missing file is an empty provider, an unreadable one is
    /// renamed aside (never deleted) and logged, and one written by a newer vct
    /// is left exactly as it is and that provider is never written back.
    pub fn open(cache_dir: &Path) -> Self {
        let dir = cache_dir.join(LEDGER_DIR_NAME);
        let mut providers = FastHashMap::default();
        for provider in ALL_PROVIDERS {
            providers.insert(provider, load_provider(&provider_path(&dir, provider)));
        }
        Self {
            dir: Some(dir),
            providers,
            ..Self::new()
        }
    }

    /// Where this ledger's files live, or `None` for an in-memory ledger.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Writes every provider whose entries changed since the last save.
    ///
    /// Each file is replaced atomically, so a concurrent reader sees the old
    /// file or the whole new one. A session another process wrote in the
    /// meantime that this ledger never saw is adopted rather than overwritten,
    /// so a long-running TUI cannot drop a session a one-shot scan recorded
    /// just before its source went away. Providers that failed to load as
    /// this build's format are skipped so their file is never overwritten.
    ///
    /// # Errors
    ///
    /// Returns the first write failure after attempting every provider.
    pub fn save(&mut self) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let mut first_error = None;
        for (provider, book) in &mut self.providers {
            if !book.dirty || book.read_only {
                continue;
            }
            match adopt_unseen_sessions(&provider_path(dir, *provider), &mut book.sessions) {
                Adoption::Writable => {}
                Adoption::Skip => continue,
                Adoption::Freeze => {
                    book.read_only = true;
                    continue;
                }
            }
            let file = LedgerFile {
                schema_version: SCHEMA_VERSION,
                provider: provider_file_stem(*provider).to_string(),
                source: book.source.clone(),
                sessions: std::mem::take(&mut book.sessions),
            };
            let written = write_json_atomic_pretty(provider_path(dir, *provider), &file);
            book.sessions = file.sessions;
            match written {
                Ok(()) => book.dirty = false,
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

    /// [`save`](Self::save), but at most once per `min_interval`.
    ///
    /// The first save is immediate, so a cold scan's whole parse lands on disk
    /// right away; after that a live session that changes every refresh
    /// rewrites its provider's file once a minute rather than every tick.
    ///
    /// # Errors
    ///
    /// As [`save`](Self::save).
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

    /// Starts one scan and resets its per-scan counters.
    pub(crate) fn begin_scan(&mut self) {
        self.parsed_sources = 0;
        self.retained = 0;
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

    /// Takes one provider's book out of the ledger for the duration of its
    /// scan; [`put_provider`](Self::put_provider) returns it.
    pub(crate) fn take_provider(&mut self, provider: ExtensionType) -> ProviderLedger {
        self.providers.remove(&provider).unwrap_or_default()
    }

    pub(crate) fn put_provider(&mut self, provider: ExtensionType, book: ProviderLedger) {
        self.providers.insert(provider, book);
    }
}

/// One provider's sessions and, for a shared-database provider, the stamps of
/// the database reads that produced them.
#[derive(Default)]
pub(crate) struct ProviderLedger {
    pub(crate) source: HalfStamps,
    pub(crate) sessions: BTreeMap<String, SessionEntry>,
    pub(crate) dirty: bool,
    read_only: bool,
}

impl ProviderLedger {
    /// Settles the sessions a scan did not find.
    ///
    /// When `mark` is set, a session not in `seen` is marked missing as of
    /// `today` (its retained diagnostics dropped, since the source can no
    /// longer be re-read) and one that holds no days at all is removed. A
    /// scan whose discovery was partial passes `mark = false`: it cannot tell
    /// a deleted source from one it failed to list, so it leaves the marks as
    /// they are. Either way the unseen sessions stay in the book and are the
    /// caller's to fold.
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

const ALL_PROVIDERS: [ExtensionType; 9] = [
    ExtensionType::ClaudeCode,
    ExtensionType::Codex,
    ExtensionType::Copilot,
    ExtensionType::Gemini,
    ExtensionType::Grok,
    ExtensionType::DeepSeek,
    ExtensionType::OpenCode,
    ExtensionType::Cursor,
    ExtensionType::Hermes,
];

/// The file stem of a provider's ledger, the same name as its `[providers]`
/// toggle in `config.toml`.
pub(crate) fn provider_file_stem(provider: ExtensionType) -> &'static str {
    match provider {
        ExtensionType::ClaudeCode => "claude",
        ExtensionType::Codex => "codex",
        ExtensionType::Copilot => "copilot",
        ExtensionType::Gemini => "gemini",
        ExtensionType::Grok => "grok",
        ExtensionType::DeepSeek => "dsh",
        ExtensionType::OpenCode => "opencode",
        ExtensionType::Cursor => "cursor",
        ExtensionType::Hermes => "hermes",
    }
}

/// `<dir>/<provider>.json`.
pub fn provider_path(dir: &Path, provider: ExtensionType) -> PathBuf {
    dir.join(format!("{}.json", provider_file_stem(provider)))
}

/// Today's local date, the way ledger days and missing marks are keyed.
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

/// Re-reads the provider file and adopts every session it holds that
/// `sessions` does not, so a save never discards what another process wrote
/// since this ledger loaded. In-memory entries win for a session both hold,
/// and an entry holding no days is not adopted: it is either a blank or failed
/// source, which costs one re-read to recover, or one this ledger has just
/// settled away, which would otherwise come back on every save.
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
        // An older or unreadable file: what this ledger holds was loaded from
        // it or parsed since, so writing over it loses nothing.
        _ => Adoption::Writable,
    }
}

fn load_provider(path: &Path) -> ProviderLedger {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ProviderLedger::default();
        }
        Err(error) => {
            // A file that exists but cannot be read must not be overwritten by
            // a ledger rebuilt from scratch.
            log::warn!("cannot read session ledger {}: {error}", path.display());
            return ProviderLedger {
                read_only: true,
                ..Default::default()
            };
        }
    };
    match serde_json::from_slice::<LedgerFile>(&body) {
        Ok(file) if file.schema_version == SCHEMA_VERSION => {
            return ProviderLedger {
                source: file.source,
                sessions: file.sessions,
                dirty: false,
                read_only: false,
            };
        }
        Ok(file) => return set_aside_or_freeze(path, file.schema_version),
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

/// A file in another schema version: a newer one is frozen for the build that
/// wrote it, an older one (none exists yet) is set aside like a corrupt file.
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

/// Moves an unreadable file out of the way under a timestamped name and
/// starts the provider empty. If even that fails the provider is frozen so
/// the file is never overwritten.
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
}
