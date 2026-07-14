//! File-based diagnostic logging (`~/.vct/logs/vct-YYYY-MM-DD.log`).
//!
//! Keeps the standard `log` facade — every existing `log::warn!` / `error!` in
//! the crate is unchanged — but swaps the output backend for a small file
//! logger. Records land in a plain-text daily file under `~/.vct/logs`, **never**
//! on stdout/stderr, so the interactive TUI is never corrupted. The file is
//! created lazily on the first record, so a command that logs nothing (e.g. a
//! successful `vct version`) leaves `~/.vct` untouched.
//!
//! [`init`] installs the logger (default level `warn`) plus a panic hook that
//! restores the terminal and records the panic. [`apply`] later reconfigures the
//! level from `[logging]` in the user config and prunes stale files.

use crate::config::{LogLevel, LoggingConfig};
use crate::utils::now_rfc3339_utc_nanos;
use chrono::{Duration, NaiveDate, Utc};
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Subdirectory of `~/.vct` holding the daily log files.
const LOG_DIR_NAME: &str = "logs";
const LOG_FILE_PREFIX: &str = "vct-";
const LOG_FILE_SUFFIX: &str = ".log";

/// The process-wide file logger, installed once by [`init`].
static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// An open daily log file plus the UTC date it was opened for.
///
/// Tracking the date lets a process that crosses UTC midnight roll over to the
/// next day's file instead of appending a whole multi-day session into the file
/// named for the day it started.
struct OpenLog {
    date: String,
    file: File,
}

/// A thread-safe file logger implementing [`log::Log`].
///
/// The open file lives behind a `Mutex` so the main thread and the (up to four)
/// background quota workers can share it. Each record is written with a single
/// `write_all` and no user-space buffering, so nothing is lost on a
/// `panic = "abort"` process abort.
struct FileLogger {
    /// Current max level as `LevelFilter as usize`, kept in sync with
    /// `log::set_max_level` so [`FileLogger::enabled`] stays authoritative.
    level: AtomicUsize,
    /// The log directory (`~/.vct/logs`), or `None` when the home directory
    /// cannot be resolved (logging then silently no-ops).
    dir: Option<PathBuf>,
    /// The currently-open daily file, opened lazily on the first record and
    /// reopened when the UTC day rolls over.
    open: Mutex<Option<OpenLog>>,
}

impl FileLogger {
    /// Builds a logger rooted at `dir` (test seam) with an initial max level.
    fn new(dir: Option<PathBuf>, level: LevelFilter) -> Self {
        Self {
            level: AtomicUsize::new(level as usize),
            dir,
            open: Mutex::new(None),
        }
    }

    /// Appends one preformatted line, opening (or rotating to) the day's file.
    ///
    /// Creating `~/.vct/logs` and the file only happens here, so a process that
    /// never emits a record never creates anything on disk. The file name uses
    /// the same UTC date as the line timestamps, and rolls over on the first
    /// record after UTC midnight.
    fn append(&self, line: &str) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let today = utc_date();
        // A poisoned mutex still holds a valid file handle; recover and keep logging.
        let mut guard = self.open.lock().unwrap_or_else(|p| p.into_inner());
        // (Re)open when there is no file yet or the UTC day has rolled over.
        if guard.as_ref().is_none_or(|o| o.date != today) {
            if fs::create_dir_all(dir).is_err() {
                return;
            }
            let path = dir.join(format!("{LOG_FILE_PREFIX}{today}{LOG_FILE_SUFFIX}"));
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => *guard = Some(OpenLog { date: today, file }),
                Err(_) => return,
            }
        }
        if let Some(open) = guard.as_mut() {
            let _ = open.file.write_all(line.as_bytes());
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() as usize <= self.level.load(Ordering::Relaxed)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &now_rfc3339_utc_nanos(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        self.append(&line);
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.open.lock()
            && let Some(open) = guard.as_mut()
        {
            let _ = open.file.flush();
        }
    }
}

/// Today's date in UTC as `YYYY-MM-DD`.
///
/// Deliberately UTC (not the local-day `utils::get_current_date`) so a log
/// file's name matches the UTC timestamps of the lines inside it.
fn utc_date() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

/// Formats one log line: `<utc-nanos> <LEVEL> <target>  <message>`.
///
/// The crate's own `vibe_coding_tracker::` target prefix is shortened to `vct::`
/// to keep lines readable.
fn format_line(now: &str, level: Level, target: &str, msg: &str) -> String {
    format!("{now} {:<5} {}  {msg}\n", level, normalize_target(target))
}

/// Rewrites the crate's module-path target from `vibe_coding_tracker[...]` to
/// `vct[...]`; leaves third-party targets untouched.
fn normalize_target(target: &str) -> String {
    match target.strip_prefix("vibe_coding_tracker") {
        Some(rest) => format!("vct{rest}"),
        None => target.to_string(),
    }
}

/// Maps the user-facing [`LogLevel`] onto the `log` crate's [`LevelFilter`].
fn to_level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Off => LevelFilter::Off,
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    }
}

