//! Frame layout, plus the comfy-table and ratatui cell / table builders shared
//! across both views.
//!
//! The builders are pure widget constructors that encode the common styling so
//! the static-table and TUI renderers stay visually consistent; the summary
//! bar's process-metrics helpers are the one exception, sampling this process's
//! own CPU and memory.
//! A recurring convention: the leading label column(s) are left-aligned and
//! every trailing (numeric) column is right-aligned. The comfy-table helpers
//! left-align the first two (index 0 and 1); the ratatui `styled_row` helper
//! takes the number of left-aligned columns as its `left_cols` argument.

use crate::display::common::tui::ScrollState;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color as RatatuiColor, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell as RatatuiCell, Paragraph, Row as RatatuiRow, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table as RatatuiTable,
    },
};
use sysinfo::System;

/// Normalizes a process's raw (per-core-summed) CPU usage into a 0-100% share
/// of the machine by dividing by the CPU count.
///
/// `cores` must be the **same basis** sysinfo scales `cpu_usage()` against, i.e.
/// `sys.cpus().len()` (the host's logical CPUs read from `/proc/stat`). Using
/// that basis keeps the reading a true machine share even under CPU affinity or
/// a cgroup CPU quota, where `available_parallelism()` would report a smaller
/// count and inflate the percentage.
fn normalized_cpu(raw: f32, cores: f32) -> f32 {
    (raw / cores).clamp(0.0, 100.0)
}

/// Refreshes only this process's CPU + memory in `sys` (skipping disk / exe /
/// tasks), pruning any dead entry. This is the single source of the "our own
/// process, cheap metrics only" contract shared by every summary-bar refresh —
/// deliberately narrower than sysinfo's default `refresh_processes`. On Linux it
/// reads this process's stat plus `/proc/stat` for the CPU-time delta (a couple
/// of small reads), cheap enough to run on a fast metrics tick. `.with_cpu()`
/// also populates `sys.cpus()`, which `create_summary` uses to normalize CPU%.
pub fn refresh_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory(),
    );
}

/// One-time init for the summary-bar metrics `System`, called once after
/// `System::new()` before the refresh loop.
///
/// It populates the CPU list (`refresh_cpu_all`) so `create_summary` can read
/// `sys.cpus().len()` as the CPU% divisor, then primes this process's first
/// sample. The CPU-list step is **required**: a process-only refresh does not
/// initialize `sys.cpus()` on macOS, which would leave the divisor at 1 and
/// over-report CPU%. The list is stable, so it is never refreshed again — the
/// per-tick path stays [`refresh_process_metrics`].
pub fn init_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_cpu_all();
    refresh_process_metrics(sys, pid);
}

/// Builds the bordered, centered title paragraph for the top of a TUI view.
pub fn create_title(title_text: &str, color: RatatuiColor) -> Paragraph<'_> {
    Paragraph::new(vec![Line::from(vec![Span::styled(
        title_text,
        Style::default().fg(color).bold(),
    )])])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color)),
    )
    .centered()
}

/// Separator drawn between summary-bar segments.
const SUMMARY_SEP: &str = "  |  ";

/// Display width of an ASCII summary fragment (labels / formatted numbers are
/// ASCII, so char count equals column count).
fn seg_width(s: &str) -> usize {
    s.chars().count()
}

/// Decides which trailing diagnostic segments fit after the primary items.
///
/// `used` is the primary items' width, `available` the content width. Returns
/// `(show_memory, show_cpu)`: Memory is shown only if it fits, and CPU only if
/// Memory is also shown and both fit — so on a narrowing bar CPU drops first,
/// then Memory, and neither is ever clipped mid-word.
fn fit_diagnostics(
    used: usize,
    available: usize,
    mem_width: usize,
    cpu_width: usize,
) -> (bool, bool) {
    if used + mem_width > available {
        return (false, false);
    }
    (true, used + mem_width + cpu_width <= available)
}

