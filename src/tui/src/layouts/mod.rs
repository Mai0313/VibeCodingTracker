//! Pluggable interactive TUI layouts, auto-discovered at compile time.
//!
//! `src/tui/build.rs` scans this directory for `<id>/mod.rs` folders and `<id>.rs` files, generating `$OUT_DIR/layouts_registry.rs` with [`LAYOUTS`] and [`LAYOUT_IDS`]. Each layout module exports a `static LAYOUT` visible to this module that implements [`TuiLayout`].

use crate::display::analysis::{AnalysisRow, AnalysisUiState};
use crate::display::app::Tab;
use crate::display::common::provider_tui_color;
use crate::display::common::table::title_line;
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, render_loading};
use crate::display::settings::{SettingsOutcome, SettingsUiState};
use crate::display::usage::{
    QuotaView, UsageRow, UsageTotals, UsageUiState, draw_quota_tab_with_border,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use vct_core::models::TimeRange;
use vct_core::utils::{format_compact, format_cost, format_cost_compact};

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

/// Factory registered by each layout module.
pub(crate) trait TuiLayout: Sync {
    /// Returns metadata for this layout.
    fn meta(&self) -> LayoutMeta;

    /// Creates a fresh per-session view instance holding this layout's UI state.
    fn create(&self) -> Box<dyn LayoutView>;
}

/// Borrowed application state passed into [`LayoutView`] callbacks.
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
    /// Renders the entire terminal frame (`f.area()`), including navigation chrome and the active tab's content.
    ///
    /// Implementations must keep the single-line controls footer on the bottom row (`y = f.area().bottom() - 1`), where the host's OSC 8 hyperlink pass wraps `Star on GitHub`.
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>);

    /// Handles mouse clicks and vertical scroll wheel events across the frame.
    fn on_mouse(&mut self, mouse: MouseInput, ctx: &mut LayoutContext<'_>) -> EventResult;

    /// Gives the layout first refusal on keys when not on [`Tab::Settings`].
    fn on_key(&mut self, key: KeyCode, ctx: &mut LayoutContext<'_>) -> EventResult {
        let _ = (key, ctx);
        EventResult::Ignored
    }

    /// Clears transient overlays when the user switches tabs.
    fn on_tab_changed(&mut self, next: Tab, ctx: &mut LayoutContext<'_>) {
        let _ = (next, ctx);
    }

    /// Returns custom `(key, description)` rows for the `?` help modal on `tab`, or `None` to use the default list.
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
    pub(crate) tabs: [(Tab, Option<Rect>); 4],
    pub(crate) ranges: [(TimeRange, Option<Rect>); 4],
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

/// Returns whether `(column, row)` lies inside `rect`.
#[inline]
pub(crate) fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

/// Handles mouse input on [`Tab::Settings`] for layouts with a horizontal top bar, giving an open editor modal precedence over tab-bar clicks.
pub(crate) fn handle_settings_mouse(
    mouse: MouseInput,
    ctx: &mut LayoutContext<'_>,
    top_bar: &TopBarHits,
) -> EventResult {
    if ctx.settings.has_open_editor() {
        let changed = ctx.settings.handle_mouse(mouse) == SettingsOutcome::Changed;
        return EventResult::Consumed(LayoutCommand {
            settings_changed: changed,
            ..LayoutCommand::default()
        });
    }
    if let MouseInput::Click { column, row } = mouse
        && let Some(next) = top_bar.hit_tab(column, row)
    {
        return EventResult::Consumed(LayoutCommand {
            switch_tab: Some(next),
            ..LayoutCommand::default()
        });
    }
    let changed = ctx.settings.handle_mouse(mouse) == SettingsOutcome::Changed;
    EventResult::Consumed(LayoutCommand {
        settings_changed: changed,
        ..LayoutCommand::default()
    })
}

/// Resolves a click on the top bar into a tab-switch or range-change command.
pub(crate) fn handle_top_bar_click(
    column: u16,
    row: u16,
    top_bar: &TopBarHits,
    tab: Tab,
) -> Option<LayoutCommand> {
    if let Some(next) = top_bar.hit_tab(column, row) {
        return Some(LayoutCommand {
            switch_tab: Some(next),
            ..LayoutCommand::default()
        });
    }
    if tab.scans()
        && let Some(range) = top_bar.hit_range(column, row)
    {
        return Some(LayoutCommand {
            set_range: Some(range),
            ..LayoutCommand::default()
        });
    }
    None
}

/// Renders the loading/retry state, `Quota` tab, or `Settings` tab with `border_type`.
pub(crate) fn render_standard_non_data(
    f: &mut Frame,
    body: Rect,
    ctx: &mut LayoutContext<'_>,
    border_type: BorderType,
) {
    match ctx.tab {
        Tab::Usage | Tab::Analysis => {
            let message = if ctx.retry_pending {
                "Load failed, press r to retry"
            } else {
                "Loading sessions..."
            };
            render_loading(f, body, ctx.spinner, message);
        }
        Tab::Quota => {
            if let Some((ref quota, now)) = ctx.quota {
                draw_quota_tab_with_border(f, body, quota, now, ctx.update_status, border_type);
            }
        }
        Tab::Settings => {
            ctx.settings
                .draw_with_border(f, body, ctx.update_status, border_type);
        }
    }
}

/// Renders the single-row navigation bar into `area` and returns hit-test rectangles for visible tab and range pills.
pub(crate) fn render_top_bar(
    f: &mut Frame,
    area: Rect,
    current: Tab,
    range: Option<TimeRange>,
    metrics: (f64, f32),
) -> TopBarHits {
    render_top_bar_styled(f, area, current, range, metrics, false)
}

/// Like [`render_top_bar`], styling the active tab with a bold accent underline instead of an inverted pill when `underline_tabs` is set.
pub(crate) fn render_top_bar_styled(
    f: &mut Frame,
    area: Rect,
    current: Tab,
    range: Option<TimeRange>,
    metrics: (f64, f32),
    underline_tabs: bool,
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
        let span = if tab == current {
            if underline_tabs {
                Span::styled(text, theme.accent().bold().underlined())
            } else {
                Span::styled(text, theme.active())
            }
        } else if underline_tabs {
            Span::styled(text, dim)
        } else {
            Span::raw(text)
        };
        left.push(span);
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

/// Returns the longest title (`full`, `medium`, or `short`) that fits beside `note` in a bordered table of width `table_w`, truncating `short` with `…` if needed.
pub(crate) fn fit_table_title(
    full: String,
    medium: String,
    short: String,
    table_w: u16,
    note: &str,
) -> String {
    // 2 borders + 2 padding around title + 2 padding around note + >=1 dash gap.
    let avail = usize::from(table_w).saturating_sub(note.chars().count() + 7);
    if full.chars().count() <= avail {
        full
    } else if medium.chars().count() <= avail {
        medium
    } else if short.chars().count() <= avail {
        short
    } else {
        truncate_ellipsis(&short, avail.max(6))
    }
}

/// Truncates `text` to at most `max_chars` characters, appending `…` when cut.
pub(crate) fn truncate_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Builds a two-span horizontal progress/share bar of `width` columns.
pub(crate) fn bar_spans(
    ratio: f64,
    width: usize,
    filled_ch: char,
    empty_ch: char,
    filled_style: Style,
) -> [Span<'static>; 2] {
    let clamped = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut filled = (clamped * width as f64).round() as usize;
    if clamped > 0.0 && width > 0 {
        filled = filled.max(1);
    }
    filled = filled.min(width);
    [
        Span::styled(filled_ch.to_string().repeat(filled), filled_style),
        Span::styled(
            empty_ch.to_string().repeat(width - filled),
            theme().border(),
        ),
    ]
}

/// Returns a width-adaptive toggle label for a Usage side pane switching between `Inspector` and `Providers`.
pub(crate) fn side_toggle_note(inspector_active: bool, side_width: u16) -> Line<'static> {
    let theme = theme();
    if side_width >= 48 {
        if inspector_active {
            Line::from(vec![
                Span::styled(" [Model] ", theme.accent().bold()),
                Span::styled("· Providers (p) ", theme.dim()),
            ])
        } else {
            Line::from(vec![
                Span::styled(" Model · ", theme.dim()),
                Span::styled("[Providers] (p) ", theme.accent().bold()),
            ])
        }
    } else if side_width >= 38 {
        if inspector_active {
            Line::from(Span::styled(" p: providers ", theme.dim()))
        } else {
            Line::from(Span::styled(" p: model ", theme.accent().bold()))
        }
    } else if inspector_active {
        Line::from(Span::styled(" (p) ", theme.dim()))
    } else {
        Line::from(Span::styled(" (p) ", theme.accent().bold()))
    }
}

/// Renders an empty rounded `Inspector` placeholder when no model is selected.
pub(crate) fn render_empty_inspector(
    f: &mut Frame,
    area: Rect,
    right_note: Option<Line<'static>>,
    placeholder: &'static str,
) {
    let theme = theme();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(title_line("Inspector"));
    if let Some(note) = right_note {
        block = block.title(note.right_aligned());
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {placeholder}"),
            Style::default().fg(theme.dim),
        )))
        .block(block),
        area,
    );
}

