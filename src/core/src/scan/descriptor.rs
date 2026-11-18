//! Data-driven provider fan-out for the ledger-backed file scan.
//!
//! The seven file-backed providers are scanned with the identical
//! [`scan_provider_files`] call, differing only in their session roots, filter,
//! depth cap, and enable toggle. Listing them once here means adding a provider
//! is a single table row instead of a new `if` block in every scan loop.

use super::compact::{
    NoopSink, ScanTarget, cutoff_string, finish_provider_scan, scan_provider_files,
};
use super::database::{group_usage_rows, read_opencode_analysis_half, scan_database_half};
use super::{CompactSink, ScanDiagnostics, ScanFeature, scan_cursor_stores};
use crate::config::ProvidersConfig;
use crate::ledger::{SessionLedger, stamp, today};
use crate::models::ExtensionType;
use crate::models::TimeRange;
use crate::pricing::TierThresholds;
use crate::session::hermes::read_hermes_usage_contributions;
use crate::session::opencode::read_opencode_usage_contributions;
use crate::utils::{
    ANTIGRAVITY_SESSION_MAX_DEPTH, COPILOT_SESSION_MAX_DEPTH, DSH_SESSION_MAX_DEPTH,
    GROK_SESSION_MAX_DEPTH, HelperPaths, is_antigravity_session_file, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_dsh_session_file, is_gemini_session_file,
    is_grok_session_file,
};
use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

/// One file-backed provider's scan parameters.
///
/// `dirs` is a list because a provider can keep its sessions in more than one
/// root — Codex splits them across active and archived directories, and
/// Antigravity across the `agy` CLI, Desktop and other builds.
struct FileProviderSpec {
    provider: ExtensionType,
    enabled: fn(&ProvidersConfig) -> bool,
    dirs: fn(&HelperPaths) -> Vec<&Path>,
    filter: fn(&Path) -> bool,
    max_depth: Option<usize>,
}

/// The file-backed providers, in canonical scan order.
///
/// Database-backed providers (OpenCode, Cursor, Hermes) are not here: each has a
/// bespoke reader that differs between the usage and analysis features.
const FILE_PROVIDERS: [FileProviderSpec; 7] = [
    FileProviderSpec {
        provider: ExtensionType::ClaudeCode,
        enabled: |p| p.claude,
        dirs: |p| vec![p.claude_session_dir.as_path()],
        filter: is_claude_session_file,
        max_depth: None,
    },
    FileProviderSpec {
        provider: ExtensionType::Codex,
        enabled: |p| p.codex,
        dirs: |p| p.codex_session_dirs().to_vec(),
        filter: is_codex_session_file,
        max_depth: None,
    },
    FileProviderSpec {
        provider: ExtensionType::Copilot,
        enabled: |p| p.copilot,
        dirs: |p| vec![p.copilot_session_dir.as_path()],
        filter: is_copilot_session_file,
        max_depth: Some(COPILOT_SESSION_MAX_DEPTH),
    },
    FileProviderSpec {
        provider: ExtensionType::Gemini,
        enabled: |p| p.gemini,
        dirs: |p| vec![p.gemini_session_dir.as_path()],
        filter: is_gemini_session_file,
        max_depth: None,
    },
    FileProviderSpec {
        provider: ExtensionType::Grok,
        enabled: |p| p.grok,
        dirs: |p| vec![p.grok_session_dir.as_path()],
        filter: is_grok_session_file,
        max_depth: Some(GROK_SESSION_MAX_DEPTH),
    },
    FileProviderSpec {
        provider: ExtensionType::DeepSeek,
        enabled: |p| p.dsh,
        dirs: |p| vec![p.dsh_session_dir.as_path()],
        filter: is_dsh_session_file,
        max_depth: Some(DSH_SESSION_MAX_DEPTH),
    },
    FileProviderSpec {
        provider: ExtensionType::Antigravity,
        enabled: |p| p.antigravity,
        dirs: |p| p.antigravity_session_dirs().to_vec(),
        filter: is_antigravity_session_file,
        max_depth: Some(ANTIGRAVITY_SESSION_MAX_DEPTH),
    },
];

