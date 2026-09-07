//! The ledger scan of Cursor's per-conversation chat stores.
//!
//! Every store is its own session and its own source, so it is stamped and
//! settled like a session log; the two halves are read by different queries
//! (usage reads only the gauge nodes, analysis also the tool-result blobs),
//! so each half carries its own stamp. Every stamp also carries the shared
//! model-attribution database, whose change re-reads every store.

use super::compact::{CompactSink, cutoff_string, fold_days, fold_present, fold_unseen};
use super::database::{DatabaseHalfRead, group_usage_rows};
use super::{ScanDiagnostics, ScanFeature};
use crate::constants::{FastHashMap, FastHashSet};
use crate::ledger::key::relative_key;
use crate::ledger::{DayEntry, ScanStamp, SessionLedger, stamp, today};
use crate::models::{ExtensionType, TimeRange};
use crate::session::ParseMode;
use crate::session::cursor::{
    discover_cursor_store_dbs, load_conversation_model_snapshot, read_cursor_usage_store,
    read_store_analysis,
};
use crate::session::sqlite::is_cacheable_sqlite_failure;
use crate::utils::{HelperPaths, get_current_user, get_machine_id};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// One store's `feature` half from one read.
struct StoreHalfRead {
    days: BTreeMap<String, DayEntry>,
    parsed: bool,
    failure: Option<String>,
}

impl From<DatabaseHalfRead> for StoreHalfRead {
    fn from(read: DatabaseHalfRead) -> Self {
        Self {
            days: read
                .sessions
                .into_values()
                .next()
                .map(|session| session.days)
                .unwrap_or_default(),
            parsed: read.parsed,
            failure: read.failure,
        }
    }
}

fn read_store_half(
    store: &Path,
    feature: ScanFeature,
    conv_models: &FastHashMap<String, String>,
    user: &str,
    machine: &str,
) -> Result<StoreHalfRead> {
    match feature {
        ScanFeature::Usage => read_cursor_usage_store(store, conv_models, TimeRange::All)
            .map(|read| group_usage_rows(read, "Cursor usage payloads").into()),
        ScanFeature::Analysis => {
            let read = read_store_analysis(
                store,
                conv_models,
                TimeRange::All,
                ParseMode::UsageOnly,
                user,
                machine,
            )?;
            let complete_failure = read.normalized_messages == 0 && read.failed_payloads > 0;
            let failure = if complete_failure {
                Some(format!(
                    "none of {} analyzer payloads used a supported schema",
                    read.failed_payloads
                ))
            } else if read.failed_payloads > 0 {
                Some(format!(
                    "{} analyzer payloads used an unsupported schema",
                    read.failed_payloads
                ))
            } else {
                None
            };
            let mut days: BTreeMap<String, DayEntry> = BTreeMap::new();
            for (date, analysis) in read.rows {
                days.entry(date)
                    .or_default()
                    .add_analysis_records(&analysis);
            }
            Ok(StoreHalfRead {
                days,
                parsed: !complete_failure,
                failure,
            })
        }
    }
}

/// Scans every Cursor chat store's `feature` half through the ledger.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_cursor_stores(
    paths: &HelperPaths,
    feature: ScanFeature,
    tiers: Option<String>,
    time_range: TimeRange,
    ledger: &mut SessionLedger,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
) {
    let provider = ExtensionType::Cursor;
    let chats_dir = paths.cursor_chats_dir.as_path();
    let tracking_db = paths.cursor_tracking_db.as_path();
    let cutoff = cutoff_string(time_range);
    let mut book = ledger.take_provider(provider);

    if !chats_dir.exists() {
        book.settle_all_missing(&today());
        let retained = fold_unseen(
            provider,
            &book,
            &FastHashSet::default(),
            cutoff.as_deref(),
            sink,
            diagnostics,
        );
        ledger.record_retained(retained);
        ledger.put_provider(provider, book);
        return;
    }

    let discovery = discover_cursor_store_dbs(chats_dir);
    let partial = !discovery.failures.is_empty();
    for failure in discovery.failures {
        diagnostics.candidates += 1;
        diagnostics.record_hard_failure(provider, &failure.path, failure.error);
    }

    // A store's model comes from the tracking database, read once per scan.
    // When that read fails the stores are still read and folded, but nothing
    // is stamped or written: an attribution made without it is not one worth
    // keeping.
    let (conv_models, tracking_fingerprint, tracking_ok) =
        match load_conversation_model_snapshot(tracking_db) {
            Ok(snapshot) => (snapshot.models, snapshot.fingerprint, true),
            Err(error) => {
                diagnostics.record_hard_failure(provider, tracking_db, format!("{error:#}"));
                (FastHashMap::default(), None, false)
            }
        };
    let user = get_current_user();
    let machine = get_machine_id();
    let roots = [chats_dir];
    let mut seen = FastHashSet::default();

    for store in discovery.stores {
        diagnostics.candidates += 1;
        let key = relative_key(&roots, &store);
        seen.insert(key.clone());
        let current = if tracking_ok {
            match stamp::sqlite_source(&store) {
                Ok(mut files) => {
                    stamp::add_dependency(&mut files, tracking_db, tracking_fingerprint.as_ref());
                    Some(ScanStamp::new(tiers.clone(), files))
                }
                Err(error) => {
                    diagnostics.record_hard_failure(provider, &store, error.to_string());
                    continue;
                }
            }
        } else {
            None
        };
        if let Some(current) = &current
            && let Some(entry) = book.sessions.get(&key)
            && entry.scanned.is_current(feature, current)
        {
            fold_present(
                provider,
                &store,
                entry,
                feature,
                cutoff.as_deref(),
                sink,
                diagnostics,
            );
            book.mark_present(&key);
            continue;
        }

        ledger.record_parses(1);
        match read_store_half(&store, feature, &conv_models, &user, machine) {
            Ok(read) => {
                let Some(current) = current else {
                    if read.parsed {
                        diagnostics.parsed += 1;
                        fold_days(provider, &read.days, cutoff.as_deref(), sink);
                    }
                    if let Some(failure) = read.failure {
                        diagnostics.record_failure(provider, &store, failure);
                    }
                    continue;
                };
                let entry = book.sessions.entry(key).or_default();
                entry.replace_half(feature, read.days);
                entry
                    .scanned
                    .set_half(feature, current.with_verdict(read.parsed, read.failure));
                entry.missing_since = None;
                fold_present(
                    provider,
                    &store,
                    entry,
                    feature,
                    cutoff.as_deref(),
                    sink,
                    diagnostics,
                );
                book.dirty = true;
            }
            Err(error) => {
                let failure = format!("{error:#}");
                diagnostics.record_hard_failure(provider, &store, failure.clone());
                if let Some(current) = current
                    && is_cacheable_sqlite_failure(&error)
                {
                    let entry = book.sessions.entry(key).or_default();
                    entry
                        .scanned
                        .set_half(feature, current.with_verdict(false, Some(failure)));
                    book.dirty = true;
                }
            }
        }
    }

    book.settle_unseen(&seen, !partial, &today());
    let retained = fold_unseen(provider, &book, &seen, cutoff.as_deref(), sink, diagnostics);
    ledger.record_retained(retained);
    ledger.put_provider(provider, book);
}