/// Builds the one-line TUI summary bar from caller-supplied items plus live
/// memory and CPU readouts.
///
/// Each `(icon, value, color)` tuple in `summary_items` becomes a colored,
/// pipe-separated segment; these primary items are always rendered. A
/// `Memory: <n> MB` and a `CPU: <n>%` segment for the current process `pid` are
/// then appended **only while they fit** within `width` (the summary rect's
/// width): on a terminal too narrow for the full line the diagnostic segments
/// degrade gracefully, dropped whole (CPU first, then Memory) instead of
/// clipped mid-word. They read `0.0` if `sys` has no entry for `pid`. CPU is
/// normalized to a 0-100% share of the machine. `sys` is expected to have been
/// refreshed by the caller before this call.
///
/// The bar is deliberately unbordered: it carries a single line of text, and a
/// border around it would cost two more rows of the scrollable table above.
pub fn create_summary<'a>(
    summary_items: Vec<(&'a str, &'a str, RatatuiColor)>, // (icon, value, color) tuples
    sys: &'a System,
    pid: sysinfo::Pid,
    width: u16,
) -> Paragraph<'a> {
    let available = width as usize;

    let mut spans = Vec::new();
    let mut used = 0usize;

    // Primary items are always shown (they clip only on an extremely narrow bar).
    for (i, (icon, value, color)) in summary_items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(SUMMARY_SEP));
            used += seg_width(SUMMARY_SEP);
        }
        let label = format!("{} ", icon);
        used += seg_width(&label) + seg_width(value);
        spans.push(Span::styled(label, Style::default().fg(*color).bold()));
        spans.push(Span::styled(*value, Style::default().fg(*color).bold()));
    }

    // Diagnostic segments for this process. Both are measured up front; how
    // many actually render is decided by `fit_diagnostics` (CPU drops first).
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);
    let mem_value = format!("{:.1} MB", memory_mb);
    let mem_width = seg_width(SUMMARY_SEP) + seg_width("Memory: ") + seg_width(&mem_value);

    // CPU normalized to a 0-100% machine share. Divide by `sys.cpus().len()`
    // (sysinfo's own basis, populated by `init_process_metrics`); `.max(1)`
    // guards the empty-CPU-list case.
    let cores = sys.cpus().len().max(1) as f32;
    let cpu_percent = sys
        .process(pid)
        .map_or(0.0, |p| normalized_cpu(p.cpu_usage(), cores));
    let cpu_value = format!("{:.1}%", cpu_percent);
    let cpu_width = seg_width(SUMMARY_SEP) + seg_width("CPU: ") + seg_width(&cpu_value);

    let (show_mem, show_cpu) = fit_diagnostics(used, available, mem_width, cpu_width);
    if show_mem {
        spans.push(Span::raw(SUMMARY_SEP));
        spans.push(Span::styled(
            "Memory: ",
            Style::default().fg(RatatuiColor::LightRed).bold(),
        ));
        spans.push(Span::styled(
            mem_value,
            Style::default().fg(RatatuiColor::LightYellow).bold(),
        ));
    }
    if show_cpu {
        spans.push(Span::raw(SUMMARY_SEP));
        spans.push(Span::styled(
            "CPU: ",
            Style::default().fg(RatatuiColor::LightRed).bold(),
        ));
        spans.push(Span::styled(
            cpu_value,
            Style::default().fg(RatatuiColor::LightYellow).bold(),
        ));
    }

    Paragraph::new(vec![Line::from(spans)]).centered()
}

/// Short call-to-action shown in the footer (also the OSC 8 hyperlink text).
pub const REPO_LABEL: &str = "Star on GitHub";
/// URL the footer's repository label points at when clicked.
pub const REPO_URL: &str = "https://github.com/Mai0313/VibeCodingTracker";

/// Builds the single-line key-hint footer (navigation + the GitHub repo link).
///
/// Everything is on one line to save vertical space. `extra` inserts
/// view-specific `(key, label)` hints just before `r refresh`; a view with none
/// passes an empty slice. The repo label is drawn as plain (underlined) text
/// here; a terminal hyperlink is layered on afterward by
/// [`overlay_repo_hyperlink`](super::tui::overlay_repo_hyperlink).
pub fn create_controls(extra: &[(&str, &str)], width: u16) -> Paragraph<'static> {
    create_controls_with_status(extra, None, width)
}

