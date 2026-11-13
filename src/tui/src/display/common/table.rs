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

use crate::display::common::theme::theme;
use crate::display::common::tui::ScrollState;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color as RatatuiColor, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell as RatatuiCell, Clear, Paragraph, Row as RatatuiRow, Scrollbar,
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
/// also populates `sys.cpus()`, which `process_metrics` uses to normalize CPU%.
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
/// It populates the CPU list (`refresh_cpu_all`) so `process_metrics` can read
/// `sys.cpus().len()` as the CPU% divisor, then primes this process's first
/// sample. The CPU-list step is **required**: a process-only refresh does not
/// initialize `sys.cpus()` on macOS, which would leave the divisor at 1 and
/// over-report CPU%. The list is stable, so it is never refreshed again — the
/// per-tick path stays [`refresh_process_metrics`].
pub fn init_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_cpu_all();
    refresh_process_metrics(sys, pid);
}

/// Display width of an ASCII footer fragment (labels / formatted numbers are
/// ASCII, so char count equals column count).
fn seg_width(s: &str) -> usize {
    s.chars().count()
}

/// This process's resident memory in MB and its CPU use as a 0-100% share of
/// the machine, both `0.0` when `sys` has no entry for `pid`.
///
/// `sys` is expected to have been refreshed by the caller before this call.
pub fn process_metrics(sys: &System, pid: sysinfo::Pid) -> (f64, f32) {
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);
    // Divide by `sys.cpus().len()` (sysinfo's own basis, populated by
    // `init_process_metrics`); `.max(1)` guards the empty-CPU-list case.
    let cores = sys.cpus().len().max(1) as f32;
    let cpu_percent = sys
        .process(pid)
        .map_or(0.0, |p| normalized_cpu(p.cpu_usage(), cores));
    (memory_mb, cpu_percent)
}

/// Short call-to-action shown in the footer (also the OSC 8 hyperlink text).
pub const REPO_LABEL: &str = "Star on GitHub";
/// URL the footer's repository label points at when clicked.
pub const REPO_URL: &str = "https://github.com/Mai0313/VibeCodingTracker";

/// The hints every tab's footer ends with.
const CORE_HINTS: [(&str, &str); 3] = [("1-4/tab", "view"), ("?", "help"), ("q", "quit")];

/// Builds the single-line key-hint footer with no status.
///
/// This is the no-status shorthand for [`create_controls_with_status`].
pub fn create_controls(hints: &[(&str, &str)], width: u16) -> Paragraph<'static> {
    create_controls_with_status(hints, None, width)
}

/// Builds the footer every tab shares: `hints` as `(key, what it does)` pairs,
/// the hints common to all tabs, an optional status in its color, and the
/// GitHub repo label at the right edge.
///
/// What does not fit `width` is dropped whole rather than clipped mid-word, in
/// this order: the repo label, then `hints` from the right. The common hints
/// stay, since `?` lists whatever was dropped. A status too long to sit beside
/// them takes the row: it is the only place a failure is reported.
///
/// The repo label is drawn as plain underlined text; a terminal hyperlink is
/// layered on afterward by
/// [`overlay_repo_hyperlink`](super::tui::overlay_repo_hyperlink).
pub fn create_controls_with_status(
    hints: &[(&str, &str)],
    status: Option<(&str, RatatuiColor)>,
    width: u16,
) -> Paragraph<'static> {
    let theme = theme();
    let dim = theme.dim();
    let available = width as usize;
    let hint_w = |(key, label): &(&str, &str)| seg_width(key) + 1 + seg_width(label) + 2;

    let core: usize = CORE_HINTS.iter().map(hint_w).sum::<usize>() - 1;
    let repo = seg_width(REPO_LABEL) + 3;
    let status_w = status.map_or(0, |(text, _)| seg_width(text) + 5);
    let mut shown = hints.len();
    let mut show_repo = true;
    let used = |shown: usize, repo_shown: bool| {
        hints[..shown].iter().map(hint_w).sum::<usize>()
            + core
            + status_w
            + if repo_shown { repo } else { 0 }
    };
    if used(shown, show_repo) > available {
        show_repo = false;
    }
    while shown > 0 && used(shown, show_repo) > available {
        shown -= 1;
    }

    let mut spans = Vec::new();
    if let Some((text, color)) = status
        && used(0, false) > available
    {
        spans.push(Span::styled(
            format!(" {text}"),
            Style::default().fg(color).bold(),
        ));
        return Paragraph::new(Line::from(spans));
    }
    for (key, label) in hints[..shown].iter().chain(&CORE_HINTS) {
        let gap = if spans.is_empty() { " " } else { "  " };
        spans.push(Span::styled(format!("{gap}{key}"), Style::default().bold()));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    if let Some((text, color)) = status {
        spans.push(Span::styled("  ·  ", dim));
        spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(color).bold(),
        ));
    }
    if show_repo {
        let gap = available.saturating_sub(used(shown, true)) + 2;
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(REPO_LABEL, dim.underlined()));
    }
    Paragraph::new(Line::from(spans))
}

