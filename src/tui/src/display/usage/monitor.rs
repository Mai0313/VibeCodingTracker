//! The quota-only TUI behind `vct monitor`.
//!
//! It starts the same background quota workers as the usage view and draws
//! nothing but their cards, full screen, with bars stretched to the card width.
//! No session is scanned, so it is up as soon as the cached snapshots load.

use super::interactive::{
    GridSpec, QuotaRuntime, QuotaView, dim_line, fit_cards, no_cards_reason, render_quota_card,
};
use crate::display::common::table::render_too_small;
use crate::display::common::tui::{InputAction, TerminalSession, handle_input};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vct_core::config::ProvidersConfig;

/// The monitor's grid: two cards across an 80-column terminal, three at 120.
const MONITOR_GRID: GridSpec = GridSpec {
    min_w: 40,
    max_w: 72,
    height: 7,
};
/// Below one card plus the header and footer rows only a notice is drawn.
const MONITOR_MIN_W: u16 = MONITOR_GRID.min_w;
const MONITOR_MIN_H: u16 = MONITOR_GRID.height + 2;

/// Runs the quota monitor until the user quits (`q`, `Esc` or `Ctrl+C`).
///
/// `quota_panels`, `providers` and `quota_refresh_secs` mean what they do for
/// the usage view: which cards to show and how often their workers poll.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or restored, if reading a
/// terminal input event fails, or if a frame fails to draw.
pub fn display_quota_monitor(
    quota_panels: Vec<String>,
    providers: ProvidersConfig,
    quota_refresh_secs: u64,
) -> anyhow::Result<()> {
    let mut terminal = TerminalSession::new()?;
    let result = (|| -> anyhow::Result<()> {
        let runtime = QuotaRuntime::start(&quota_panels, providers, quota_refresh_secs);
        let mut last_draw = None;
        loop {
            // Once a second: the clock ticks, and a worker's new snapshot shows
            // up without waiting for a key.
            if last_draw.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(1)) {
                let (claude, codex, copilot) = (
                    snapshot(&runtime.claude),
                    snapshot(&runtime.codex),
                    snapshot(&runtime.copilot),
                );
                let (cursor, grok, antigravity) = (
                    snapshot(&runtime.cursor),
                    snapshot(&runtime.grok),
                    snapshot(&runtime.antigravity),
                );
                let quota = QuotaView {
                    claude: &claude,
                    codex: &codex,
                    copilot: &copilot,
                    cursor: &cursor,
                    grok: &grok,
                    antigravity: &antigravity,
                    present: runtime.present,
                    band_enabled: runtime.band_enabled,
                    provider_panel_open: false,
                    overlay_open: false,
                };
                let now = chrono::Local::now();
                let clock = now.format("%H:%M:%S").to_string();
                terminal
                    .terminal_mut()
                    .draw(|f| render_monitor(f, &quota, now.timestamp(), &clock))?;
                last_draw = Some(Instant::now());
            }
            match handle_input()? {
                InputAction::Quit | InputAction::Close => break,
                InputAction::Resize => last_draw = None,
                _ => {}
            }
        }
        Ok(())
    })();
    terminal.finish(result)
}

/// A copy of a worker's latest snapshot.
fn snapshot<T: Clone + Default>(shared: &Mutex<T>) -> T {
    shared.lock().map(|value| value.clone()).unwrap_or_default()
}

/// Draws one monitor frame: a title row, the card grid centered in what is
/// left, and a footer that counts any provider the terminal had no room for.
fn render_monitor(f: &mut Frame, quota: &QuotaView, now: i64, clock: &str) {
    let area = f.area();
    if area.width < MONITOR_MIN_W || area.height < MONITOR_MIN_H {
        render_too_small(f, MONITOR_MIN_W, MONITOR_MIN_H);
        return;
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let dim = Style::default().fg(Color::DarkGray);

    f.render_widget(
        Line::from(Span::styled(
            " Quota Monitor",
            Style::default().fg(Color::Cyan).bold(),
        )),
        header,
    );
    f.render_widget(
        Line::from(Span::styled(format!("{clock} "), dim)).right_aligned(),
        header,
    );

    let (cards, hidden) = fit_cards(&MONITOR_GRID, body, quota, now, true);
    if cards.is_empty() {
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .areas(body);
        f.render_widget(
            Paragraph::new(dim_line(no_cards_reason(quota))).centered(),
            middle,
        );
    }
    // The grid is laid out from the top of the body; push it down by half of
    // whatever it left unused.
    let used = cards.last().map_or(body.y, |(cell, _)| cell.bottom());
    let shift = (body.bottom() - used) / 2;
    for (cell, card) in &cards {
        let cell = Rect {
            y: cell.y + shift,
            ..*cell
        };
        render_quota_card(f, cell, card);
    }

    let mut controls = vec![
        Span::styled("q", Style::default().fg(Color::Red).bold()),
        Span::styled(" quit", dim),
    ];
    if hidden > 0 {
        controls.push(Span::styled(
            format!("  |  +{hidden} hidden, enlarge the terminal"),
            Style::default().fg(Color::Yellow),
        ));
    }
    f.render_widget(Line::from(controls).centered(), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use vct_core::models::{ClaudeQuotaSnapshot, QuotaWindow};

    /// Renders a monitor frame at `width` x `height` with `present` providers
    /// drawn from default snapshots, Claude's 5h window half used.
    fn frame(width: u16, height: u16, present: usize, band_enabled: bool) -> String {
        let claude = ClaudeQuotaSnapshot {
            five_hour: Some(QuotaWindow {
                used_percent: 50.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let (codex, copilot, cursor) = Default::default();
        let (grok, antigravity) = Default::default();
        let mut quota = QuotaView {
            claude: &claude,
            codex: &codex,
            copilot: &copilot,
            cursor: &cursor,
            grok: &grok,
            antigravity: &antigravity,
            present: Default::default(),
            band_enabled,
            provider_panel_open: false,
            overlay_open: false,
        };
        let flags = [
            &mut quota.present.claude,
            &mut quota.present.codex,
            &mut quota.present.copilot,
            &mut quota.present.cursor,
            &mut quota.present.grok,
            &mut quota.present.antigravity,
        ];
        for flag in flags.into_iter().take(present) {
            *flag = true;
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| render_monitor(f, &quota, 0, "12:34:56"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn bars_stretch_with_the_card() {
        let narrow = frame(40, 12, 1, true);
        let wide = frame(72, 12, 1, true);
        let filled = |frame: &str| frame.matches('▰').count();
        assert!(filled(&narrow) > 3, "{narrow}");
        assert!(filled(&wide) > filled(&narrow), "{wide}");
    }

    #[test]
    fn every_provider_fits_an_80_by_24_terminal() {
        let rendered = frame(80, 24, 6, true);
        for title in [
            "Claude",
            "Codex",
            "Copilot",
            "Cursor",
            "Grok",
            "Antigravity",
        ] {
            assert!(rendered.contains(title), "{title} missing:\n{rendered}");
        }
        assert!(!rendered.contains("hidden"), "{rendered}");
    }

    #[test]
    fn providers_that_do_not_fit_are_counted() {
        let rendered = frame(80, 16, 6, true);
        assert!(rendered.contains("+2 hidden"), "{rendered}");
    }

    #[test]
    fn an_empty_monitor_says_why() {
        assert!(frame(80, 24, 0, true).contains("No provider on this machine"));
        assert!(frame(80, 24, 0, false).contains("Quota panels are off"));
    }

    #[test]
    fn a_terminal_below_the_minimum_only_says_so() {
        assert!(frame(39, 24, 1, true).contains("Terminal too small"));
    }
}
