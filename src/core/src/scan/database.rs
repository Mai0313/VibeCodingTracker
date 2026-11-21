//! Ledger-backed scanner for single-database providers (OpenCode and Hermes).
//!
//! One query returns every session, placing the scan stamp on the provider book rather than individual sessions. Stale reads replace each returned session's `feature` half and mark unreturned sessions missing while retaining their history.

use super::compact::{CompactSink, cutoff_string, fold_days};
use super::{ScanDiagnostics, ScanFeature};
use crate::constants::{FastHashMap, FastHashSet};
use crate::ledger::{DayEntry, ProviderLedger, ScanStamp, SessionLedger, TinyMap, stamp, today};
use crate::models::{ExtensionType, TimeRange};
use crate::session::diagnostics::{DatabaseAnalysisRow, DatabaseUsageRead};
use crate::session::sqlite::is_cacheable_sqlite_failure;
use anyhow::Result;
use std::path::Path;

/// One half of every session a database read produced.
pub(crate) struct DatabaseHalfRead {
    /// Session id → that session's `feature` half, by day.
    pub(crate) sessions: FastHashMap<String, SessionRead>,
    /// Whether the read produced usable data.
    pub(crate) parsed: bool,
    /// The read's diagnostic, beside its data or in place of it.
    pub(crate) failure: Option<String>,
}

/// One session's half from one database read.
#[derive(Default)]
pub(crate) struct SessionRead {
    pub(crate) cwd: Option<String>,
    pub(crate) days: TinyMap<String, DayEntry>,
}

/// Groups a [`DatabaseUsageRead`] by session and day, formatting schema-failure messages with `noun` and recording stored costs only when `records_cost` is true.
pub(crate) fn group_usage_rows(
    read: DatabaseUsageRead,
    noun: &str,
    records_cost: bool,
) -> DatabaseHalfRead {
    let complete_failure = read.expected_records > 0 && read.parsed_records == 0;
    let failed = read.failed_records();
    let failure = if complete_failure {
        Some(format!(
            "none of {} {noun} used a supported schema",
            read.expected_records
        ))
    } else if failed > 0 {
        Some(format!("{failed} {noun} used an unsupported schema"))
    } else {
        None
    };
    let mut sessions: FastHashMap<String, SessionRead> = FastHashMap::default();
    for row in read.rows {
        let session = match sessions.get_mut(&row.session_id) {
            Some(session) => session,
            None => sessions.entry(row.session_id).or_default(),
        };
        if row.cwd.is_some() {
            session.cwd = row.cwd;
        }
        let day = match session.days.get_mut(&row.date) {
            Some(day) => day,
            None => session.days.entry(row.date).or_default(),
        };
        day.add_usage_row(
            row.model,
            row.tokens,
            row.tier_level,
            records_cost.then_some(row.stored_cost),
        );
    }
    DatabaseHalfRead {
        sessions,
        parsed: !complete_failure,
        failure,
    }
}

/// Groups analysis rows by session and day; the caller supplies the verdict.
pub(crate) fn group_analysis_rows(
    rows: Vec<DatabaseAnalysisRow>,
    parsed: bool,
    failure: Option<String>,
) -> DatabaseHalfRead {
    let mut sessions: FastHashMap<String, SessionRead> = FastHashMap::default();
    for row in rows {
        let session = match sessions.get_mut(&row.session_id) {
            Some(session) => session,
            None => sessions.entry(row.session_id).or_default(),
        };
        if let Some(record) = row.analysis.records.first()
            && !record.folder_path.is_empty()
        {
            session.cwd = Some(record.folder_path.clone());
        }
        let day = match session.days.get_mut(&row.date) {
            Some(day) => day,
            None => session.days.entry(row.date).or_default(),
        };
        day.add_analysis_records(&row.analysis);
    }
    DatabaseHalfRead {
        sessions,
        parsed,
        failure,
    }
}

/// Reads OpenCode's analysis half in `UsageOnly` mode and groups it into a [`DatabaseHalfRead`].
pub(crate) fn read_opencode_analysis_half(db_path: &Path) -> Result<DatabaseHalfRead> {
    let read = crate::session::opencode::read_opencode_analysis_with_diagnostics(
        db_path,
        TimeRange::All,
        crate::session::ParseMode::UsageOnly,
    )?;
    let complete_failure = read.expected_records > 0 && read.parsed_records == 0;
    let failed = read.expected_records.saturating_sub(read.parsed_records) + read.failed_tool_parts;
    let failure = if complete_failure {
        Some(format!(
            "none of {} analysis records used a recognized schema",
            read.expected_records
        ))
    } else if failed > 0 {
        Some(format!(
            "{failed} analysis payloads used an unsupported schema"
        ))
    } else {
        None
    };
    Ok(group_analysis_rows(read.rows, !complete_failure, failure))
}

