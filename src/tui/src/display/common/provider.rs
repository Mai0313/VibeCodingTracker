//! Display styling for a single provider row (label + per-renderer colors).

use comfy_table::Color as TableColor;
use ratatui::style::Color as RatatuiColor;
use vct_core::models::Provider;

/// Per-provider display configuration paired with borrowed totals (`T`).
///
/// Pairs a provider's label with matching [`tui_color`](Self::tui_color) and [`table_color`](Self::table_color) values across renderers. Construct via [`ProviderTotal::new`] or [`ProviderTotal::new_overall`].
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

/// Returns the TUI foreground color for `provider`.
pub const fn provider_tui_color(provider: Provider) -> RatatuiColor {
    match provider {
        Provider::ClaudeCode => RatatuiColor::Cyan,
        Provider::Codex => RatatuiColor::Yellow,
        Provider::Copilot => RatatuiColor::Green,
        Provider::Gemini => RatatuiColor::LightBlue,
        Provider::OpenCode => RatatuiColor::Red,
        Provider::Cursor => RatatuiColor::LightMagenta,
        Provider::Hermes => RatatuiColor::LightYellow,
        Provider::Grok => RatatuiColor::LightRed,
        Provider::DeepSeek => RatatuiColor::LightCyan,
        Provider::Antigravity => RatatuiColor::Blue,
        Provider::Unknown => RatatuiColor::Gray,
    }
}

impl<'a, T> ProviderTotal<'a, T> {
    /// Creates a per-provider total row with `provider`'s canonical label and renderer colors.
    pub fn new(provider: Provider, stats: &'a T, emphasize: bool) -> Self {
        let tui_color = provider_tui_color(provider);
        let (label, table_color) = match provider {
            Provider::ClaudeCode => (Provider::ClaudeCode.display_name(), TableColor::Cyan),
            Provider::Codex => (Provider::Codex.display_name(), TableColor::Yellow),
            Provider::Copilot => (Provider::Copilot.display_name(), TableColor::Green),
            Provider::Gemini => (Provider::Gemini.display_name(), TableColor::Blue),
            Provider::OpenCode => (Provider::OpenCode.display_name(), TableColor::Red),
            Provider::Cursor => (Provider::Cursor.display_name(), TableColor::DarkMagenta),
            Provider::Hermes => (Provider::Hermes.display_name(), TableColor::DarkYellow),
            Provider::Grok => (Provider::Grok.display_name(), TableColor::DarkRed),
            Provider::DeepSeek => (Provider::DeepSeek.display_name(), TableColor::DarkCyan),
            Provider::Antigravity => (Provider::Antigravity.display_name(), TableColor::DarkBlue),
            Provider::Unknown => ("Unknown", TableColor::Grey),
        };

        Self {
            label,
            tui_color,
            table_color,
            stats,
            emphasize,
        }
    }

    /// Creates an emphasized magenta "All Providers" summary row.
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
