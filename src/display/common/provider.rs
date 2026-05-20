use crate::models::Provider;
use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;

/// Per-provider display configuration paired with the provider's totals.
pub struct ProviderTotal<'a, T> {
    pub label: &'static str,
    pub tui_color: RatatuiColor,
    pub table_color: TableColor,
    pub stats: &'a T,
    pub emphasize: bool,
}

impl<'a, T> ProviderTotal<'a, T> {
    /// Create a new per-provider total row.
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

    /// Create the "overall" total row (sum across all providers).
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
