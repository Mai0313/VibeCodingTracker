//! Analysis summary rendering helpers.
//!
//! Re-exports [`vct_core::analysis::summary`] and builds borrowed [`ProviderTotal`] rows for the table and TUI renderers.

pub use vct_core::analysis::summary::*;

use crate::display::common::ProviderTotal;
use vct_core::models::Provider;

/// Returns per-provider total rows for display, borrowing from `totals`.
///
/// Includes one row per analysis provider with at least one active day, followed by an emphasized "All Providers" row whenever overall activity is non-zero or no individual provider matched.
pub fn build_analysis_provider_rows(
    totals: &AnalysisProviderTotals,
) -> Vec<ProviderTotal<'_, AnalysisProviderStats>> {
    let mut rows = Vec::with_capacity(10); // 9 analysis providers (Hermes is usage-only) + overall

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

    if totals.deepseek.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::DeepSeek,
            &totals.deepseek,
            false,
        ));
    }

    if totals.antigravity.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::Antigravity,
            &totals.antigravity,
            false,
        ));
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