/// Builds the controls footer with an optional transient refresh status.
///
/// Hints are dropped whole rather than clipped mid-word when they do not fit
/// `width`, in this order: the repo label, then the view-specific `extra` hints
/// from the right, then the status. Scrolling, refresh and quit always survive —
/// a footer that cannot say how to quit is worse than one that says less.
pub fn create_controls_with_status(
    extra: &[(&str, &str)],
    status: Option<&str>,
    width: u16,
) -> Paragraph<'static> {
    let key = Style::default().fg(RatatuiColor::Cyan).bold();
    let dim = Style::default().fg(RatatuiColor::DarkGray);
    let available = width as usize;

    // Width of every part; which of them survive is decided below.
    let core = seg_width("↑/↓ scroll  r refresh  q quit");
    let repo = seg_width("  |  ") + seg_width(REPO_LABEL);
    let status_w = status.map_or(0, |s| seg_width("  |  ") + seg_width(s));
    let extra_w: Vec<usize> = extra
        .iter()
        .map(|(k, label)| seg_width(k) + seg_width(label))
        .collect();

    // Drop from the cheapest end until what is left fits.
    let mut shown_extra = extra.len();
    let mut show_status = status.is_some();
    let mut show_repo = true;
    let used = |shown: usize, status: bool, repo_shown: bool| {
        core + extra_w[..shown].iter().sum::<usize>()
            + if status { status_w } else { 0 }
            + if repo_shown { repo } else { 0 }
    };
    if used(shown_extra, show_status, show_repo) > available {
        show_repo = false;
    }
    while shown_extra > 0 && used(shown_extra, show_status, show_repo) > available {
        shown_extra -= 1;
    }
    if used(shown_extra, show_status, show_repo) > available {
        show_status = false;
    }

    let mut spans = vec![Span::styled("↑/↓", key), Span::styled(" scroll  ", dim)];
    for (k, label) in &extra[..shown_extra] {
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(label.to_string(), dim));
    }
    spans.push(Span::styled("r", key));
    spans.push(Span::styled(" refresh  ", dim));
    spans.push(Span::styled(
        "q",
        Style::default().fg(RatatuiColor::Red).bold(),
    ));
    spans.push(Span::styled(" quit", dim));
    if show_status && let Some(status) = status {
        spans.push(Span::styled("  |  ", dim));
        spans.push(Span::styled(
            status.to_string(),
            Style::default().fg(RatatuiColor::Yellow).bold(),
        ));
    }
    if show_repo {
        spans.push(Span::styled("  |  ", dim));
        spans.push(Span::styled(
            REPO_LABEL,
            Style::default().fg(RatatuiColor::Cyan).underlined(),
        ));
    }
    Paragraph::new(vec![Line::from(spans)]).centered()
}

/// Rows the scrollable content pane is never squeezed below. The block border,
/// header row and header margin eat 4, so this keeps ~2 body rows alive.
pub const CONTENT_MIN_H: u16 = 6;
/// Rows the frame always spends below the content: summary + controls.
pub const FOOTER_H: u16 = 2;

/// Chunk rects for an interactive frame.
pub struct FrameChunks {
    /// Scrollable main table area, always the full width of the frame.
    pub content: Rect,
    /// Full-width band under the content, present only when one was asked for.
    pub band: Option<Rect>,
    /// Single-line summary area.
    pub summary: Rect,
    /// Single-line controls footer area.
    pub controls: Rect,
}

/// Splits `area` into the standard interactive-view regions.
///
/// The content pane absorbs all leftover height, then an optional
/// `band_height`-row band, then one row each of summary and controls. Every
/// region spans the full width: a view that wants to show more than its table
/// spends rows on a band, never columns on a side pane, so the table's own
/// columns are never the thing that pays for it.
pub fn frame_layout(area: Rect, band_height: u16) -> FrameChunks {
    let mut constraints: Vec<Constraint> = Vec::with_capacity(4);
    constraints.push(Constraint::Min(CONTENT_MIN_H));
    if band_height > 0 {
        constraints.push(Constraint::Length(band_height));
    }
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Length(1));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut next = 0;
    let mut take = || {
        let rect = rows[next];
        next += 1;
        rect
    };
    let content = take();
    let band = (band_height > 0).then(&mut take);
    let summary = take();
    let controls = take();

    FrameChunks {
        content,
        band,
        summary,
        controls,
    }
}

/// One numeric column of a scrollable table, plus how readily it may go.
pub struct ColumnSpec {
    /// Header text.
    pub header: &'static str,
    /// Column width in cells.
    pub width: u16,
    /// Drop order when the pane is too narrow: the highest rank goes first.
    /// The columns a reader would miss most carry the lowest ranks.
    pub drop_rank: u8,
}

