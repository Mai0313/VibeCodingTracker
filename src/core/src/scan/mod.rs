//! Shared provider-scan primitives for the `usage` and `analysis` roll-ups.
//!
//! Owns the unified [`ScanDiagnostics`] and [`ScanFailure`] types, the ledger-backed file and database scanners, and the dedicated scan thread pool ([`build_scan_pool`]).

pub(crate) mod compact;
pub(crate) mod cursor;
pub(crate) mod database;
pub(crate) mod descriptor;

pub(crate) use crate::ledger::ScanFeature;
pub(crate) use compact::{CompactSink, fold_remotes};
pub(crate) use cursor::scan_cursor_stores;
pub(crate) use database::{group_usage_rows, scan_database_half};
pub(crate) use descriptor::{refresh_ledger_for_export, scan_all_cached_files};

use crate::models::ExtensionType;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// One independently readable source that could not be collected.
///
/// A source is one session file, one Cursor store, or one provider database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure {
    /// Provider whose file, database, or store collection failed.
    pub provider: ExtensionType,
    /// File or collection root passed to the parser or database reader.
    pub source: PathBuf,
    /// Parser or reader error, or the reason a parsed result was rejected.
    pub error: String,
}

/// Candidate, parsed, retained, and failure counts for one scan.
///
/// A candidate is the smallest independently readable source. `parsed` counts successfully read candidates (including valid empty sources), not individual sessions emitted by a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanDiagnostics {
    /// Number of independently readable sources discovered.
    pub candidates: usize,
    /// Number of candidates parsed or read successfully.
    pub parsed: usize,
    /// Sessions folded from the ledger whose source is no longer on disk.
    pub retained: usize,
    /// Failures in deterministic provider and source order.
    pub failures: Vec<ScanFailure>,
}

impl ScanDiagnostics {
    /// Returns whether at least one source failed or was only partially parsed.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Returns whether candidates existed with zero parsed sources and zero retained ledger sessions.
    pub fn all_failed(&self) -> bool {
        self.candidates > 0 && self.parsed == 0 && self.retained == 0
    }

    /// Returns whether at least one source parsed alongside one or more failures.
    pub fn partially_failed(&self) -> bool {
        self.parsed > 0 && self.has_failures()
    }

    /// Appends a failure without logging; used for retained ledger verdicts re-observed across refreshes.
    pub(crate) fn record_failure(&mut self, provider: ExtensionType, source: &Path, error: String) {
        self.failures.push(ScanFailure {
            provider,
            source: source.to_path_buf(),
            error,
        });
    }

    /// Logs a newly observed discovery or read failure to the log file and appends it to [`Self::failures`].
    pub(crate) fn record_hard_failure(
        &mut self,
        provider: ExtensionType,
        source: &Path,
        error: String,
    ) {
        log::warn!(
            "failed to collect {} session from {}: {}",
            provider,
            source.display(),
            error
        );
        self.record_failure(provider, source, error);
    }

    /// Sorts [`Self::failures`] deterministically by provider scan rank, source path, and error text.
    pub(crate) fn finalize(&mut self) {
        self.failures.sort_by(|left, right| {
            left.provider
                .scan_rank()
                .cmp(&right.provider.scan_rank())
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.error.cmp(&right.error))
        });
    }
}

/// Builds a dedicated Rayon thread pool for provider scans with at least one worker thread.
///
/// Isolates scan parallelism from Rayon's global pool; `threads` values of `0` are clamped to `1`.
///
/// # Errors
///
/// Returns an error if Rayon fails to spawn the thread pool.
pub fn build_scan_pool(threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|index| format!("vct-scan-{index}"))
        .build()
        .map_err(Into::into)
}
