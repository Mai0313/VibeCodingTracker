//! Frame layout, plus the comfy-table and ratatui cell / table builders shared
//! across both views.
//!
//! The builders are pure widget constructors that encode the common styling so
//! the static-table and TUI renderers stay visually consistent; the controls
//! footer's process-metrics helpers are the one exception, sampling this
//! process's own CPU and memory.
//! A recurring convention: the leading label column(s) are left-aligned and
//! every trailing (numeric) column is right-aligned. The comfy-table helpers
//! left-align the first two (index 0 and 1); the ratatui `styled_row` helper
//! takes the number of left-aligned columns as its `left_cols` argument.

use crate::display::common::style;
use crate::display::common::tui::ScrollState;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color as RatatuiColor, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Cell as RatatuiCell, HighlightSpacing, Paragraph, Row as RatatuiRow, Scrollbar,
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
/// process, cheap metrics only" contract shared by every footer refresh —
/// deliberately narrower than sysinfo's default `refresh_processes`. On Linux it
/// reads this process's stat plus `/proc/stat` for the CPU-time delta (a couple
/// of small reads), cheap enough to run on a fast metrics tick. `.with_cpu()`
/// also populates `sys.cpus()`, which the footer's own metrics helper uses to
/// normalize CPU%.
pub fn refresh_process_metrics(sys: &mut System, pid: sysinfo::Pid) {
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory(),
    );
}

/// One-time init for the footer's metrics `System`, called once after
/// `System::new()` before the refresh loop.
///
/// It populates the CPU list (`refresh_cpu_all`) so the footer's metrics helper
/// can read `sys.cpus().len()` as the CPU% divisor, then primes this process's
/// first sample. The CPU-list step is **required**: a process-only refresh does not
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

/// Separator between footer segments.
const SEP: &str = " · ";

/// This process's `97.1 MB · CPU 0.1%` readout for the right of the footer.
///
/// Both numbers read `0.0` if `sys` has no entry for `pid`, and CPU is
/// normalized to a 0-100% share of the machine. `sys` is expected to have been
/// refreshed by the caller before this call.
fn process_metrics(sys: &System, pid: sysinfo::Pid) -> String {
    let memory_mb = sys
        .process(pid)
        .map_or(0.0, |p| p.memory() as f64 / 1024.0 / 1024.0);
    // CPU normalized to a 0-100% machine share. Divide by `sys.cpus().len()`
    // (sysinfo's own basis, populated by `init_process_metrics`); `.max(1)`
    // guards the empty-CPU-list case.
    let cores = sys.cpus().len().max(1) as f32;
    let cpu_percent = sys
        .process(pid)
        .map_or(0.0, |p| normalized_cpu(p.cpu_usage(), cores));
    format!("{memory_mb:.1} MB · CPU {cpu_percent:.1}%")
}

/// Short call-to-action shown in the footer (also the OSC 8 hyperlink text).
pub const REPO_LABEL: &str = "Star on GitHub";
/// URL the footer's repository label points at when clicked.
pub const REPO_URL: &str = "https://github.com/Mai0313/VibeCodingTracker";

