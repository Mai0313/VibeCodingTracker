//! Display styling for a single provider row (label + per-renderer colors).

use crate::models::Provider;
use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;

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
    /// Foreground color used when rendering this row in the TUI.
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
    /// The label comes from [`Provider::display_name`] and each provider maps to
    /// a fixed color pair (e.g. Claude → cyan, Codex → yellow). The caller
    /// chooses whether to `emphasize` (bold) the row.
    pub fn new(provider: Provider, stats: &'a T, emphasize: bool) -> Self {
        let (label, tui_color, table_color) = match provider {
            Provider::ClaudeCode => (
                Provider::ClaudeCode.display_name(),
                RatatuiColor::Cyan,
                TableColor::Cyan,
            ),
            Provider::Codex => (
                Provider::Codex.display_name(),
                RatatuiColor::Yellow,
                TableColor::Yellow,
            ),
            Provider::Copilot => (
                Provider::Copilot.display_name(),
                RatatuiColor::Green,
                TableColor::Green,
            ),
            Provider::Gemini => (
                Provider::Gemini.display_name(),
                RatatuiColor::LightBlue,
                TableColor::Blue,
            ),
            Provider::OpenCode => (
                Provider::OpenCode.display_name(),
                RatatuiColor::Red,
                TableColor::Red,
            ),
            Provider::Cursor => (
                Provider::Cursor.display_name(),
                RatatuiColor::LightMagenta,
                TableColor::DarkMagenta,
            ),
            Provider::Unknown => ("Unknown", RatatuiColor::Gray, TableColor::Grey),
        };

        Self {
            label,
            tui_color,
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