/// One row inside a horizontal inspector strip column.
pub(crate) enum StripEntry {
    /// Key-value statistic row with an optional emphasis style on the value.
    Stat {
        label: &'static str,
        value: String,
        value_style: Option<Style>,
    },
    /// Provider attribution row prefixed with a colored `●` dot.
    Provider {
        color: Color,
        label: &'static str,
        value: String,
    },
}

/// One titled column inside a 3-column horizontal inspector strip.
pub(crate) struct StripColumn {
    pub(crate) heading: &'static str,
    pub(crate) entries: Vec<StripEntry>,
}

/// Builds the 3 compact horizontal inspector columns for a selected Usage model.
pub(crate) fn build_usage_strip_columns(row: &UsageRow, totals: &UsageTotals) -> [StripColumn; 3] {
    let theme = theme();
    let providers: Vec<StripEntry> = row
        .providers
        .iter()
        .map(|entry| StripEntry::Provider {
            color: provider_tui_color(entry.provider),
            label: entry.provider.display_name(),
            value: format!(
                "{} ({})",
                format_cost_compact(entry.cost),
                format_compact(entry.total_tokens)
            ),
        })
        .collect();

    let cost_share = if totals.cost > 0.0 {
        format!("{:.1}%", row.cost / totals.cost * 100.0)
    } else {
        "-".to_string()
    };
    let eff_rate = if row.total > 0 && row.cost > 0.0 {
        let per_m = row.cost / (row.total as f64 / 1_000_000.0);
        if per_m < 0.01 {
            format!("${per_m:.4}/1M")
        } else {
            format!("{}/1M", format_cost(per_m))
        }
    } else {
        "-".to_string()
    };
    let prompt_total = row.input_tokens + row.cache_read + row.cache_creation;
    let cache_hit = if row.cache_read > 0 && prompt_total > 0 {
        format!(
            "{:.0}%",
            row.cache_read as f64 / prompt_total as f64 * 100.0
        )
    } else {
        "-".to_string()
    };

    let mut eff_entries = vec![
        StripEntry::Stat {
            label: "Cost share",
            value: cost_share,
            value_style: Some(theme.accent().bold()),
        },
        StripEntry::Stat {
            label: "Effective",
            value: eff_rate,
            value_style: None,
        },
        StripEntry::Stat {
            label: "Cache hit",
            value: cache_hit,
            value_style: Some(Style::default().fg(theme.good)),
        },
    ];
    if let Some(matched) = &row.matched_model {
        eff_entries.push(StripEntry::Stat {
            label: "Priced as",
            value: matched.clone(),
            value_style: None,
        });
    }

    let mut token_entries = vec![
        StripEntry::Stat {
            label: "In / Out",
            value: format!(
                "{} / {}",
                format_compact(row.input_tokens),
                format_compact(row.output_with_reasoning())
            ),
            value_style: None,
        },
        StripEntry::Stat {
            label: "Cache R/W",
            value: format!(
                "{} / {}",
                format_compact(row.cache_read),
                format_compact(row.cache_creation)
            ),
            value_style: None,
        },
    ];
    if row.reasoning_tokens > 0 {
        token_entries.push(StripEntry::Stat {
            label: "Reasoning",
            value: format_compact(row.reasoning_tokens),
            value_style: None,
        });
    }
    if row.cache_creation_1h > 0 {
        token_entries.push(StripEntry::Stat {
            label: "1h TTL",
            value: format_compact(row.cache_creation_1h),
            value_style: None,
        });
    }
    if row.tool_tokens > 0 {
        token_entries.push(StripEntry::Stat {
            label: "Tool tok",
            value: format_compact(row.tool_tokens),
            value_style: None,
        });
    }
    if row.above_tier_tokens > 0 {
        token_entries.push(StripEntry::Stat {
            label: "Long ctx",
            value: format_compact(row.above_tier_tokens),
            value_style: None,
        });
    }
    if token_entries.len() < 3 {
        token_entries.push(StripEntry::Stat {
            label: "Total",
            value: format_compact(row.total),
            value_style: None,
        });
    }

    [
        StripColumn {
            heading: "Providers",
            entries: providers,
        },
        StripColumn {
            heading: "Efficiency & Share",
            entries: eff_entries,
        },
        StripColumn {
            heading: "Token Mix",
            entries: token_entries,
        },
    ]
}