/// Builds the single footer row: key hints on the left, this process's metrics
/// and the GitHub repo link on the right.
///
/// `extra` inserts view-specific `(key, label)` hints just before `r refresh`,
/// and `status` carries a transient refresh state. Keys are bold and their
/// descriptions dim, so the row reads as one band of secondary text with the
/// pressable characters standing out.
///
/// Segments are dropped whole rather than clipped mid-word when they do not fit
/// `width`, in this order: the repo label, the process metrics, the
/// view-specific `extra` hints from the right, the status, then scrolling and
/// refresh. `q quit` always survives — a footer that cannot say how to quit is
/// worse than one that says less.
///
/// The repo label is drawn as plain (dim, underlined) text here; a terminal
/// hyperlink is layered on afterward by
/// [`overlay_repo_hyperlink`](super::tui::overlay_repo_hyperlink).
pub fn create_controls(
    extra: &[(&str, &str)],
    status: Option<&str>,
    sys: &System,
    pid: sysinfo::Pid,
    width: u16,
) -> Paragraph<'static> {
    let key = style::strong();
    let dim = style::secondary();
    let available = width as usize;
    let metrics = process_metrics(sys, pid);

    // Width of every part; which of them survive is decided below. The core is
    // the two indent columns plus the one hint that never drops.
    let hint_w = |k: &str, label: &str| seg_width(SEP) + seg_width(k) + 1 + seg_width(label);
    let core = 2 + seg_width("q quit");
    let repo = seg_width(SEP) + seg_width(REPO_LABEL);
    let metrics_w = seg_width(SEP) + seg_width(&metrics);
    let status_w = status.map_or(0, |s| seg_width(SEP) + seg_width(s));
    let scroll_w = hint_w("↑↓", "scroll");
    let refresh_w = hint_w("r", "refresh");
    let extra_w: Vec<usize> = extra.iter().map(|(k, label)| hint_w(k, label)).collect();

    // Give up one thing at a time, cheapest first, until what is left fits.
    // `q quit` is not on the list, so the loop always terminates with it shown.
    let mut shown_extra = extra.len();
    let mut show_status = status.is_some();
    let mut show_metrics = true;
    let mut show_repo = true;
    let mut show_scroll = true;
    let mut show_refresh = true;
    loop {
        let used = core
            + extra_w[..shown_extra].iter().sum::<usize>()
            + if show_status { status_w } else { 0 }
            + if show_metrics { metrics_w } else { 0 }
            + if show_repo { repo } else { 0 }
            + if show_scroll { scroll_w } else { 0 }
            + if show_refresh { refresh_w } else { 0 };
        if used <= available {
            break;
        }
        if show_repo {
            show_repo = false;
        } else if show_metrics {
            show_metrics = false;
        } else if shown_extra > 0 {
            shown_extra -= 1;
        } else if show_status {
            show_status = false;
        } else if show_scroll {
            show_scroll = false;
        } else if show_refresh {
            show_refresh = false;
        } else {
            break;
        }
    }

    // Left half: the keys. The indent is already there, so the separator goes
    // in front of every hint but the first.
    let mut left = vec![Span::raw("  ")];
    let hint = |spans: &mut Vec<Span<'static>>, k: &str, label: &str| {
        if spans.len() > 1 {
            spans.push(Span::styled(SEP, dim));
        }
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(format!(" {label}"), dim));
    };
    if show_scroll {
        hint(&mut left, "↑↓", "scroll");
    }
    for (k, label) in &extra[..shown_extra] {
        hint(&mut left, k, label);
    }
    if show_refresh {
        hint(&mut left, "r", "refresh");
    }
    hint(&mut left, "q", "quit");
    if show_status && let Some(status) = status {
        left.push(Span::styled(SEP, dim));
        left.push(Span::styled(status.to_string(), style::attention()));
    }

    // Right half: what the footer reports rather than what it offers.
    let mut right: Vec<Span<'static>> = Vec::new();
    if show_metrics {
        right.push(Span::styled(metrics, dim));
    }
    if show_repo {
        if !right.is_empty() {
            right.push(Span::styled(SEP, dim));
        }
        right.push(Span::styled(
            REPO_LABEL,
            dim.add_modifier(Modifier::UNDERLINED),
        ));
    }

    // Push the right half flush to the edge. Both halves were fitted above, so
    // the gap is never negative and nothing has to be clipped here.
    let mut spans = left;
    if !right.is_empty() {
        let used: usize = spans
            .iter()
            .chain(right.iter())
            .map(|s| seg_width(s.content.as_ref()))
            .sum();
        spans.push(Span::raw(" ".repeat(available.saturating_sub(used))));
        spans.extend(right);
    }
    Paragraph::new(vec![Line::from(spans)])
}

/// Rows the scrollable content pane is never squeezed below. The title, its
/// blank row and the header row eat 3, so this keeps ~3 body rows alive.
pub const CONTENT_MIN_H: u16 = 6;
/// Rows the frame always spends below the content: a blank spacer + controls.
pub const FOOTER_H: u16 = 2;
/// Columns a scrollable table spends beside its own cells: two for the
/// selection marker's gutter, one for the scrollbar. Callers fit their columns
/// into the content width less this.
pub const TABLE_SIDE_W: u16 = 3;

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
/// `band_height`-row band, then a blank spacer row and the controls footer. The
/// spacer is the only region nothing is drawn into: with no borders anywhere,
/// one empty row is what separates the footer from the band above it. Every
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
    let _spacer = take();
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

/// Title and drop-notice for a scrollable table.
pub struct TableChrome<'a> {
    /// Left-aligned title line; carries the totals for the rows underneath.
    pub title: Line<'a>,
    /// Right-aligned notice naming what the table had to leave out.
    pub note: Option<String>,
}

