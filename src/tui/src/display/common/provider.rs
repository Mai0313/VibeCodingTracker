//! Display styling for a single provider row (label + per-renderer colors).

use crate::display::common::style::provider_color;
use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;
use vct_core::models::Provider;

/// Per-provider display configuration paired with the provider's totals.
///
/// Bundles a borrowed stats value (`T`) with the label and the matching color
/// for each renderer, so the TUI ([`tui_color`](Self::tui_color)) and the
/// static table ([`table_color`](Self::table_color)) stay visually consistent
/// for a given provider. Construct via [`ProviderTotal::new`] (a known
/// provider) or [`ProviderTotal::new_overall`] (the summary row).
pub struct ProviderTotal<'a, T> {
    /// Human-readable provider name shown in the row.
    pub label: &'static str,
    /// Color of this provider's `●` marker in the TUI.
    pub tui_color: RatatuiColor,
    /// Foreground color used when rendering this row in a static table.
    pub table_color: TableColor,
    /// Borrowed per-provider totals to render.
    pub stats: &'a T,
    /// Whether the row should be bold (used for the "All Providers" summary).
    pub emphasize: bool,
}

impl<'a, T> ProviderTotal<'a, T> {
    /// Creates a per-provider total row with the provider's canonical label and colors.
    ///
    /// The label comes from [`Provider::display_name`]. The TUI color is the
    /// shared [`provider_color`] map — the one the whole TUI uses — while the
    /// static table keeps its own, because comfy-table paints a whole cell
    /// rather than a marker. The caller chooses whether to `emphasize` (bold)
    /// the row.
    pub fn new(provider: Provider, stats: &'a T, emphasize: bool) -> Self {
        let table_color = match provider {
            Provider::ClaudeCode => TableColor::Cyan,
            Provider::Codex => TableColor::Yellow,
            Provider::Copilot => TableColor::Green,
            Provider::Gemini => TableColor::Blue,
            Provider::OpenCode => TableColor::Red,
            Provider::Cursor => TableColor::DarkMagenta,
            Provider::Hermes => TableColor::DarkYellow,
            Provider::Grok => TableColor::DarkRed,
            Provider::DeepSeek => TableColor::DarkCyan,
            Provider::Unknown => TableColor::Grey,
        };

        Self {
            label: provider.display_name(),
            tui_color: provider_color(provider),
            table_color,
            stats,
            emphasize,
        }
    }

    /// Creates the "All Providers" summary row (sum across every provider).
    ///
    /// Always magenta and always emphasized, to set it apart from the
    /// per-provider rows.
    pub fn new_overall(stats: &'a T) -> Self {
        Self {
            label: "All Providers",
            tui_color: RatatuiColor::Magenta,
            table_color: TableColor::Magenta,
            stats,
            emphasize: true,
        }
    }
}
