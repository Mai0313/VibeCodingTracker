//! Data-driven provider fan-out for the cached file scan.
//!
//! The six file-backed providers are scanned with the identical
//! [`scan_cached_files`] call, differing only in their session roots, filter,
//! depth cap, and enable toggle. Listing them once here means adding a provider
//! is a single table row instead of a new `if` block in every scan loop.

use super::{CompactSink, ScanDiagnostics, scan_cached_files};
use crate::config::ProvidersConfig;
use crate::constants::FastHashSet;
use crate::models::ExtensionType;
use crate::models::TimeRange;
use crate::pricing::TierThresholds;
use crate::summary_cache::{SummaryCacheKey, SummaryScanCache};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, DSH_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths,
    is_claude_session_file, is_codex_session_file, is_copilot_session_file, is_dsh_session_file,
    is_gemini_session_file, is_grok_session_file,
};
use anyhow::Result;
use std::path::Path;

/// One file-backed provider's scan parameters.
///
/// `dirs` is a list because a provider can keep its sessions in more than one
/// root — Codex splits them across active and archived directories.
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
const FILE_PROVIDERS: [FileProviderSpec; 6] = [
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
];

/// Scans every enabled file-backed provider through the incremental cache,
/// folding each into `sink`. Shared by the usage and analysis cached collectors.
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
                &(spec.dirs)(paths),
                spec.provider,
                spec.filter,
                time_range,
                spec.max_depth,
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
