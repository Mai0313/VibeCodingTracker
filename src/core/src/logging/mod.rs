//! File-based diagnostic logging (`~/.vct/logs/vct-YYYY-MM-DD.log`).
//!
//! Implements the [`log`] facade and writes plain-text UTC-dated daily files under `~/.vct/logs`; records are never written to stdout or stderr. Both the directory and the daily file are created lazily on the first emitted record, and files roll over across UTC midnight.
//!
//! [`init`] installs the global logger at `warn`, and [`apply`] updates the active filter and prunes expired files from [`LoggingConfig`].

use crate::config::{LogLevel, LoggingConfig};
use crate::utils::now_rfc3339_utc_nanos;
use chrono::{Duration, NaiveDate, Utc};
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::cell::RefCell;
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

/// Open daily log file paired with its `YYYY-MM-DD` UTC date for midnight rollover.
struct OpenLog {
    date: String,
    file: File,
}

/// Cap retained capacity of the reusable thread-local formatting buffer after an unusually large log line.
const MAX_RETAINED_BUF_BYTES: usize = 16 * 1024;
const SHRUNK_BUF_BYTES: usize = 4 * 1024;

thread_local! {
    /// Per-thread scratch buffer so record formatting never holds the global file mutex (preventing re-entrant deadlocks from `Display`/`Debug` callbacks that log).
    static FORMAT_BUF: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Thread-safe [`log::Log`] backend writing unbuffered records to daily UTC log files.
struct FileLogger {
    /// Active [`LevelFilter`] discriminant, kept in sync with [`log::set_max_level`].
    level: AtomicUsize,
    /// Target log directory, or `None` when the home directory is unavailable (logging no-ops).
    dir: Option<PathBuf>,
    /// Lazily opened daily log file, rotated on UTC date change.
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

    /// Writes `bytes` to `dir`'s daily log file for `date`, lazily creating the directory and rotating on UTC date change.
    fn write_to_date(dir: &Path, open: &mut Option<OpenLog>, date: &str, bytes: &[u8]) {
        if open.as_ref().is_none_or(|o| o.date != date) {
            if fs::create_dir_all(dir).is_err() {
                return;
            }
            let path = dir.join(format!("{LOG_FILE_PREFIX}{date}{LOG_FILE_SUFFIX}"));
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => {
                    *open = Some(OpenLog {
                        date: date.to_string(),
                        file,
                    })
                }
                Err(_) => return,
            }
        }
        if let Some(open) = open.as_mut() {
            let _ = open.file.write_all(bytes);
        }
    }

    /// Appends one preformatted line to the file for `date`.
    fn append_bytes_for_date(&self, date: &str, bytes: &[u8]) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        // A poisoned mutex still holds a valid file handle; recover and keep logging.
        let mut guard = self.open.lock().unwrap_or_else(|p| p.into_inner());
        Self::write_to_date(dir, &mut guard, date, bytes);
    }

    /// Appends one preformatted line to the file for `date`.
    #[cfg(test)]
    fn append_for_date(&self, date: &str, line: &str) {
        self.append_bytes_for_date(date, line.as_bytes());
    }

    /// Appends one preformatted line using the current UTC date.
    #[cfg(test)]
    fn append(&self, line: &str) {
        self.append_for_date(&utc_date(), line);
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() as usize <= self.level.load(Ordering::Relaxed)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) || self.dir.is_none() {
            return;
        }
        let now = now_rfc3339_utc_nanos();
        let date = utc_date_from_rfc3339(&now);
        // Format outside the file mutex using a reusable thread-local buffer, falling back to a fresh local buffer on re-entrant `Display`/`Debug` logging.
        FORMAT_BUF.with(|cell| match cell.try_borrow_mut() {
            Ok(mut buf) => {
                buf.clear();
                write_formatted_line(
                    &mut buf,
                    &now,
                    record.level(),
                    record.target(),
                    record.args(),
                );
                self.append_bytes_for_date(date, buf.as_bytes());
                if buf.capacity() > MAX_RETAINED_BUF_BYTES {
                    buf.clear();
                    buf.shrink_to(SHRUNK_BUF_BYTES);
                }
            }
            Err(_) => {
                let mut fallback = String::new();
                write_formatted_line(
                    &mut fallback,
                    &now,
                    record.level(),
                    record.target(),
                    record.args(),
                );
                self.append_bytes_for_date(date, fallback.as_bytes());
            }
        });
    }

    fn flush(&self) {
        // Same contract as `log`: a poisoned mutex still holds a valid file
        // handle, so recover instead of turning flush into a permanent no-op.
        let mut guard = self.open.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(open) = guard.as_mut() {
            let _ = open.file.flush();
        }
    }
}

/// Extracts the `YYYY-MM-DD` UTC date prefix from an RFC 3339 timestamp.
fn utc_date_from_rfc3339(timestamp: &str) -> &str {
    timestamp.split('T').next().unwrap_or(timestamp)
}