/// Scans one single-database provider's `feature` half through `ledger`, invoking `read` only when stale.
///
/// `read` must return sessions across all time; `time_range` is applied during folding, and a missing database marks all cached sessions missing while continuing to fold them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_database_half<R>(
    provider: ExtensionType,
    db_path: &Path,
    feature: ScanFeature,
    tiers: Option<String>,
    time_range: TimeRange,
    ledger: &mut SessionLedger,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    read: R,
) where
    R: FnOnce() -> Result<DatabaseHalfRead>,
{
    let cutoff = cutoff_string(time_range);
    let mut book = ledger.take_provider(provider);

    if !db_path.exists() {
        book.settle_all_missing(&today());
        let retained = fold_book(provider, &book, cutoff.as_deref(), true, sink, diagnostics);
        ledger.record_retained(retained);
        ledger.put_provider(provider, book);
        return;
    }

    diagnostics.candidates += 1;
    let current = match stamp::sqlite_source(db_path) {
        Ok(files) => ScanStamp::new(tiers, files),
        Err(error) => {
            diagnostics.record_hard_failure(provider, db_path, error.to_string());
            let retained = fold_book(provider, &book, cutoff.as_deref(), true, sink, diagnostics);
            ledger.record_retained(retained);
            ledger.put_provider(provider, book);
            return;
        }
    };

    // Track whether data came from a valid read; unreadable DBs still serve cached sessions as `retained` rather than `parsed` so the scan neither reports all-failed nor claims a fresh read.
    let read_ok;
    if let Some(stamp) = book.source.half(feature)
        && stamp.is_current(feature, &current)
    {
        read_ok = stamp.parsed;
        if stamp.parsed {
            diagnostics.parsed += 1;
        }
        if let Some(failure) = &stamp.failure {
            diagnostics.record_failure(provider, db_path, failure.clone());
        }
    } else {
        ledger.record_parses(1);
        match read() {
            Ok(read) => {
                let mut seen = FastHashSet::default();
                for (id, session) in read.sessions {
                    seen.insert(id.clone());
                    let entry = book.sessions.entry(id).or_default();
                    if session.cwd.is_some() {
                        entry.cwd = session.cwd;
                    }
                    entry.replace_half(feature, session.days);
                    entry.missing_since = None;
                }
                // Only settle unseen sessions when the read succeeded; a failed schema parse says nothing about deleted sessions.
                if read.parsed {
                    book.settle_unseen(&seen, true, &today());
                    diagnostics.parsed += 1;
                }
                read_ok = read.parsed;
                if let Some(failure) = &read.failure {
                    diagnostics.record_failure(provider, db_path, failure.clone());
                }
                book.source
                    .set_half(feature, current.with_verdict(read.parsed, read.failure));
                book.dirty = true;
            }
            Err(error) => {
                read_ok = false;
                let failure = format!("{error:#}");
                diagnostics.record_hard_failure(provider, db_path, failure.clone());
                if is_cacheable_sqlite_failure(&error) {
                    book.source
                        .set_half(feature, current.with_verdict(false, Some(failure)));
                    book.dirty = true;
                }
            }
        }
    }

    let retained = fold_book(
        provider,
        &book,
        cutoff.as_deref(),
        !read_ok,
        sink,
        diagnostics,
    );
    ledger.record_retained(retained);
    ledger.put_provider(provider, book);
}

/// Folds all sessions in `book` and returns the count of non-empty entries served as retained (all when `from_ledger` is true, else only missing ones).
fn fold_book(
    provider: ExtensionType,
    book: &ProviderLedger,
    cutoff: Option<&str>,
    from_ledger: bool,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) -> usize {
    let mut retained = 0;
    for entry in book.sessions.values() {
        fold_days(provider, &entry.days, cutoff, sink);
        if !entry.days.is_empty() && (from_ledger || entry.missing_since.is_some()) {
            retained += 1;
        }
    }
    diagnostics.retained += retained;
    retained
}
