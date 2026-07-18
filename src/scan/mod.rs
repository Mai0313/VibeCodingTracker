//! Shared provider-scan primitives for the `usage` and `analysis` roll-ups.
//!
//! Both features discover the same provider sources, parse each one, and record
//! the same candidate / parsed / failure diagnostics. This module owns the
//! parts that do not depend on which feature is folding: the unified
//! [`ScanDiagnostics`] result type and the dedicated scan thread pool.

pub(crate) mod compact;
pub(crate) mod descriptor;

pub(crate) use compact::{
    CompactSink, LoadedCompactSummary, fold_cached, fold_loaded, scan_cached_files,
};
pub(crate) use descriptor::scan_all_cached_files;

use crate::models::ExtensionType;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// One independently readable source that could not be collected.
///
/// A source is the smallest unit this layer reads on its own: one session file,
/// one Cursor store, or one provider database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure {
    /// Provider whose file, database, or store collection failed.
    pub provider: ExtensionType,
    /// File or collection root passed to the parser or database reader.
    pub source: PathBuf,
    /// Parser or reader error, or the reason a parsed result was rejected.
    pub error: String,
}

/// Candidate, success, and failure counts for one scan.
///
/// A candidate is the smallest independently readable source. `parsed` counts
/// candidates read successfully (including valid blank sources), not the number
/// of sessions a database emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanDiagnostics {
    /// Number of independently readable sources discovered.
    pub candidates: usize,
    /// Number of candidates parsed or read successfully.
    pub parsed: usize,
    /// Failures in deterministic provider and source order.
    pub failures: Vec<ScanFailure>,
}

impl ScanDiagnostics {
    /// Whether at least one source failed or was only partially understood.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Whether candidates existed but none could be read successfully.
    pub fn all_failed(&self) -> bool {
        self.candidates > 0 && self.parsed == 0
    }

    /// Whether successful results coexist with one or more failures.
    pub fn partially_failed(&self) -> bool {
        self.parsed > 0 && self.has_failures()
    }

    /// Records one failed source without logging.
    ///
    /// Used for failures that are re-observed on every cached refresh (a
    /// retained parse verdict), so logging here would append the same line each
    /// tick. Direct discovery/read failures use [`record_hard_failure`] instead.
    ///
    /// [`record_hard_failure`]: Self::record_hard_failure
    pub(crate) fn record_failure(&mut self, provider: ExtensionType, source: &Path, error: String) {
        self.failures.push(ScanFailure {
            provider,
            source: source.to_path_buf(),
            error,
        });
    }

    /// Records a hard discovery/read failure and mirrors it to the daily log.
    ///
    /// For failures produced anew each scan (directory traversal, fingerprint
    /// I/O, a hard parser error) — not the retained verdicts folded from cache —
    /// so the log line matches one real failure per scan, as the analysis file
    /// scan did before it moved onto the shared scanner. The log is file-only
    /// (never stdout/stderr), so this stays TUI-safe.
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

    /// Sorts failures into the canonical, deterministic order:
    /// provider scan rank, then source path, then error text.
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

/// Builds the dedicated Rayon pool used by CLI scans.
///
/// Kept off Rayon's global pool so batch scans never contend with (or
/// reconfigure) the process-wide thread pool.
pub fn build_scan_pool(threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|index| format!("vct-scan-{index}"))
        .build()
        .map_err(Into::into)
}