/// Lays `left` out with `right` pushed flush to the far edge of `width`.
///
/// Both sides are the caller's already-styled spans and the gap between them is
/// plain padding, so a right-aligned marker shares a row with the text on the
/// left instead of costing a row of its own. `right` gives way when the two
/// cannot both fit, since every caller puts the shortening-resistant half there.
pub fn spread<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> Line<'a> {
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|s| seg_width(s.content.as_ref()))
        .sum();
    let mut spans = left;
    spans.push(Span::raw(
        " ".repeat(usize::from(width).saturating_sub(used)),
    ));
    spans.extend(right);
    Line::from(spans)
}

/// True for a numeric cell that holds no quantity: `0`, `$0.00`, `0.0%`, blank.
///
/// Tested by looking for a significant digit rather than by parsing, which
/// covers every formatted shape the views produce (compact counts, money,
/// percentages) without each one having to say so.
fn is_zero_value(text: &str) -> bool {
    !text.chars().any(|c| ('1'..='9').contains(&c))
}

/// Builds a table row with the first `left_cols` cells left-aligned and the
/// rest right-aligned, painted with `row_style`.
///
/// Right-aligning the numeric columns keeps variable-width compact values
/// (`1.23M`, `999K`) flush instead of ragged. A numeric cell holding nothing but
/// zeros is dimmed on top of `row_style`, so a table of mostly-idle models reads
/// as the few numbers that carry a value.
pub fn styled_row(cells: Vec<String>, row_style: Style, left_cols: usize) -> RatatuiRow<'static> {
    let cells: Vec<RatatuiCell> = cells
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let align = if i < left_cols {
                Alignment::Left
            } else {
                Alignment::Right
            };
            let zero = i >= left_cols && is_zero_value(&s);
            let cell = RatatuiCell::from(Text::from(s).alignment(align));
            if zero {
                cell.style(style::secondary())
            } else {
                cell
            }
        })
        .collect();
    RatatuiRow::new(cells).style(row_style)
}

/// Renders a scrollable, selectable table plus a side scrollbar into `area`.
///
/// The table has no border: the title takes the first row, a blank row follows,
/// and the rest is the table itself, so the visible body height is
/// `area.height - 3`. Alignment and whitespace divide the regions instead of
/// box-drawing characters.
///
/// `rows` are the already-styled body rows (change highlighting applied by the
/// caller); `row_count` is the total displayed row count used to size the
/// scrollbar.
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
    let title = match chrome.note {
        Some(note) => spread(
            chrome.title.spans,
            vec![Span::styled(note, style::secondary())],
            area.width,
        ),
        None => chrome.title,
    };
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });

    // The title and its blank row sit above the table; the header is then the
    // only thing marking where the data starts.
    let rows_area = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    let viewport = rows_area.height.saturating_sub(1);

    // The scrollbar owns the last column, so the table is drawn one column
    // narrower than the pane. Together with the two-column selection gutter
    // that is `TABLE_SIDE_W`, which the caller fitted its columns into.
    let table = create_ratatui_table(rows, header, widths)
        .row_highlight_style(style::strong())
        .highlight_symbol(Span::styled("› ", style::accent()))
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(
        table,
        Rect {
            width: rows_area.width.saturating_sub(1),
            ..rows_area
        },
        &mut scroll.table,
    );

    // Scrollbar driven by the selected index, inset by the header row so it
    // runs beside the body and nothing else.
    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(viewport as usize)
        .position(selected);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol("┃")
            .track_symbol(Some("│"))
            .thumb_style(style::secondary())
            .track_style(style::secondary()),
        Rect {
            y: rows_area.y + 1,
            height: viewport,
            ..rows_area
        },
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
        Line::from(Span::styled("Terminal too small", style::alert())),
        Line::from(Span::styled(
            format!(
                "resize to at least {min_w} x {min_h}  (now {} x {})",
                area.width, area.height
            ),
            style::secondary(),
        )),
    ])
    .alignment(Alignment::Center);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);
    f.render_widget(para, v[1]);
}

/// Builds a borderless ratatui [`Table`](RatatuiTable) with the standard header
/// styling.
///
/// `widths` sets the per-column constraints; the header row is dimmed and sits
/// directly on top of the body, carrying no visual weight of its own.
pub fn create_ratatui_table<'a>(
    rows: Vec<RatatuiRow<'a>>,
    header: Vec<&'a str>,
    widths: &'a [Constraint],
) -> RatatuiTable<'a> {
    RatatuiTable::new(rows, widths).header(RatatuiRow::new(header).style(style::secondary()))
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

