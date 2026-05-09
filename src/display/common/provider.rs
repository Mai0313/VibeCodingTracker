use crate::models::Provider;
use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;

/// Per-provider display configuration paired with the provider's totals.
pub struct ProviderTotal<'a, T> {
    pub label: &'static str,
    pub icon: &'static str,
    pub tui_color: RatatuiColor,
    pub table_color: TableColor,
    pub stats: &'a T,
    pub emphasize: bool,
}

impl<'a, T> ProviderTotal<'a, T> {
    /// Create a new per-provider total row.
    pub fn new(provider: Provider, stats: &'a T, emphasize: bool) -> Self {
        let (label, icon, tui_color, table_color) = match provider {
            Provider::ClaudeCode => (
                Provider::ClaudeCode.display_name(),
                Provider::ClaudeCode.icon(),
                RatatuiColor::Cyan,
                TableColor::Cyan,
            ),
            Provider::Codex => (
                Provider::Codex.display_name(),
                Provider::Codex.icon(),
                RatatuiColor::Yellow,
                TableColor::Yellow,
            ),
            Provider::Copilot => (
                Provider::Copilot.display_name(),
                Provider::Copilot.icon(),
                RatatuiColor::Green,
                TableColor::Green,
            ),
            Provider::Gemini => (
                Provider::Gemini.display_name(),
                Provider::Gemini.icon(),
                RatatuiColor::LightBlue,
                TableColor::Blue,
            ),
            Provider::Unknown => ("Unknown", "❓", RatatuiColor::Gray, TableColor::Grey),
        };

        Self {
            label,
            icon,
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
            icon: "⭐",
            tui_color: RatatuiColor::Magenta,
            table_color: TableColor::Magenta,
            stats,
            emphasize: true,
        }
    }
}