/// Chooses which numeric columns survive in `inner` content columns.
///
/// Returns the kept columns in their original order together with the number
/// dropped, which the caller shows in the table's title. `name_min_w` cells are
/// reserved for the model-name column before any numeric column is fitted, so a
/// pane too narrow for even one of them keeps none rather than squeezing the
/// name.
pub fn fit_columns(specs: &[ColumnSpec], inner: u16, name_min_w: u16) -> (Vec<usize>, usize) {
    let mut kept: Vec<usize> = (0..specs.len()).collect();
    // Each column is preceded by one space of column spacing.
    let span = |kept: &[usize]| -> u16 {
        kept.iter()
            .map(|&i| specs[i].width + 1)
            .sum::<u16>()
            .saturating_add(name_min_w)
    };
    while !kept.is_empty() && span(&kept) > inner {
        let worst = kept
            .iter()
            .enumerate()
            .max_by_key(|&(_, &i)| specs[i].drop_rank)
            .map(|(pos, _)| pos);
        match worst {
            Some(pos) => {
                kept.remove(pos);
            }
            None => break,
        }
    }
    let dropped = specs.len() - kept.len();
    (kept, dropped)
}

/// Border, title and drop-notice for a scrollable table.
pub struct TableChrome<'a> {
    /// Left-aligned title; carries the totals for the rows underneath.
    pub title: &'a str,
    /// Right-aligned notice naming what the table had to leave out.
    pub note: Option<String>,
    /// Border color.
    pub border_color: RatatuiColor,
}

/// Builds a table row with the first `left_cols` cells left-aligned and the
/// rest right-aligned, painted with `style`.
///
/// Right-aligning the numeric columns keeps variable-width compact values
/// (`1.23M`, `999K`) flush instead of ragged.
pub fn styled_row(cells: Vec<String>, style: Style, left_cols: usize) -> RatatuiRow<'static> {
    let cells: Vec<RatatuiCell> = cells
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let align = if i < left_cols {
                Alignment::Left
            } else {
                Alignment::Right
            };
            RatatuiCell::from(Text::from(s).alignment(align))
        })
        .collect();
    RatatuiRow::new(cells).style(style)
}

/// Renders a scrollable, selectable table plus a side scrollbar into `area`.
///
/// `rows` are the already-styled body rows (highlight / TOTAL styling applied by
/// the caller); `row_count` is the total displayed row count used to size the
/// scrollbar. The block border + header + header margin consume 4 rows, so the
/// visible body height is `area.height - 4`.
#[allow(clippy::too_many_arguments)]
pub fn render_scrollable_table(
    f: &mut Frame,
    area: Rect,
    chrome: TableChrome<'_>,
    header: Vec<&str>,
    rows: Vec<RatatuiRow>,
    widths: &[Constraint],
    row_count: usize,
    scroll: &mut ScrollState,
) {
    let viewport = area.height.saturating_sub(4);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(format!(" {} ", chrome.title)))
        .border_style(Style::default().fg(chrome.border_color));
    if let Some(note) = chrome.note {
        block = block.title(
            Line::from(Span::styled(
                format!(" {note} "),
                Style::default().fg(RatatuiColor::DarkGray),
            ))
            .right_aligned(),
        );
    }

    // Selection is shown purely by the row color (no leading symbol / gutter).
    let table = create_ratatui_table(rows, header, widths, chrome.border_color)
        .block(block)
        .row_highlight_style(
            Style::default()
                .fg(RatatuiColor::Black)
                .bg(RatatuiColor::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, area, &mut scroll.table);

    // Scrollbar driven by the selected index, inset by one row so it sits
    // between the block's top/bottom borders instead of over the corners.
    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(viewport as usize)
        .position(selected);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scroll.scrollbar,
    );
}

/// Renders a centered "terminal too small" notice and nothing else.
///
/// Used as an early-out when the window is below the view's hard minimum, so the
/// normal layout never tries to draw into an area that would overlap.
pub fn render_too_small(f: &mut Frame, min_w: u16, min_h: u16) {
    let area = f.area();
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default().fg(RatatuiColor::Red).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "resize to at least {min_w} x {min_h}  (now {} x {})",
                area.width, area.height
            ),
            Style::default().fg(RatatuiColor::Gray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(RatatuiColor::Red)),
    )
    .alignment(Alignment::Center);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(area);
    f.render_widget(para, v[1]);
}