/// Returns the current UTC date as `YYYY-MM-DD` to match the UTC line timestamps.
#[cfg(test)]
fn utc_date() -> String {
    utc_date_from_rfc3339(&now_rfc3339_utc_nanos()).to_string()
}

/// Appends one formatted log line `<utc-nanos> <LEVEL> <target>  <message>\n` into `buf` with `vct_core` shortened to `vct`.
fn write_formatted_line(
    buf: &mut String,
    now: &str,
    level: Level,
    target: &str,
    args: &std::fmt::Arguments<'_>,
) {
    use std::fmt::Write as _;

    let _ = match target.strip_prefix("vct_core") {
        Some(rest) => writeln!(buf, "{now} {level:<5} vct{rest}  {args}"),
        None => writeln!(buf, "{now} {level:<5} {target}  {args}"),
    };
}

/// Formats one log line as `<utc-nanos> <LEVEL> <target>  <message>\n` with `vct_core` shortened to `vct`.
#[cfg(test)]
fn format_line(now: &str, level: Level, target: &str, msg: &str) -> String {
    let mut buf = String::new();
    write_formatted_line(&mut buf, now, level, target, &format_args!("{msg}"));
    buf
}

/// Rewrites the core crate's module-path target from `vct_core[...]` to
/// `vct[...]`; leaves third-party targets untouched.
#[cfg(test)]
fn normalize_target(target: &str) -> String {
    match target.strip_prefix("vct_core") {
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

/// Installs the process-wide file logger at `warn`; repeat calls are ignored.
///
/// Call [`apply`] after loading configuration to update the level and prune old logs.
pub fn init() {
    let dir = home::home_dir().map(|home| home.join(".vct").join(LOG_DIR_NAME));
    let logger = LOGGER.get_or_init(|| FileLogger::new(dir, LevelFilter::Warn));
    // `set_logger` succeeds only on the first install for the whole process.
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Warn);
    }
}

/// Applies `cfg` by updating the active log level and pruning daily log files older than `cfg.retention_days`.
pub fn apply(cfg: &LoggingConfig) {
    let filter = to_level_filter(cfg.level);
    log::set_max_level(filter);
    if let Some(logger) = LOGGER.get() {
        logger.level.store(filter as usize, Ordering::Relaxed);
    }
    prune_old_logs(cfg.retention_days);
}

/// Prunes daily log files older than `retention_days` from the log directory; `0` disables pruning.
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

/// Deletes every `vct-YYYY-MM-DD.log` file in `dir` whose date is strictly before `cutoff`.
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
            "vct_core::quota::claude",
            "quota fetch failed: HTTP 403",
        );
        assert_eq!(
            line,
            "2026-07-12T10:30:15.123456789Z WARN  vct::quota::claude  quota fetch failed: HTTP 403\n"
        );
    }

    #[test]
    fn normalize_target_shortens_only_our_crate() {
        assert_eq!(normalize_target("vct_core"), "vct");
        assert_eq!(normalize_target("vct_core::pricing"), "vct::pricing");
        assert_eq!(normalize_target("hyper_util::client"), "hyper_util::client");
    }

    #[test]
    fn level_filter_ordering_matches_log_crate() {
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

        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("vct_core::x")
                .args(format_args!("skip me"))
                .build(),
        );
        assert!(
            !dir.exists(),
            "info is below warn, so no file/dir is created"
        );

        logger.log(
            &Record::builder()
                .level(Level::Warn)
                .target("vct_core::quota")
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

    #[test]
    fn utc_date_prefix_and_midnight_rollover_split_daily_files() {
        assert_eq!(
            utc_date_from_rfc3339("2026-07-12T23:59:59.999999999Z"),
            "2026-07-12"
        );
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("logs");
        let logger = FileLogger::new(Some(dir.clone()), LevelFilter::Warn);

        logger.append_for_date("2026-07-12", "day one\n");
        logger.append_for_date("2026-07-13", "day two\n");

        assert_eq!(
            fs::read_to_string(dir.join("vct-2026-07-12.log")).unwrap(),
            "day one\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("vct-2026-07-13.log")).unwrap(),
            "day two\n"
        );
    }

    #[test]
    fn reentrant_display_logging_does_not_deadlock() {
        struct Reentrant<'a>(&'a FileLogger);
        impl std::fmt::Display for Reentrant<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.log(
                    &Record::builder()
                        .level(Level::Warn)
                        .target("vct_core::inner")
                        .args(format_args!("inner message"))
                        .build(),
                );
                f.write_str("outer message")
            }
        }

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("logs");
        let logger = FileLogger::new(Some(dir.clone()), LevelFilter::Warn);
        let payload = Reentrant(&logger);
        logger.log(
            &Record::builder()
                .level(Level::Warn)
                .target("vct_core::outer")
                .args(format_args!("{payload}"))
                .build(),
        );

        let file = dir.join(format!("vct-{}.log", utc_date()));
        let contents = fs::read_to_string(&file).unwrap();
        assert!(
            contents.contains("vct::inner  inner message\n"),
            "{contents}"
        );
        assert!(
            contents.contains("vct::outer  outer message\n"),
            "{contents}"
        );
    }
}