/// Rows the scrollable content pane is never squeezed below. The block border,
/// header row and header margin eat 4, so this keeps ~2 body rows alive.
pub const CONTENT_MIN_H: u16 = 6;
/// Rows the frame always spends below the content: the controls line.
pub const FOOTER_H: u16 = 1;

/// Chunk rects for an interactive frame.
pub struct FrameChunks {
    /// Scrollable main table area, always the full width of the frame.
    pub content: Rect,
    /// Full-width band under the content, present only when one was asked for.
    pub band: Option<Rect>,
    /// Single-line controls footer area.
    pub controls: Rect,
}

/// Splits `area` into the standard interactive-view regions.
///
/// The content pane absorbs all leftover height, then an optional
/// `band_height`-row band, then one row of controls. Every
/// region spans the full width: a view that wants to show more than its table
/// spends rows on a band, never columns on a side pane, so the table's own
/// columns are never the thing that pays for it.
pub fn frame_layout(area: Rect, band_height: u16) -> FrameChunks {
    let mut constraints: Vec<Constraint> = Vec::with_capacity(3);
    constraints.push(Constraint::Min(CONTENT_MIN_H));
    if band_height > 0 {
        constraints.push(Constraint::Length(band_height));
    }
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
    let controls = take();

    FrameChunks {
        content,
        band,
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

/// Title, drop-notice and sort mark for a scrollable table.
pub struct TableChrome<'a> {
    /// Left-aligned title; carries the totals for the rows underneath.
    pub title: &'a str,
    /// Right-aligned notice naming what the table had to leave out.
    pub note: Option<String>,
    /// Index of the header the rows are ordered by, when it is on screen.
    pub sorted: Option<usize>,
}

/// A block title from `text`: the part before the first ` · ` bold, each
/// separator dim.
pub fn title_line(text: &str) -> Line<'static> {
    let dim = theme().dim();
    let mut spans = vec![Span::raw(" ")];
    for (at, part) in text.split(" · ").enumerate() {
        if at > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        let style = if at == 0 {
            theme().title()
        } else {
            theme().title().not_bold()
        };
        spans.push(Span::styled(part.to_string(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// `cells` as table cells, the first `left_cols` left-aligned and the rest
/// right-aligned.
pub fn aligned_cells(cells: Vec<String>, left_cols: usize) -> Vec<RatatuiCell<'static>> {
    cells
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
        .collect()
}

/// Builds a table row with the first `left_cols` cells left-aligned and the
/// rest right-aligned, painted with `style`.
///
/// Right-aligning the numeric columns keeps variable-width compact values
/// (`1.23M`, `999K`) flush instead of ragged.
pub fn styled_row(cells: Vec<String>, style: Style, left_cols: usize) -> RatatuiRow<'static> {
    RatatuiRow::new(aligned_cells(cells, left_cols)).style(style)
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
    let theme = theme();

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title_line(chrome.title))
        .border_style(theme.border());
    if let Some(note) = chrome.note {
        block =
            block.title(Line::from(Span::styled(format!(" {note} "), theme.dim())).right_aligned());
    }

    // Selection is shown purely by the row color (no leading symbol / gutter).
    let table = create_ratatui_table(rows, header, widths, chrome.sorted)
        .block(block)
        .row_highlight_style(theme.selected());

    f.render_stateful_widget(table, area, &mut scroll.table);

    // Scrollbar driven by the selected index, inset by one row so it sits
    // between the block's top/bottom borders instead of over the corners.
    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(viewport as usize)
        .position(selected);
    // Nothing to scroll to when every row is on screen.
    if row_count <= usize::from(viewport) {
        return;
    }
    f.render_stateful_widget(
        // Drawn as the border itself, with the part in view a heavier line.
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .track_style(theme.border())
            .thumb_style(theme.dim()),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scroll.scrollbar,
    );
}

/// Draws a centered panel over `area` with the details of one table row:
/// `name` in full, wrapped where it is wider than the panel, then `label
/// value` rows. Closed with `Esc`.
pub fn render_detail_panel(f: &mut Frame, area: Rect, name: &str, rows: &[(&str, String)]) {
    let theme = theme();
    let width = area.width.saturating_sub(4).min(52);
    let inner_w = usize::from(width.saturating_sub(4)).max(1);
    let name: Vec<char> = name.chars().collect();
    let mut lines: Vec<Line> = name
        .chunks(inner_w)
        .map(|chunk| {
            Line::from(Span::styled(
                format!(" {}", chunk.iter().collect::<String>()),
                Style::default().bold(),
            ))
        })
        .collect();
    lines.push(Line::default());
    lines.extend(rows.iter().map(|(label, value)| {
        let pad = inner_w.saturating_sub(label.chars().count() + value.chars().count());
        Line::from(vec![
            Span::styled(format!(" {label}"), theme.dim()),
            Span::raw(" ".repeat(pad)),
            Span::raw(value.clone()),
        ])
    }));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.accent())
                .title(Line::from(Span::styled(" Details ", theme.title())))
                .title(Line::from(Span::styled(" esc close ", theme.dim())).right_aligned()),
        ),
        rect,
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
            Style::default().fg(theme().bad).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "resize to at least {min_w} x {min_h}  (now {} x {})",
                area.width, area.height
            ),
            theme().dim(),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme().border()),
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
/// `widths` sets the per-column constraints. The header is dim with a
/// one-line bottom margin, aligned like the body (first column left, the rest
/// right); the `sorted` header, if any, is drawn in the accent color.
pub fn create_ratatui_table<'a>(
    rows: Vec<RatatuiRow<'a>>,
    header: Vec<&'a str>,
    widths: &'a [Constraint],
    sorted: Option<usize>,
) -> RatatuiTable<'a> {
    let theme = theme();
    let header = header.into_iter().enumerate().map(|(i, name)| {
        let align = if i == 0 {
            Alignment::Left
        } else {
            Alignment::Right
        };
        let style = if sorted == Some(i) {
            theme.accent().bold()
        } else {
            theme.dim()
        };
        RatatuiCell::from(Text::from(name).alignment(align)).style(style)
    });
    RatatuiTable::new(rows, widths)
        .header(RatatuiRow::new(header).bottom_margin(1))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border()),
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

