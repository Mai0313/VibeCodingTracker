//! Usage summary rendering helpers.
//!
//! The priced, aggregated summary (rows, totals, per-provider totals) is core
//! business logic in [`vct_core::usage::summary`], re-exported here. This
//! module adds only the display-only provider-total rows, which borrow into the
//! comfy-table / ratatui renderers.

pub use vct_core::usage::summary::*;

use crate::display::common::ProviderTotal;
use vct_core::models::Provider;

/// Builds the per-provider footer rows, borrowing from `totals`.
///
/// A provider with no active days is skipped; the overall row is appended when
/// it has active days or when no provider qualified, so the result is never
/// empty.
pub fn build_provider_total_rows(
    totals: &UsageProviderTotals,
) -> Vec<ProviderTotal<'_, ProviderStats>> {
    let mut rows = Vec::with_capacity(10); // max 9 providers + overall

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

    if totals.hermes.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Hermes, &totals.hermes, false));
    }

    if totals.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderTotal::new_overall(&totals.overall));
    }

    rows
}