/// Builds the 3 compact horizontal inspector columns for a selected Analysis model.
pub(crate) fn build_analysis_strip_columns(
    row: &AnalysisRow,
    totals: &AnalysisRow,
) -> [StripColumn; 3] {
    let theme = theme();
    let providers: Vec<StripEntry> = row
        .providers
        .iter()
        .map(|entry| StripEntry::Provider {
            color: provider_tui_color(entry.provider),
            label: entry.provider.display_name(),
            value: format!(
                "{}L · {}c",
                format_compact(entry.total_lines as i64),
                format_compact(entry.total_calls as i64)
            ),
        })
        .collect();

    let avg = |lines: usize, ops: usize| -> String {
        if ops > 0 && lines > 0 {
            format!("~{}/op", (lines as f64 / ops as f64).round() as usize)
        } else {
            "-".to_string()
        }
    };
    let line_share = if totals.total_lines() > 0 {
        format!(
            "{:.1}%",
            row.total_lines() as f64 / totals.total_lines() as f64 * 100.0
        )
    } else {
        "-".to_string()
    };

    [
        StripColumn {
            heading: "Providers",
            entries: providers,
        },
        StripColumn {
            heading: "Operations",
            entries: vec![
                StripEntry::Stat {
                    label: "Edit",
                    value: format!(
                        "{}L ({} ops)",
                        format_compact(row.edit_lines as i64),
                        format_compact(row.edit_count as i64)
                    ),
                    value_style: None,
                },
                StripEntry::Stat {
                    label: "Read",
                    value: format!(
                        "{}L ({} ops)",
                        format_compact(row.read_lines as i64),
                        format_compact(row.read_count as i64)
                    ),
                    value_style: None,
                },
                StripEntry::Stat {
                    label: "Write",
                    value: format!(
                        "{}L ({} ops)",
                        format_compact(row.write_lines as i64),
                        format_compact(row.write_count as i64)
                    ),
                    value_style: None,
                },
            ],
        },
        StripColumn {
            heading: "Commands & Avg",
            entries: vec![
                StripEntry::Stat {
                    label: "Line share",
                    value: line_share,
                    value_style: Some(theme.accent().bold()),
                },
                StripEntry::Stat {
                    label: "Bash / Todo",
                    value: format!(
                        "{} / {}",
                        format_compact(row.bash_count as i64),
                        format_compact(row.todo_write_count as i64)
                    ),
                    value_style: None,
                },
                StripEntry::Stat {
                    label: "Avg E / R",
                    value: format!(
                        "{} · {}",
                        avg(row.edit_lines, row.edit_count),
                        avg(row.read_lines, row.read_count)
                    ),
                    value_style: None,
                },
            ],
        },
    ]
}