/// Installs the global file logger at the default level (`warn`) and a panic
/// hook that restores the terminal and records the panic.
///
/// Called once at process start, before any subcommand runs. Safe to call more
/// than once (subsequent calls are ignored) so tests that exercise `main` don't
/// double-install. The level is later refined by [`apply`] once the user config
/// is loaded.
pub fn init() {
    let dir = home::home_dir().map(|home| home.join(".vct").join(LOG_DIR_NAME));
    let logger = LOGGER.get_or_init(|| FileLogger::new(dir, LevelFilter::Warn));
    // `set_logger` succeeds only on the first install for the whole process.
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Warn);
    }
    // Terminal safety is independent of logger ownership. This also preserves
    // a logger or panic hook that an embedding application installed first.
    crate::display::common::tui::ensure_terminal_panic_hook();
}

/// Applies the persisted `[logging]` settings: sets the max level and prunes
/// stale daily files. Called after the user config is loaded (usage / analysis).
pub fn apply(cfg: &LoggingConfig) {
    let filter = to_level_filter(cfg.level);
    log::set_max_level(filter);
    if let Some(logger) = LOGGER.get() {
        logger.level.store(filter as usize, Ordering::Relaxed);
    }
    prune_old_logs(cfg.retention_days);
}

/// Prunes daily log files older than `retention_days` from the log directory.
fn prune_old_logs(retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    let Some(dir) = LOGGER.get().and_then(|l| l.dir.as_deref()) else {
        return;
    };
    let cutoff = Utc::now().date_naive() - Duration::days(retention_days as i64);
    prune_before(dir, cutoff);
}

/// Deletes every `vct-YYYY-MM-DD.log` file in `dir` whose date is strictly
/// before `cutoff` (test seam: the cutoff is injected rather than derived from
/// "now").
fn prune_before(dir: &Path, cutoff: NaiveDate) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date_str) = name
            .strip_prefix(LOG_FILE_PREFIX)
            .and_then(|s| s.strip_suffix(LOG_FILE_SUFFIX))
        else {
            continue;
        };
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            && date < cutoff
        {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn format_line_shapes_the_record() {
        let line = format_line(
            "2026-07-12T10:30:15.123456789Z",
            Level::Warn,
            "vibe_coding_tracker::quota::claude",
            "quota fetch failed: HTTP 403",
        );
        assert_eq!(
            line,
            "2026-07-12T10:30:15.123456789Z WARN  vct::quota::claude  quota fetch failed: HTTP 403\n"
        );
    }

    #[test]
    fn normalize_target_shortens_only_our_crate() {
        assert_eq!(normalize_target("vibe_coding_tracker"), "vct");
        assert_eq!(
            normalize_target("vibe_coding_tracker::pricing"),
            "vct::pricing"
        );
        assert_eq!(normalize_target("hyper_util::client"), "hyper_util::client");
    }

    #[test]
    fn level_filter_ordering_matches_log_crate() {
        // Off gates out even Error; Warn admits Error+Warn but not Info.
        let warn = FileLogger::new(None, LevelFilter::Warn);
        assert!(warn.enabled(&Metadata::builder().level(Level::Error).build()));
        assert!(warn.enabled(&Metadata::builder().level(Level::Warn).build()));
        assert!(!warn.enabled(&Metadata::builder().level(Level::Info).build()));

        let off = FileLogger::new(None, LevelFilter::Off);
        assert!(!off.enabled(&Metadata::builder().level(Level::Error).build()));
    }

    #[test]
    fn append_creates_dir_lazily_and_writes() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("logs");
        let logger = FileLogger::new(Some(dir.clone()), LevelFilter::Warn);

        // Nothing on disk until the first append.
        assert!(!dir.exists());

        logger.append("line one\n");
        logger.append("line two\n");

        let file = dir.join(format!("vct-{}.log", utc_date()));
        let contents = fs::read_to_string(&file).unwrap();
        assert_eq!(contents, "line one\nline two\n");
    }

    #[test]
    fn log_routes_through_facade_gated_and_formatted() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("logs");
        let logger = FileLogger::new(Some(dir.clone()), LevelFilter::Warn);

        // Below the gate (info < warn): dropped, and nothing is created on disk.
        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("vibe_coding_tracker::x")
                .args(format_args!("skip me"))
                .build(),
        );
        assert!(
            !dir.exists(),
            "info is below warn, so no file/dir is created"
        );

        // At the gate (warn): written through the full log() -> format -> append path.
        logger.log(
            &Record::builder()
                .level(Level::Warn)
                .target("vibe_coding_tracker::quota")
                .args(format_args!("boom"))
                .build(),
        );
        let file = dir.join(format!("vct-{}.log", utc_date()));
        let contents = fs::read_to_string(&file).unwrap();
        assert!(
            contents.contains(" WARN  vct::quota  boom\n"),
            "unexpected line: {contents}"
        );
        assert!(
            !contents.contains("skip me"),
            "gated record must not appear"
        );
    }

    #[test]
    fn prune_before_removes_only_older_dated_files() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        for name in ["vct-2026-07-01.log", "vct-2026-07-10.log", "unrelated.txt"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let cutoff = NaiveDate::from_ymd_opt(2026, 7, 5).unwrap();
        prune_before(dir, cutoff);

        assert!(!dir.join("vct-2026-07-01.log").exists(), "old file pruned");
        assert!(dir.join("vct-2026-07-10.log").exists(), "recent file kept");
        assert!(dir.join("unrelated.txt").exists(), "non-log file untouched");
    }
}
