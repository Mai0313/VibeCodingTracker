//! Pluggable interactive TUI layouts, auto-discovered at compile time.
//!
//! `src/tui/build.rs` scans `src/tui/src/layouts/` for `<id>/mod.rs` folders and
//! standalone `<id>.rs` files, generating `$OUT_DIR/layouts_registry.rs` with
//! [`LAYOUTS`] and [`LAYOUT_IDS`]. Adding a new layout requires only dropping a
//! new module into `src/tui/src/layouts/` that exports
//! `pub static LAYOUT: <Type> = <Type>;` implementing [`TuiLayout`]; removing a
//! layout is deleting its folder or file.

use crate::display::analysis::AnalysisUiState;
use crate::display::app::Tab;
use crate::display::common::theme::theme;
use crate::display::common::tui::MouseInput;
use crate::display::settings::SettingsUiState;
use crate::display::usage::{QuotaView, UsageUiState};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use vct_core::models::TimeRange;

/// Static metadata describing a compiled-in TUI layout plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LayoutMeta {
    /// Stable identifier matching the module/folder name and `[general].layout`.
    pub(crate) id: &'static str,
    /// Human-readable display name.
    pub(crate) title: &'static str,
    /// One-line summary of the layout's structure.
    pub(crate) summary: &'static str,
}

/// Factory registered by each layout module under `src/tui/src/layouts/`.
pub(crate) trait TuiLayout: Sync {
    /// Metadata for this layout.
    fn meta(&self) -> LayoutMeta;

    /// Creates a fresh per-session view instance holding this layout's UI state.
    fn create(&self) -> Box<dyn LayoutView>;
}

/// Borrowed application state passed into [`LayoutView::draw`],
/// [`LayoutView::on_mouse`], and [`LayoutView::on_key`].
pub(crate) struct LayoutContext<'a> {
    pub(crate) tab: Tab,
    pub(crate) time_range: TimeRange,
    pub(crate) loading: bool,
    pub(crate) retry_pending: bool,
    pub(crate) spinner: usize,
    pub(crate) metrics: (f64, f32),
    pub(crate) data_status: Option<(&'a str, Color)>,
    pub(crate) update_status: Option<(&'a str, Color)>,
    pub(crate) usage: &'a mut UsageUiState,
    pub(crate) analysis: &'a mut AnalysisUiState,
    pub(crate) quota: Option<(QuotaView<'a>, i64)>,
    pub(crate) settings: &'a mut SettingsUiState,
}

/// State transitions a [`LayoutView`] can request from the host loop.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct LayoutCommand {
    /// Switch to another tab.
    pub(crate) switch_tab: Option<Tab>,
    /// Jump directly to a chosen time range.
    pub(crate) set_range: Option<TimeRange>,
    /// Step the time range backward (`-1`) or forward (`1`).
    pub(crate) step_range: Option<isize>,
    /// Trigger an immediate session re-scan.
    pub(crate) refresh: bool,
    /// Open the `?` keybinding help modal.
    pub(crate) open_help: bool,
    /// Propagate a `Settings` mutation saved during mouse handling.
    pub(crate) settings_changed: bool,
    /// Exit the interactive app.
    pub(crate) quit: bool,
}

/// Outcome of handing an input event to the active [`LayoutView`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum EventResult {
    /// The layout handled the event and optionally requests a host command.
    Consumed(LayoutCommand),
    /// The layout left the event to the host's default keybindings.
    Ignored,
}

impl EventResult {
    /// Shorthand for consuming an event with no extra host side effects.
    pub(crate) const fn consumed() -> Self {
        Self::Consumed(LayoutCommand {
            switch_tab: None,
            set_range: None,
            step_range: None,
            refresh: false,
            open_help: false,
            settings_changed: false,
            quit: false,
        })
    }
}

/// Per-session stateful instance of a TUI layout.
pub(crate) trait LayoutView {
    /// Renders the entire terminal frame (`f.area()`), including navigation
    /// chrome and the active tab's content.
    ///
    /// Keep the single-line controls footer on the bottom-most row of the frame
    /// (`y = f.area().bottom() - 1`) so the host's OSC 8 hyperlink pass wraps
    /// `Star on GitHub`.
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>);

    /// Handles mouse clicks and vertical scroll wheel events across the frame.
    fn on_mouse(&mut self, mouse: MouseInput, ctx: &mut LayoutContext<'_>) -> EventResult;

    /// Gives the layout first refusal on keys when not on `Tab::Settings`.
    fn on_key(&mut self, key: KeyCode, ctx: &mut LayoutContext<'_>) -> EventResult {
        let _ = (key, ctx);
        EventResult::Ignored
    }

    /// Clears transient overlays when the user switches tabs.
    fn on_tab_changed(&mut self, next: Tab, ctx: &mut LayoutContext<'_>) {
        let _ = (next, ctx);
    }

    /// Customizes the `(key, description)` rows shown in the `?` help modal for
    /// `tab`, or returns `None` to use the default list.
    fn help_rows(&self, tab: Tab) -> Option<Vec<(&'static str, &'static str)>> {
        let _ = tab;
        None
    }
}