/// Builds a ratatui [`Table`](RatatuiTable) with the standard header and border styling.
///
/// `widths` sets the per-column constraints; the header row is rendered black-on-green
/// and bold with a one-line bottom margin.
pub fn create_ratatui_table<'a>(
    rows: Vec<RatatuiRow<'a>>,
    header: Vec<&'a str>,
    widths: &'a [Constraint],
    border_color: RatatuiColor,
) -> RatatuiTable<'a> {
    RatatuiTable::new(rows, widths)
        .header(
            RatatuiRow::new(header)
                .style(
                    Style::default()
                        .fg(RatatuiColor::Black)
                        .bg(RatatuiColor::Green)
                        .bold(),
                )
                .bottom_margin(1),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
}

/// Builds an empty comfy [`Table`] with a colored, UTF-8-bordered header row.
///
/// Header cells use the shared alignment convention: indices 0 and 1 are
/// left-aligned, the rest right-aligned. The returned table has no body rows.
pub fn create_comfy_table(headers: Vec<&str>, header_color: Color) -> Table {
    let mut table = Table::new();
    table.load_style(UTF8_FULL).set_header(
        headers
            .iter()
            .enumerate()
            .map(|(i, &header)| {
                let alignment = if i <= 1 {
                    CellAlignment::Left
                } else {
                    CellAlignment::Right
                };
                Cell::new(header).fg(header_color).set_alignment(alignment)
            })
            .collect::<Vec<_>>(),
    );
    table
}

/// Appends a single colored totals row to `table`.
///
/// Cells follow the shared alignment convention: indices 0 and 1 are
/// left-aligned, the rest right-aligned. Every cell is painted `color`.
pub fn add_totals_row(table: &mut Table, cells: Vec<String>, color: Color) {
    let colored_cells: Vec<Cell> = cells
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let alignment = if i <= 1 {
                CellAlignment::Left
            } else {
                CellAlignment::Right
            };
            Cell::new(text).fg(color).set_alignment(alignment)
        })
        .collect();

    table.add_row(colored_cells);
}