/// Scans every enabled file-backed provider through the ledger, folding each
/// into `sink`. Shared by the usage and analysis ledger-backed collectors.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_all_cached_files(
    paths: &HelperPaths,
    providers: ProvidersConfig,
    time_range: TimeRange,
    ledger: &mut SessionLedger,
    feature: ScanFeature,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    tiers: Option<&TierThresholds>,
) -> Result<()> {
    scan_all_cached_files_target(
        paths,
        providers,
        time_range,
        ledger,
        ScanTarget::Single(feature),
        sink,
        diagnostics,
        tiers,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_all_cached_files_target(
    paths: &HelperPaths,
    providers: ProvidersConfig,
    time_range: TimeRange,
    ledger: &mut SessionLedger,
    target: ScanTarget,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    tiers: Option<&TierThresholds>,
) -> Result<()> {
    let mut enabled = Vec::with_capacity(FILE_PROVIDERS.len());
    for spec in &FILE_PROVIDERS {
        if (spec.enabled)(&providers) {
            enabled.push((spec, ledger.take_provider(spec.provider)));
        }
    }
    if enabled.is_empty() {
        return Ok(());
    }

    let cutoff = cutoff_string(time_range);
    let tiers_fingerprint = stamp::tiers_fingerprint(tiers);
    let today_str = today();

    let outcomes: Vec<_> = if enabled.len() >= 2 {
        enabled
            .into_par_iter()
            .map(|(spec, book)| {
                scan_provider_files(
                    &(spec.dirs)(paths),
                    spec.provider,
                    spec.filter,
                    spec.max_depth,
                    book,
                    target,
                    tiers,
                    tiers_fingerprint.as_deref(),
                    &today_str,
                )
            })
            .collect()
    } else {
        enabled
            .into_iter()
            .map(|(spec, book)| {
                scan_provider_files(
                    &(spec.dirs)(paths),
                    spec.provider,
                    spec.filter,
                    spec.max_depth,
                    book,
                    target,
                    tiers,
                    tiers_fingerprint.as_deref(),
                    &today_str,
                )
            })
            .collect()
    };

    for outcome in outcomes {
        finish_provider_scan(
            outcome,
            target,
            cutoff.as_deref(),
            ledger,
            sink,
            diagnostics,
        );
    }
    Ok(())
}

/// Refreshes both halves of every enabled provider in `ledger` for `vct ledger
/// export`, walking file providers once and skipping in-memory day folding and
/// remote snapshot reloads.
pub(crate) fn refresh_ledger_for_export(
    paths: &HelperPaths,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    tiers: Option<&TierThresholds>,
) -> Result<()> {
    ledger.begin_local_scan();
    let mut sink = NoopSink;
    let mut diagnostics = ScanDiagnostics::default();

    scan_all_cached_files_target(
        paths,
        providers,
        TimeRange::All,
        ledger,
        ScanTarget::BothHalves,
        &mut sink,
        &mut diagnostics,
        tiers,
    )?;

    if providers.opencode {
        scan_database_half(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            ScanFeature::Usage,
            stamp::tiers_fingerprint(tiers),
            TimeRange::All,
            ledger,
            &mut sink,
            &mut diagnostics,
            || {
                read_opencode_usage_contributions(&paths.opencode_db, TimeRange::All, tiers)
                    .map(|read| group_usage_rows(read, "usage records", true))
            },
        );
        scan_database_half(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            ScanFeature::Analysis,
            None,
            TimeRange::All,
            ledger,
            &mut sink,
            &mut diagnostics,
            || read_opencode_analysis_half(&paths.opencode_db),
        );
    }
    if providers.cursor {
        scan_cursor_stores(
            paths,
            ScanFeature::Usage,
            None,
            TimeRange::All,
            ledger,
            &mut sink,
            &mut diagnostics,
        );
        scan_cursor_stores(
            paths,
            ScanFeature::Analysis,
            None,
            TimeRange::All,
            ledger,
            &mut sink,
            &mut diagnostics,
        );
    }
    if providers.hermes {
        scan_database_half(
            ExtensionType::Hermes,
            &paths.hermes_db,
            ScanFeature::Usage,
            None,
            TimeRange::All,
            ledger,
            &mut sink,
            &mut diagnostics,
            || {
                read_hermes_usage_contributions(&paths.hermes_db, TimeRange::All)
                    .map(|read| group_usage_rows(read, "usage records", true))
            },
        );
    }
    Ok(())
}
