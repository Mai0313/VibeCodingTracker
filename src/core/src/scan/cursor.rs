//! Ledger-backed scanner for Cursor's per-conversation SQLite chat stores.
//!
//! Each chat store is tracked as an independent session with separate usage and analysis stamps. Every stamp includes the shared model-attribution tracking database; changes to that database invalidate all stores.

use super::compact::{CompactSink, cutoff_string, fold_days, fold_present, fold_unseen};
use super::database::{DatabaseHalfRead, group_usage_rows};
use super::{ScanDiagnostics, ScanFeature};
use crate::constants::{FastHashMap, FastHashSet};
use crate::ledger::key::relative_key;
use crate::ledger::{DayEntry, ScanStamp, SessionLedger, TinyMap, stamp, today};
use crate::models::{ExtensionType, TimeRange};
use crate::session::ParseMode;
use crate::session::cursor::{
    discover_cursor_store_dbs, load_conversation_model_snapshot, read_cursor_usage_store,
    read_store_analysis,
};
use crate::session::sqlite::{is_cacheable_sqlite_failure, optional_database_fingerprint};
use crate::utils::{HelperPaths, get_current_user, get_machine_id};
use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

/// One store's `feature` half from one read.
struct StoreHalfRead {
    days: TinyMap<String, DayEntry>,
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
            .map(|read| group_usage_rows(read, "Cursor usage payloads", false).into()),
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
            let mut days: TinyMap<String, DayEntry> = TinyMap::new();
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

    // Fingerprint the shared tracking DB upfront; only open and query it when at least one store misses the ledger (or when no stores exist to validate its readability).
    let (initial_tracking_fp, tracking_ok) = match optional_database_fingerprint(tracking_db) {
        Ok(fingerprint) => (fingerprint, true),
        Err(error) => {
            diagnostics.record_hard_failure(provider, tracking_db, format!("{error:#}"));
            (None, false)
        }
    };
    let discovery_empty = discovery.stores.is_empty();
    let roots = [chats_dir];
    let mut seen = FastHashSet::default();
    let mut misses = Vec::new();

    for store in discovery.stores {
        diagnostics.candidates += 1;
        let key = relative_key(&roots, &store);
        seen.insert(key.clone());
        let current = if tracking_ok {
            match stamp::sqlite_source(&store) {
                Ok(mut files) => {
                    stamp::add_dependency(&mut files, tracking_db, initial_tracking_fp.as_ref());
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
        misses.push((store, key, current));
    }

    let conv_models = if tracking_ok
        && (!misses.is_empty() || (discovery_empty && initial_tracking_fp.is_some()))
    {
        match load_conversation_model_snapshot(tracking_db) {
            Ok(snapshot) => {
                if snapshot.fingerprint != initial_tracking_fp {
                    for (_, _, current) in &mut misses {
                        if let Some(stamp) = current {
                            stamp::add_dependency(
                                &mut stamp.files,
                                tracking_db,
                                snapshot.fingerprint.as_ref(),
                            );
                        }
                    }
                }
                snapshot.models
            }
            Err(error) => {
                diagnostics.record_hard_failure(provider, tracking_db, format!("{error:#}"));
                for (_, _, current) in &mut misses {
                    *current = None;
                }
                FastHashMap::default()
            }
        }
    } else {
        FastHashMap::default()
    };

    if !misses.is_empty() {
        let user = get_current_user();
        let machine = get_machine_id();
        ledger.record_parses(misses.len());
        let reads: Vec<_> = if misses.len() >= 2 {
            misses
                .into_par_iter()
                .map(|(store, key, current)| {
                    let read = read_store_half(&store, feature, &conv_models, &user, machine);
                    (store, key, current, read)
                })
                .collect()
        } else {
            misses
                .into_iter()
                .map(|(store, key, current)| {
                    let read = read_store_half(&store, feature, &conv_models, &user, machine);
                    (store, key, current, read)
                })
                .collect()
        };

        for (store, key, current, read) in reads {
            match read {
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
    }

    book.settle_unseen(&seen, !partial, &today());
    let retained = fold_unseen(provider, &book, &seen, cutoff.as_deref(), sink, diagnostics);
    ledger.record_retained(retained);
    ledger.put_provider(provider, book);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::compact::NoopSink;
    use crate::utils::resolve_paths_from_home;
    use rusqlite::Connection;

    fn seed_store(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); \
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn warm_cursor_scan_defers_opening_tracking_db() {
        let dir = tempfile::tempdir().unwrap();
        let paths = resolve_paths_from_home(dir.path());
        seed_store(&paths.cursor_chats_dir.join("proj/conv-a/store.db"));
        seed_store(&paths.cursor_chats_dir.join("proj/conv-b/store.db"));
        std::fs::create_dir_all(paths.cursor_tracking_db.parent().unwrap()).unwrap();
        {
            let conn = Connection::open(&paths.cursor_tracking_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE ai_code_hashes (conversationId TEXT, model TEXT); \
                 INSERT INTO ai_code_hashes VALUES ('conv-a', 'model-a'), ('conv-b', 'model-b');",
            )
            .unwrap();
        }

        let mut ledger = SessionLedger::new();
        let mut sink = NoopSink;
        let mut cold_diag = ScanDiagnostics::default();
        scan_cursor_stores(
            &paths,
            ScanFeature::Usage,
            None,
            TimeRange::All,
            &mut ledger,
            &mut sink,
            &mut cold_diag,
        );
        assert_eq!(cold_diag.candidates, 2);
        assert_eq!(cold_diag.parsed, 2);
        assert!(cold_diag.failures.is_empty());
        assert_eq!(ledger.stats().parsed_sources, 2);

        // On Unix, removing read permission leaves `len` and `mtime` intact so `stat` matches while opening `tracking.db` would fail.
        #[cfg(unix)]
        struct RestorePerms<'a>(&'a Path);
        #[cfg(unix)]
        impl Drop for RestorePerms<'_> {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o644));
            }
        }
        #[cfg(unix)]
        let _guard = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &paths.cursor_tracking_db,
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
            RestorePerms(&paths.cursor_tracking_db)
        };

        ledger.begin_local_scan();
        let mut warm_diag = ScanDiagnostics::default();
        scan_cursor_stores(
            &paths,
            ScanFeature::Usage,
            None,
            TimeRange::All,
            &mut ledger,
            &mut sink,
            &mut warm_diag,
        );
        assert_eq!(warm_diag.candidates, 2);
        assert_eq!(warm_diag.parsed, 2);
        assert!(warm_diag.failures.is_empty(), "{:?}", warm_diag.failures);
        assert_eq!(ledger.stats().parsed_sources, 0);
    }
}
