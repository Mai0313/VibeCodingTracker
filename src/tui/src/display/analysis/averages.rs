//! Analysis summary rendering helpers.
//!
//! The aggregated per-provider summary is core business logic and lives in
//! [`vibe_coding_tracker::analysis::summary`]; it is re-exported here so the renderers keep
//! importing it as `crate::display::analysis::averages::<item>`. This module
//! adds only the display-only provider-total rows, which borrow into the
//! comfy-table / ratatui renderers.

pub use vibe_coding_tracker::analysis::summary::*;

use crate::display::common::ProviderTotal;
use vibe_coding_tracker::models::Provider;

/// Build the per-provider total rows for the display layer.
///
/// Emits one row per provider that has at least one active day, followed by an
/// emphasized "All Providers" overall row. The overall row is always appended
/// when there is overall activity, and also when no provider matched so the
/// table is never empty.
pub fn build_analysis_provider_rows(
    totals: &AnalysisProviderTotals,
) -> Vec<ProviderTotal<'_, AnalysisProviderStats>> {
    let mut rows = Vec::with_capacity(8); // max 7 providers + overall

    if totals.claude.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::ClaudeCode,
            &totals.claude,
            false,
        ));
    }

    if totals.codex.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Codex, &totals.codex, false));
    }

    if totals.copilot.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::Copilot,
            &totals.copilot,
            false,
        ));
    }

    if totals.gemini.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Gemini, &totals.gemini, false));
    }

    if totals.grok.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Grok, &totals.grok, false));
    }

    if totals.opencode.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::OpenCode,
            &totals.opencode,
            false,
        ));
    }

    if totals.cursor.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Cursor, &totals.cursor, false));
    }

    if totals.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderTotal::new_overall(&totals.overall));
    }

    rows
}
