//! The TUI's whole style vocabulary.
//!
//! Hierarchy comes from the terminal's own palette — its default foreground
//! plus `DIM` and `BOLD` — rather than from a color per kind of data, so the
//! view reads the same way under a dark and a light theme. On top of that sit
//! one accent, reserved for what the user can act on, and three status colors.
//! Every color is an ANSI index, which is what makes the terminal's own theme
//! resolve it; `clippy.toml` denies `Color::Rgb` and `Color::Indexed` so a later
//! change cannot quietly reintroduce a hard-coded shade.

use ratatui::style::{Color, Modifier, Style};
use vct_core::models::Provider;

/// The one accent color. It marks what can be acted on (the selected row, a
/// login hint) and nothing else, so it never competes with the data.
pub const ACCENT: Color = Color::Cyan;

/// Percentage at which a gauge starts asking to be noticed.
const ATTENTION_PCT: f64 = 75.0;
/// Percentage at which a gauge reads as spent.
const ALERT_PCT: f64 = 90.0;

/// Primary text: whatever foreground the terminal is set to.
pub fn primary() -> Style {
    Style::default()
}

/// Secondary text — column names, units, ages, key descriptions — dimmed so the
/// numbers beside it carry the row.
pub fn secondary() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Titles, keys and the one number a line is read for.
pub fn strong() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// The accent, for the selection marker and anything the user is being told to
/// run.
pub fn accent() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// A spent window, a dead token, a terminal too small to draw into.
pub fn alert() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

/// Worth noticing but not yet wrong: a gauge three-quarters spent, a snapshot
/// that has gone stale, a refresh that is still running.
pub fn attention() -> Style {
    Style::default().fg(Color::Yellow)
}

/// A row whose tokens grew since the last tick.
pub fn active() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

/// A gauge's style at `pct` consumed: plain while there is room, then
/// [`attention`], then [`alert`].
pub fn gauge(pct: f64) -> Style {
    if pct >= ALERT_PCT {
        alert()
    } else if pct >= ATTENTION_PCT {
        attention()
    } else {
        primary()
    }
}

/// The single per-provider color used anywhere in the TUI.
///
/// On a line of text it is only ever painted on a provider's `●` marker, never
/// on its name or its numbers, so the color identifies the row without deciding
/// how it reads. The one other use is the `p` panel's stacked share bar, where
/// the color *is* the data: each provider's slice of the bar is identified by
/// nothing else.
pub fn provider_color(provider: Provider) -> Color {
    match provider {
        Provider::ClaudeCode => Color::Cyan,
        Provider::Codex => Color::Yellow,
        Provider::Copilot => Color::Green,
        Provider::Gemini => Color::LightBlue,
        Provider::OpenCode => Color::Red,
        Provider::Cursor => Color::LightMagenta,
        Provider::Hermes => Color::LightYellow,
        Provider::Grok => Color::LightRed,
        Provider::DeepSeek => Color::LightCyan,
        Provider::Unknown => Color::Gray,
    }
}

/// The `● ` marker for a provider, the one place its color is used.
pub fn provider_marker(provider: Provider) -> ratatui::text::Span<'static> {
    ratatui::text::Span::styled("● ", Style::default().fg(provider_color(provider)))
}

#[cfg(test)]
mod tests {
    use super::{ALERT_PCT, ATTENTION_PCT, alert, attention, gauge, primary};

    #[test]
    fn gauge_escalates_at_its_two_thresholds() {
        assert_eq!(gauge(0.0), primary());
        assert_eq!(gauge(ATTENTION_PCT - 0.1), primary());
        assert_eq!(gauge(ATTENTION_PCT), attention());
        assert_eq!(gauge(ALERT_PCT - 0.1), attention());
        assert_eq!(gauge(ALERT_PCT), alert());
        assert_eq!(gauge(100.0), alert());
    }
}
