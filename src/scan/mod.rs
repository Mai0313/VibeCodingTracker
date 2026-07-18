//! Shared provider-scan primitives for the `usage` and `analysis` roll-ups.
//!
//! Both features discover the same provider sources, parse each one, and record
//! the same candidate / parsed / failure diagnostics. This module owns the
//! parts that do not depend on which feature is folding: the unified
//! [`ScanDiagnostics`] result type and the dedicated scan thread pool.

use crate::models::ExtensionType;
use anyhow::Result;
use std::path::PathBuf;

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