/// Builds a left-aligned, colored comfy [`Cell`] for a provider name, bolded when `emphasize`.
pub fn create_provider_cell(name: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(name).fg(color).set_alignment(CellAlignment::Left);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Builds a right-aligned, colored comfy [`Cell`] for a metric value, bolded when `emphasize`.
pub fn create_metric_cell(value: String, color: Color, emphasize: bool) -> Cell {
    let mut cell = Cell::new(value)
        .fg(color)
        .set_alignment(CellAlignment::Right);
    if emphasize {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

/// Builds a ratatui [`Row`](RatatuiRow) styled in `color`, bolded when `emphasize`.
pub fn create_provider_row<'a>(
    cells: Vec<String>,
    color: RatatuiColor,
    emphasize: bool,
) -> RatatuiRow<'a> {
    let style = if emphasize {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };

    RatatuiRow::new(cells).style(style)
}

#[cfg(test)]
mod tests {
    use super::{
        CONTENT_MIN_H, ColumnSpec, FOOTER_H, Rect, create_controls_with_status, fit_columns,
        fit_diagnostics, frame_layout, normalized_cpu,
    };

    #[test]
    fn frame_gives_every_leftover_row_to_the_content_pane() {
        let area = Rect::new(0, 0, 120, 40);
        let c = frame_layout(area, 0);
        assert_eq!(c.content.height, 40 - FOOTER_H);
        assert!(c.band.is_none());
        assert_eq!(c.summary.height, 1);
        assert_eq!(c.controls.height, 1);
        assert_eq!(c.controls.y, 39);

        let c = frame_layout(area, 6);
        assert_eq!(c.content.height, 40 - FOOTER_H - 6);
        assert_eq!(c.band.expect("band requested").height, 6);
    }

    #[test]
    fn every_region_spans_the_full_frame_width() {
        // Nothing is ever taken out of the content pane horizontally, so the
        // table's columns only ever compete with each other.
        for width in [74u16, 90, 120, 200] {
            let c = frame_layout(Rect::new(0, 0, width, 40), 6);
            assert_eq!(c.content.width, width);
            assert_eq!(c.band.expect("band requested").width, width);
            assert_eq!(c.summary.width, width);
            assert_eq!(c.controls.width, width);
        }
    }

    #[test]
    fn columns_drop_from_the_low_value_end_before_the_name_is_squeezed() {
        const COLS: [ColumnSpec; 4] = [
            ColumnSpec {
                header: "a",
                width: 10,
                drop_rank: 0,
            },
            ColumnSpec {
                header: "b",
                width: 10,
                drop_rank: 1,
            },
            ColumnSpec {
                header: "c",
                width: 10,
                drop_rank: 3,
            },
            ColumnSpec {
                header: "d",
                width: 10,
                drop_rank: 2,
            },
        ];
        // Everything fits: nothing is dropped and the order is preserved.
        assert_eq!(fit_columns(&COLS, 100, 20), (vec![0, 1, 2, 3], 0));
        // One column too many: the highest drop_rank goes first, and the kept
        // columns stay in their original order.
        assert_eq!(fit_columns(&COLS, 55, 20), (vec![0, 1, 3], 1));
        assert_eq!(fit_columns(&COLS, 44, 20), (vec![0, 1], 2));
        assert_eq!(fit_columns(&COLS, 33, 20), (vec![0], 3));
        // The name minimum always wins; an impossibly narrow pane keeps no
        // numeric column rather than cutting the name to nothing.
        assert_eq!(fit_columns(&COLS, 20, 20), (vec![], 4));
    }

    #[test]
    fn frame_holds_the_content_minimum_when_the_terminal_is_short() {
        // The band and footer are `Length`, so a shortfall is taken from them
        // first; the content pane never drops below its minimum.
        let c = frame_layout(Rect::new(0, 0, 120, CONTENT_MIN_H + FOOTER_H), 0);
        assert_eq!(c.content.height, CONTENT_MIN_H);
        assert_eq!(c.summary.height, 1);
        assert_eq!(c.controls.height, 1);
    }

    #[test]
    fn normalized_cpu_divides_by_cores_and_clamps() {
        // Two fully-used cores on a 4-core machine -> 50% of the machine.
        assert!((normalized_cpu(200.0, 4.0) - 50.0).abs() < f32::EPSILON);
        // Idle stays at 0%.
        assert_eq!(normalized_cpu(0.0, 8.0), 0.0);
        // A transient over-count (all cores summed above 100%) is clamped.
        assert_eq!(normalized_cpu(410.0, 4.0), 100.0);
    }

    #[test]
    fn controls_drop_hints_whole_and_always_keep_quit() {
        use ratatui::widgets::Widget;
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            create_controls_with_status(
                &[("m", " merge  "), ("p", " panes  "), ("Q", " quota  ")],
                None,
                width,
            )
            .render(area, &mut buf);
            (0..width)
                .map(|x| buf.cell((x, 0)).unwrap().symbol())
                .collect::<String>()
        };

        // Wide: everything, including the repo label.
        let wide = render(120);
        assert!(wide.contains("Star on GitHub") && wide.contains("Q quota"));

        // The hard minimum terminal width. The repo label goes first, then the
        // extras from the right, and nothing is left half-drawn.
        let min = render(74);
        assert!(!min.contains("Star"), "got {min:?}");
        assert!(min.contains("q quit"), "got {min:?}");
        assert!(min.trim_end().chars().count() <= 74);

        // Absurdly narrow: the keys that still fit are whole words.
        let tiny = render(30);
        assert!(!tiny.contains("Star"), "got {tiny:?}");
        assert!(!tiny.contains("quota"), "got {tiny:?}");
        assert!(tiny.contains("quit"), "got {tiny:?}");
    }

    #[test]
    fn fit_diagnostics_drops_cpu_before_memory() {
        let (mem, cpu) = (20usize, 15usize); // realistic segment widths
        let used = 56; // primary items

        // Wide bar: both fit.
        assert_eq!(fit_diagnostics(used, 91, mem, cpu), (true, true));
        // Exactly enough for Memory but not CPU (the common ~80-col case).
        assert_eq!(fit_diagnostics(used, used + mem, mem, cpu), (true, false));
        assert_eq!(fit_diagnostics(used, 78, mem, cpu), (true, false));
        // Too narrow even for Memory: both drop, never a lone CPU.
        assert_eq!(
            fit_diagnostics(used, used + mem - 1, mem, cpu),
            (false, false)
        );
        assert_eq!(fit_diagnostics(used, 60, mem, cpu), (false, false));
    }
}
