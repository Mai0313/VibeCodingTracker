use crate::models::Provider;
use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;

/// Provider-specific display configuration
pub struct ProviderAverage<'a, T> {
    pub label: &'static str,
    pub icon: &'static str,
    pub tui_color: RatatuiColor,
    pub table_color: TableColor,
    pub stats: &'a T,
    pub emphasize: bool,
}

impl<'a, T> ProviderAverage<'a, T> {
    /// Create a new provider average display configuration
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

    /// Create an "overall" provider average (for all providers combined)
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