/// Time ranges in display order, shared by top bars and sidebars.
pub(crate) const RANGES: [(TimeRange, &str); 4] = [
    (TimeRange::Daily, "day"),
    (TimeRange::Weekly, "week"),
    (TimeRange::Monthly, "month"),
    (TimeRange::All, "all"),
];

const TAB_PREFIX: &str = " vct ";

/// Hit-test regions recorded when drawing the horizontal top bar.
#[derive(Default, Clone, Copy)]
pub(crate) struct TopBarHits {
    tabs: [(Tab, Option<Rect>); 4],
    ranges: [(TimeRange, Option<Rect>); 4],
}

impl TopBarHits {
    /// Returns the tab clicked at `(column, row)`, if any.
    pub(crate) fn hit_tab(&self, column: u16, row: u16) -> Option<Tab> {
        self.tabs
            .iter()
            .find_map(|(tab, rect)| rect.filter(|r| contains(*r, column, row)).map(|_| *tab))
    }

    /// Returns the time range clicked at `(column, row)`, if any.
    pub(crate) fn hit_range(&self, column: u16, row: u16) -> Option<TimeRange> {
        self.ranges
            .iter()
            .find_map(|(range, rect)| rect.filter(|r| contains(*r, column, row)).map(|_| *range))
    }
}

/// Whether `(column, row)` lies inside `rect`.
#[inline]
pub(crate) fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

/// Renders the standard single-row horizontal navigation bar into `area` and
/// returns exact hit-test rectangles for every visible tab and time-range pill.
pub(crate) fn render_top_bar(
    f: &mut Frame,
    area: Rect,
    current: Tab,
    range: Option<TimeRange>,
    metrics: (f64, f32),
) -> TopBarHits {
    let mut hits = TopBarHits {
        tabs: Tab::ALL.map(|tab| (tab, None)),
        ranges: RANGES.map(|(r, _)| (r, None)),
    };
    if area.width == 0 || area.height == 0 {
        return hits;
    }

    let theme = theme();
    let dim = theme.dim();
    let width = usize::from(area.width);
    let mut left = vec![Span::styled(TAB_PREFIX, Style::default().bold())];
    let mut cursor = TAB_PREFIX.chars().count();

    for (idx, tab) in Tab::ALL.into_iter().enumerate() {
        let text = format!(" {} ", tab.label());
        let len = text.chars().count();
        if cursor + len <= width {
            hits.tabs[idx].1 = Some(Rect::new(area.x + cursor as u16, area.y, len as u16, 1));
        }
        cursor += len;
        left.push(if tab == current {
            Span::styled(text, theme.active())
        } else {
            Span::raw(text)
        });
    }

    let mut range_spans = Vec::new();
    let mut range_offsets: Vec<(usize, usize)> = Vec::new();
    if let Some(active_range) = range {
        let prefix = "range ";
        let mut r_cursor = prefix.chars().count();
        range_spans.push(Span::styled(prefix, dim));
        for (candidate, name) in RANGES {
            let text = format!(" {name} ");
            let len = text.chars().count();
            range_offsets.push((r_cursor, len));
            r_cursor += len;
            range_spans.push(if candidate == active_range {
                Span::styled(text, theme.active())
            } else {
                Span::raw(text)
            });
        }
    }

    let (memory_mb, cpu) = metrics;
    let usage_spans = vec![Span::styled(
        format!("  {memory_mb:.1} MB · CPU {cpu:.1}% "),
        dim,
    )];

    let span_w =
        |spans: &[Span]| -> usize { spans.iter().map(|span| span.content.chars().count()).sum() };

    let left_w = span_w(&left);
    let mut room = width.saturating_sub(left_w + 1);
    let mut show_ranges = false;
    let mut right = Vec::new();

    let range_w = span_w(&range_spans);
    if !range_spans.is_empty() && range_w <= room {
        show_ranges = true;
        room -= range_w;
        right.extend(range_spans);
        let usage_w = span_w(&usage_spans);
        if usage_w <= room {
            right.extend(usage_spans);
        }
    } else if range_spans.is_empty() {
        let usage_w = span_w(&usage_spans);
        if usage_w <= room {
            right.extend(usage_spans);
        }
    }

    let right_w = span_w(&right);
    let gap = width.saturating_sub(left_w + right_w);
    let right_start = left_w + gap;

    if show_ranges {
        for (idx, (offset, len)) in range_offsets.into_iter().enumerate() {
            let x = right_start + offset;
            if x + len <= width {
                hits.ranges[idx].1 = Some(Rect::new(area.x + x as u16, area.y, len as u16, 1));
            }
        }
    }

    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    f.render_widget(Paragraph::new(Line::from(left)), area);
    hits
}