/// Renders a 3-column horizontal inspector pane with overflow `+N more` notices.
pub(crate) fn render_strip_pane(
    f: &mut Frame,
    area: Rect,
    title: Line<'static>,
    right_note: Option<Line<'static>>,
    accent_border: bool,
    cols: &[StripColumn; 3],
) {
    let theme = theme();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if accent_border {
            theme.accent()
        } else {
            theme.border()
        })
        .title(title);
    if let Some(note) = right_note {
        block = block.title(note.right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 12 || inner.height == 0 {
        return;
    }

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let spacing = if padded.width >= 66 { 2 } else { 1 };
    let chunks = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .spacing(spacing)
    .split(padded);

    for (col, rect) in cols.iter().zip(chunks.iter()) {
        render_strip_column(f, *rect, col);
    }
}

fn render_strip_column(f: &mut Frame, area: Rect, col: &StripColumn) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = theme();
    let max_w = usize::from(area.width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(area.height));
    lines.push(Line::from(Span::styled(
        truncate_ellipsis(col.heading, max_w),
        theme.accent().bold(),
    )));

    let avail = usize::from(area.height.saturating_sub(1));
    if avail == 0 {
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    if col.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            truncate_ellipsis("none", max_w),
            theme.dim(),
        )));
        f.render_widget(Paragraph::new(lines), area);
        return;
    }

    let hidden = if col.entries.len() > avail {
        col.entries.len() - avail.saturating_sub(1)
    } else {
        0
    };
    let shown = col.entries.len() - hidden;

    for entry in col.entries.iter().take(shown) {
        lines.push(format_strip_entry(entry, max_w));
    }
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            truncate_ellipsis(&format!("+{hidden} more"), max_w),
            theme.dim(),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn format_strip_entry(entry: &StripEntry, max_w: usize) -> Line<'static> {
    let theme = theme();
    match entry {
        StripEntry::Stat {
            label,
            value,
            value_style,
        } => {
            let v_style = value_style.unwrap_or_default();
            let val_w = value.chars().count();
            let lbl_w = label.chars().count();
            if lbl_w + 1 + val_w <= max_w {
                let gap = max_w - lbl_w - val_w;
                Line::from(vec![
                    Span::styled(*label, theme.dim()),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(value.clone(), v_style),
                ])
            } else {
                let max_val = max_w.saturating_sub(lbl_w + 1);
                if max_val >= 4 {
                    let fitted = truncate_ellipsis(value, max_val);
                    let gap = max_w.saturating_sub(lbl_w + fitted.chars().count());
                    Line::from(vec![
                        Span::styled(*label, theme.dim()),
                        Span::raw(" ".repeat(gap)),
                        Span::styled(fitted, v_style),
                    ])
                } else {
                    Line::from(Span::styled(
                        truncate_ellipsis(&format!("{label} {value}"), max_w),
                        theme.dim(),
                    ))
                }
            }
        }
        StripEntry::Provider {
            color,
            label,
            value,
        } => {
            let prefix_w = 2 + label.chars().count();
            let val_w = value.chars().count();
            if prefix_w + 1 + val_w <= max_w {
                let gap = max_w - prefix_w - val_w;
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(*color)),
                    Span::raw(*label),
                    Span::raw(" ".repeat(gap)),
                    Span::raw(value.clone()),
                ])
            } else {
                let max_lbl = max_w.saturating_sub(2 + 1 + val_w);
                if max_lbl >= 3 {
                    let fitted_lbl = truncate_ellipsis(label, max_lbl);
                    let gap = max_w.saturating_sub(2 + fitted_lbl.chars().count() + val_w);
                    Line::from(vec![
                        Span::styled("● ", Style::default().fg(*color)),
                        Span::raw(fitted_lbl),
                        Span::raw(" ".repeat(gap)),
                        Span::raw(value.clone()),
                    ])
                } else {
                    let fitted =
                        truncate_ellipsis(&format!("{label} {value}"), max_w.saturating_sub(2));
                    Line::from(vec![
                        Span::styled("● ", Style::default().fg(*color)),
                        Span::raw(fitted),
                    ])
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/layouts_registry.rs"));

/// Resolves `id` (case-insensitive, trimmed) to a compiled-in [`TuiLayout`], falling back to [`vct_core::config::DEFAULT_LAYOUT`] and then the first layout in [`LAYOUTS`].
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

/// Instantiates a fresh [`LayoutView`] for `id`, returning its canonical [`LayoutMeta::id`] and boxed view.
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
        assert_eq!(resolve("legacy").meta().id, expected);
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

        // At 60 columns, range pills do not fit and are omitted.
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