/// Builds a ratatui [`Row`](RatatuiRow) for one provider, marked with a `●` in
/// `color` and bolded when `emphasize`.
///
/// The marker is the only colored thing on the row: the name and the numbers
/// stay in the terminal's own foreground, so nine providers in a column read as
/// one table rather than as nine kinds of text.
pub fn create_provider_row<'a>(
    cells: Vec<String>,
    color: RatatuiColor,
    emphasize: bool,
) -> RatatuiRow<'a> {
    let style = if emphasize {
        style::strong()
    } else {
        style::primary()
    };

    let mut cells = cells.into_iter();
    let label = cells.next().unwrap_or_default();
    let first = RatatuiCell::from(Line::from(vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::raw(label),
    ]));
    RatatuiRow::new(
        std::iter::once(first)
            .chain(cells.map(RatatuiCell::from))
            .collect::<Vec<_>>(),
    )
    .style(style)
}

#[cfg(test)]
mod tests {
    use super::{
        CONTENT_MIN_H, ColumnSpec, FOOTER_H, Rect, create_controls, fit_columns, frame_layout,
        normalized_cpu,
    };
    use sysinfo::{Pid, System};

    /// A metrics source with no entry for the pid, so both readouts are `0.0`
    /// and no test depends on this machine's load.
    fn no_metrics() -> (System, Pid) {
        (System::new(), Pid::from(0))
    }

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

    #[test]
    fn controls_drop_segments_whole_and_always_keep_quit() {
        use ratatui::widgets::Widget;
        let (sys, pid) = no_metrics();
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            create_controls(
                &[("m", "merge"), ("p", "providers"), ("Q", "quota")],
                None,
                &sys,
                pid,
                width,
            )
            .render(area, &mut buf);
            (0..width)
                .map(|x| buf.cell((x, 0)).unwrap().symbol())
                .collect::<String>()
        };

        // Wide: everything, including the metrics and the repo label.
        let wide = render(120);
        assert!(wide.contains("Star on GitHub"), "got {wide:?}");
        assert!(
            wide.contains("Q quota") && wide.contains("CPU 0.0%"),
            "got {wide:?}"
        );
        // The repo label is pushed flush to the right edge.
        assert!(wide.trim_end().ends_with("Star on GitHub"), "got {wide:?}");

        // The hard minimum terminal width. The repo label goes first, then the
        // metrics, then the extras from the right, and nothing is half-drawn.
        let min = render(74);
        assert!(!min.contains("Star"), "got {min:?}");
        assert!(min.contains("q quit"), "got {min:?}");
        assert!(min.trim_end().chars().count() <= 74);

        // Absurdly narrow: the keys that still fit are whole words.
        let tiny = render(30);
        assert!(
            !tiny.contains("Star") && !tiny.contains("MB"),
            "got {tiny:?}"
        );
        assert!(!tiny.contains("quota"), "got {tiny:?}");
        assert!(tiny.contains("quit"), "got {tiny:?}");
    }

    #[test]
    fn a_numeric_cell_of_only_zeros_is_dimmed() {
        use ratatui::style::Modifier;
        use ratatui::widgets::{Table, Widget};

        let area = Rect::new(0, 0, 24, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let row = super::styled_row(
            vec![
                "model".to_string(),
                "$0.00".to_string(),
                "$0.01".to_string(),
            ],
            super::Style::default(),
            1,
        );
        Table::new(
            vec![row],
            [
                super::Constraint::Length(8),
                super::Constraint::Length(6),
                super::Constraint::Length(6),
            ],
        )
        .render(area, &mut buf);

        let dim_at = |x: u16| {
            buf.cell((x, 0))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::DIM)
        };
        let row: String = (0..24)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect();
        let zero_at = row.find("$0.00").expect("zero cell drawn") as u16;
        let paid_at = row.find("$0.01").expect("paid cell drawn") as u16;

        // The cell that holds nothing is the one that recedes; a cent is a
        // quantity and reads at full weight.
        assert!((0..5).all(|i| dim_at(zero_at + i)), "got {row:?}");
        assert!((0..5).all(|i| !dim_at(paid_at + i)), "got {row:?}");
        // The label column is never dimmed, whatever it says.
        assert!(!dim_at(0), "got {row:?}");
    }

    #[test]
    fn controls_give_up_the_repo_label_before_the_metrics() {
        use ratatui::widgets::Widget;
        let (sys, pid) = no_metrics();
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            create_controls(&[], None, &sys, pid, width).render(area, &mut buf);
            (0..width)
                .map(|x| buf.cell((x, 0)).unwrap().symbol())
                .collect::<String>()
        };

        // Just too narrow for both: the metrics are this process's own health,
        // the label is an invitation, so the label is the one that goes.
        let row = render(62);
        assert!(row.contains("CPU 0.0%"), "got {row:?}");
        assert!(!row.contains("Star"), "got {row:?}");
    }
}
