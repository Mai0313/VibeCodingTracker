//! Data-driven provider fan-out for the cached file scan.
//!
//! The five file-backed providers are scanned with the identical
//! [`scan_cached_files`](super::scan_cached_files) call, differing only in their
//! discovery function and enable toggle. Listing them once here means
//! adding a provider is a single table row instead of a new `if` block in every
//! scan loop.

use super::{CompactSink, ScanDiagnostics, scan_cached_files};
use crate::config::ProvidersConfig;
use crate::constants::FastHashSet;
use crate::models::ExtensionType;
use crate::models::TimeRange;
use crate::pricing::TierThresholds;
use crate::summary_cache::{SummaryCacheKey, SummaryScanCache};
use crate::utils::directory::{
    FileDiscovery, collect_codex_session_files_with_diagnostics,
    collect_files_with_max_depth_diagnostics,
};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths, is_claude_session_file,
    is_copilot_session_file, is_gemini_session_file, is_grok_session_file,
};
use anyhow::Result;

/// One file-backed provider's scan parameters.
struct FileProviderSpec {
    provider: ExtensionType,
    enabled: fn(&ProvidersConfig) -> bool,
    discover: fn(&HelperPaths, TimeRange) -> FileDiscovery,
}

/// The file-backed providers, in canonical scan order.
///
/// Database-backed providers (OpenCode, Cursor, Hermes) are not here: each has a
/// bespoke reader that differs between the usage and analysis features.
const FILE_PROVIDERS: [FileProviderSpec; 5] = [
    FileProviderSpec {
        provider: ExtensionType::ClaudeCode,
        enabled: |p| p.claude,
        discover: |p, time_range| {
            collect_files_with_max_depth_diagnostics(
                &p.claude_session_dir,
                is_claude_session_file,
                time_range,
                None,
            )
        },
    },
    FileProviderSpec {
        provider: ExtensionType::Codex,
        enabled: |p| p.codex,
        discover: |p, time_range| {
            let archived_session_dir = p.codex_archived_session_dir();
            collect_codex_session_files_with_diagnostics(
                &p.codex_session_dir,
                &archived_session_dir,
                time_range,
            )
        },
    },
    FileProviderSpec {
        provider: ExtensionType::Copilot,
        enabled: |p| p.copilot,
        discover: |p, time_range| {
            collect_files_with_max_depth_diagnostics(
                &p.copilot_session_dir,
                is_copilot_session_file,
                time_range,
                Some(COPILOT_SESSION_MAX_DEPTH),
            )
        },
    },
    FileProviderSpec {
        provider: ExtensionType::Gemini,
        enabled: |p| p.gemini,
        discover: |p, time_range| {
            collect_files_with_max_depth_diagnostics(
                &p.gemini_session_dir,
                is_gemini_session_file,
                time_range,
                None,
            )
        },
    },
    FileProviderSpec {
        provider: ExtensionType::Grok,
        enabled: |p| p.grok,
        discover: |p, time_range| {
            collect_files_with_max_depth_diagnostics(
                &p.grok_session_dir,
                is_grok_session_file,
                time_range,
                Some(GROK_SESSION_MAX_DEPTH),
            )
        },
    },
];

/// Scans every enabled file-backed provider through the incremental cache,
/// folding each into `sink`. Replaces the per-provider `if` ladder in both the
/// usage and analysis cached collectors.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_all_cached_files(
    paths: &HelperPaths,
    providers: ProvidersConfig,
    time_range: TimeRange,
    cache: &mut SummaryScanCache,
    seen: &mut FastHashSet<SummaryCacheKey>,
    sink: &mut impl CompactSink,
    diagnostics: &mut ScanDiagnostics,
    tiers: Option<&TierThresholds>,
) -> Result<()> {
    for spec in &FILE_PROVIDERS {
        if (spec.enabled)(&providers) {
            scan_cached_files(
                (spec.discover)(paths, time_range),
                spec.provider,
                time_range,
                cache,
                seen,
                sink,
                diagnostics,
                tiers,
            )?;
        }
    }
    Ok(())
}