/// Builds a provider's ratatui [`Row`](RatatuiRow): a dot in the provider's
/// `color` before its name, the remaining cells right-aligned, bold when
/// `emphasize`.
pub fn create_provider_row<'a>(
    cells: Vec<String>,
    color: RatatuiColor,
    emphasize: bool,
) -> RatatuiRow<'a> {
    let mut cells = cells.into_iter();
    let name = cells.next().unwrap_or_default();
    let mut row = vec![RatatuiCell::from(Line::from(vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::raw(name),
    ]))];
    row.extend(cells.map(|cell| RatatuiCell::from(Text::from(cell).alignment(Alignment::Right))));
    let style = if emphasize {
        Style::default().bold()
    } else {
        Style::default()
    };
    RatatuiRow::new(row).style(style)
}

#[cfg(test)]
mod tests {
    use super::{
        CONTENT_MIN_H, ColumnSpec, FOOTER_H, Rect, create_controls_with_status, fit_columns,
        frame_layout, normalized_cpu,
    };

    #[test]
    fn frame_gives_every_leftover_row_to_the_content_pane() {
        let area = Rect::new(0, 0, 120, 40);
        let c = frame_layout(area, 0);
        assert_eq!(c.content.height, 40 - FOOTER_H);
        assert!(c.band.is_none());
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

    fn controls(hints: &[(&str, &str)], status: Option<&str>, width: u16) -> String {
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, width, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let status = status.map(|text| (text, ratatui::style::Color::Yellow));
        create_controls_with_status(hints, status, width).render(area, &mut buf);
        (0..width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect::<String>()
    }

    #[test]
    fn controls_drop_hints_whole_and_always_keep_quit() {
        let render = |width: u16| {
            controls(
                &[("m", "merge"), ("p", "providers"), ("[ ]", "range")],
                None,
                width,
            )
        };

        // Wide: everything, the repo label at the right edge.
        let wide = render(120);
        assert!(wide.trim_end().ends_with("Star on GitHub"), "got {wide:?}");
        assert!(wide.contains("[ ] range  1-4/tab view  ? help  q quit"));

        // The repo label goes first, then the tab's hints from the right, and
        // nothing is left half-drawn.
        let narrow = render(52);
        assert!(!narrow.contains("Star"), "got {narrow:?}");
        assert!(!narrow.contains("range"), "got {narrow:?}");
        assert!(
            narrow.contains("m merge  p providers  1-4/tab view  ? help  q quit"),
            "got {narrow:?}"
        );

        // The common hints stay even when no tab hint fits.
        let tiny = render(30);
        assert_eq!(tiny.trim(), "1-4/tab view  ? help  q quit");
    }

    #[test]
    fn the_detail_panel_shows_a_long_name_in_full() {
        use ratatui::{Terminal, backend::TestBackend};
        let name = "fireworks-ai/accounts/fireworks/models/qwen3-coder-480k-a35b-instruct";
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal
            .draw(|f| {
                super::render_detail_panel(f, f.area(), name, &[("Cost", "$1.00".to_string())]);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let shown: String = (0..16)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .filter(|symbol| !matches!(*symbol, " " | "│"))
            .collect();
        assert!(shown.contains(name), "{shown}");
        assert!(shown.contains("Cost$1.00"), "{shown}");
    }

    #[test]
    fn a_status_sits_beside_the_hints_or_takes_the_row() {
        let short = controls(&[("r", "refresh")], Some("Refreshing..."), 80);
        assert!(short.contains("q quit  ·  Refreshing..."), "got {short:?}");
        let long = "not saved: config.toml does not parse, fix it first (e)";
        let taken = controls(&[("r", "refresh")], Some(long), 70);
        assert_eq!(taken.trim(), long);
    }
}