include!(concat!(env!("OUT_DIR"), "/layouts_registry.rs"));

/// Resolves a configured layout name (case-insensitive) to a compiled-in
/// [`TuiLayout`], falling back to [`vct_core::config::DEFAULT_LAYOUT`] and then
/// the first discovered layout when an older config names a removed layout.
pub(crate) fn resolve(id: &str) -> &'static dyn TuiLayout {
    let trimmed = id.trim();
    LAYOUTS
        .iter()
        .copied()
        .find(|plugin| plugin.meta().id.eq_ignore_ascii_case(trimmed))
        .or_else(|| {
            LAYOUTS.iter().copied().find(|plugin| {
                plugin
                    .meta()
                    .id
                    .eq_ignore_ascii_case(vct_core::config::DEFAULT_LAYOUT)
            })
        })
        .unwrap_or(LAYOUTS[0])
}

/// Instantiates a fresh [`LayoutView`] for the layout named `id`, returning
/// both its canonical `meta().id` and the boxed view.
pub(crate) fn create(id: &str) -> (&'static str, Box<dyn LayoutView>) {
    let plugin = resolve(id);
    (plugin.meta().id, plugin.create())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn registry_is_non_empty_and_ids_match_metadata() {
        assert!(!LAYOUTS.is_empty());
        assert_eq!(LAYOUTS.len(), LAYOUT_IDS.len());
        for (plugin, &id) in LAYOUTS.iter().zip(LAYOUT_IDS.iter()) {
            let meta = plugin.meta();
            assert_eq!(meta.id, id);
            assert!(!meta.title.is_empty(), "empty title for {id}");
            assert!(!meta.summary.is_empty(), "empty summary for {id}");
        }
    }

    #[test]
    fn unknown_layout_name_falls_back_to_default_plugin() {
        let expected = resolve(vct_core::config::DEFAULT_LAYOUT).meta().id;
        assert_eq!(resolve("removed_or_unknown_layout").meta().id, expected);
        assert_eq!(resolve("").meta().id, expected);
        for &id in LAYOUT_IDS {
            assert_eq!(resolve(&id.to_uppercase()).meta().id, id);
        }
    }

    #[test]
    fn top_bar_records_tab_and_range_hit_boxes_and_drops_cleanly_when_narrow() {
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let mut hits = TopBarHits::default();
        terminal
            .draw(|f| {
                hits = render_top_bar(f, f.area(), Tab::Usage, Some(TimeRange::All), (97.5, 0.0));
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let shown: String = (0..120).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(shown.chars().count(), 120);
        for expected in ["Usage", "Analysis", "Quota", "Settings", "month", "97.5 MB"] {
            assert!(shown.contains(expected), "{expected} missing: {shown}");
        }

        for tab in Tab::ALL {
            let needle = format!(" {} ", tab.label());
            let start = shown.find(&needle).unwrap() as u16;
            assert_eq!(hits.hit_tab(start + 1, 0), Some(tab));
            assert_eq!(hits.hit_tab(start + 1, 1), None);
        }
        for (range, label) in RANGES {
            let needle = format!(" {label} ");
            let start = shown.rfind(&needle).unwrap() as u16;
            assert_eq!(hits.hit_range(start + 1, 0), Some(range));
        }

        // Narrow terminal drops metrics first, then ranges.
        let mut narrow_term = Terminal::new(TestBackend::new(60, 1)).unwrap();
        let mut narrow_hits = TopBarHits::default();
        narrow_term
            .draw(|f| {
                narrow_hits = render_top_bar(
                    f,
                    f.area(),
                    Tab::Analysis,
                    Some(TimeRange::Daily),
                    (97.5, 0.0),
                );
            })
            .unwrap();
        assert_eq!(narrow_hits.hit_range(50, 0), None);
    }
}
