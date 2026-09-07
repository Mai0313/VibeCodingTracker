//! The ledger scan of a provider whose sessions all live in one SQLite
//! database (OpenCode, Hermes).
//!
//! One read produces every session at once, so the read stamp lives on the
//! provider rather than on each session: when it is current every session is
//! folded from the ledger, and when it is stale the whole database is
//! re-read, each returned session's `feature` half replaced whole, and a
//! session the read no longer returns is marked missing and kept.

use super::compact::{CompactSink, cutoff_string, fold_days};
use super::{ScanDiagnostics, ScanFeature};
use crate::constants::FastHashSet;
use crate::ledger::{DayEntry, ProviderLedger, ScanStamp, SessionLedger, stamp, today};
use crate::models::{ExtensionType, TimeRange};
use crate::session::diagnostics::{DatabaseAnalysisRow, DatabaseUsageRead};
use crate::session::sqlite::is_cacheable_sqlite_failure;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// One half of every session a database read produced.
pub(crate) struct DatabaseHalfRead {
    /// Session id → that session's `feature` half, by day.
    pub(crate) sessions: BTreeMap<String, SessionRead>,
    /// Whether the read produced usable data.
    pub(crate) parsed: bool,
    /// The read's diagnostic, beside its data or in place of it.
    pub(crate) failure: Option<String>,
}

/// One session's half from one database read.
#[derive(Default)]
pub(crate) struct SessionRead {
    pub(crate) cwd: Option<String>,
    pub(crate) days: BTreeMap<String, DayEntry>,
}

/// Groups a usage read's rows by session and day.
///
/// `noun` names the rows in the failure reason (`usage records`, `Cursor usage
/// payloads`), which the CLI shows verbatim. `records_cost` says whether the
/// provider prices its own rows (OpenCode, Hermes); a provider that does not
/// (Cursor) gets no stored-cost map written at all.
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
    let mut sessions: BTreeMap<String, SessionRead> = BTreeMap::new();
    for row in read.rows {
        let session = sessions.entry(row.session_id).or_default();
        if row.cwd.is_some() {
            session.cwd = row.cwd;
        }
        session.days.entry(row.date).or_default().add_usage_row(
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
    let mut sessions: BTreeMap<String, SessionRead> = BTreeMap::new();
    for row in rows {
        let session = sessions.entry(row.session_id).or_default();
        if let Some(record) = row.analysis.records.first()
            && !record.folder_path.is_empty()
        {
            session.cwd = Some(record.folder_path.clone());
        }
        session
            .days
            .entry(row.date)
            .or_default()
            .add_analysis_records(&row.analysis);
    }
    DatabaseHalfRead {
        sessions,
        parsed,
        failure,
    }
}

/// Scans one whole-database provider's `feature` half through the ledger.
///
/// `read` runs only when the database's stamp is stale; it must read every
/// session regardless of time range, since the range is applied to the folded
/// days. A database that is not there marks every session missing and folds
/// them all, so uninstalling the assistant loses nothing.
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

    // Whether this scan's sessions come from a read that succeeded (now or
    // when the stamp was written) rather than from the ledger alone. A
    // database that exists but cannot be read still serves what it held: the
    // sessions are real usage, and they count as retained rather than parsed
    // so the scan neither reads as all-failed nor claims a read it did not get.
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
                // A read that understood nothing says nothing about which
                // sessions are still there, so it marks none of them missing.
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

/// Folds every session of `book`, counting as retained the ones served from
/// the ledger alone: every session when `from_ledger` (the database is gone
/// or unreadable), otherwise those whose source is marked missing. A session
/// holding no days contributes nothing and is not counted.
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
