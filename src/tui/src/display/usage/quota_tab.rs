//! The Quota tab: nothing but the quota workers' cards, as large as the
//! terminal allows, with bars stretched to the card width.

use super::interactive::{
    GridSpec, QuotaView, dim_line, fit_cards, no_cards_reason, render_quota_card,
};
use crate::display::common::table::render_too_small;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// The tab's grid: two cards across an 80-column terminal, three at 120.
const QUOTA_GRID: GridSpec = GridSpec {
    min_w: 40,
    max_w: 72,
    height: 7,
};
/// Below one card plus the footer row only a notice is drawn.
const QUOTA_MIN_W: u16 = QUOTA_GRID.min_w;
const QUOTA_MIN_H: u16 = QUOTA_GRID.height + 1;

/// Draws the Quota tab into `area`: the card grid centered in it, and a footer
/// that counts any provider the terminal had no room for.
pub(crate) fn draw_quota_tab(f: &mut Frame, area: Rect, quota: &QuotaView, now: i64) {
    if area.width < QUOTA_MIN_W || area.height < QUOTA_MIN_H {
        render_too_small(f, QUOTA_MIN_W, QUOTA_MIN_H);
        return;
    }
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let dim = Style::default().fg(Color::DarkGray);

    let (cards, hidden) = fit_cards(&QUOTA_GRID, body, quota, now, true);
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

    /// Renders the tab at `width` x `height` with `present` providers
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
            .draw(|f| draw_quota_tab(f, f.area(), &quota, 0))
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
        let rendered = frame(80, 15, 6, true);
        assert!(rendered.contains("+2 hidden"), "{rendered}");
    }

    #[test]
    fn an_empty_tab_says_why() {
        assert!(frame(80, 24, 0, true).contains("No provider on this machine"));
        assert!(frame(80, 24, 0, false).contains("Quota panels are off"));
    }

    #[test]
    fn a_terminal_below_the_minimum_only_says_so() {
        assert!(frame(39, 24, 1, true).contains("Terminal too small"));
    }
}
