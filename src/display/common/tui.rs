//! Terminal scaffolding for the interactive TUI.
//!
//! Covers entering / leaving raw alternate-screen mode, the polling input
//! loop ([`handle_input`]), and the state trackers that drive periodic
//! refreshes ([`RefreshState`]) and recently-changed row highlighting
//! ([`UpdateTracker`]).

use crate::display::common::table::{REPO_LABEL, REPO_URL};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    widgets::{ScrollbarState, TableState},
};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Whether the alternate-screen TUI currently owns the terminal.
///
/// Set while [`setup_terminal`] .. [`restore_terminal`] is active so the panic
/// hook ([`force_restore_terminal`]) knows whether it must undo raw mode. A
/// bare atomic (not tied to the `Terminal` handle) is what lets the panic hook,
/// which has no access to that handle, restore the screen.
static IN_TUI: AtomicBool = AtomicBool::new(false);

/// Puts the terminal into raw mode and the alternate screen, returning a ready [`Terminal`].
///
/// Must be paired with [`restore_terminal`] before the process exits, otherwise
/// the user's terminal is left in raw / alternate-screen mode.
///
/// # Errors
///
/// Returns an error if enabling raw mode, switching to the alternate screen, or
/// constructing the backing [`Terminal`] fails (typically because stdout is not
/// a TTY or the terminal rejects the control sequences).
pub fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // We navigate with the keyboard only, so mouse reporting is left OFF. This
    // also keeps the terminal's native drag-to-select / copy working untouched.
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    IN_TUI.store(true, Ordering::SeqCst);
    Ok(terminal)
}

/// Restores the terminal to normal mode (disable raw mode, leave alternate screen, show cursor).
///
/// # Errors
///
/// Returns an error if disabling raw mode, leaving the alternate screen, or
/// re-showing the cursor fails. On error the terminal may be left partially
/// restored.
pub fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    IN_TUI.store(false, Ordering::SeqCst);
    Ok(())
}

/// Best-effort terminal restore for the panic hook, when no [`Terminal`] handle
/// is available.
///
/// No-ops unless the TUI currently owns the screen (see [`IN_TUI`]), so calling
/// it from a panic that fired outside the TUI emits nothing. Every step is
/// best-effort: a panic hook must never itself panic or early-return, so errors
/// are ignored and the flag is cleared regardless.
pub fn force_restore_terminal() {
    if !IN_TUI.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Layers an OSC 8 terminal hyperlink over the repo label the footer just drew.
///
/// ratatui's cell buffer can't carry the escape sequence itself (the bytes would
/// throw off width accounting), so this runs *after* `terminal.draw()`: it finds
/// the plain [`REPO_LABEL`] on the frame's bottom row and re-emits the identical
/// glyphs wrapped in OSC 8, pointing at [`REPO_URL`]. The visible text is
/// unchanged, so terminals without hyperlink support just ignore the wrapper.
///
/// It re-applies every frame because a redraw (resize, refresh) repaints the
/// footer as plain text; writing the same bytes again is cheap and idempotent.
///
/// # Errors
///
/// Returns an error if writing the escape sequence to stdout fails.
pub fn overlay_repo_hyperlink(buffer: &Buffer) -> io::Result<()> {
    let Some((x, y)) = find_label_start(buffer) else {
        return Ok(());
    };

    // OSC 8 open (params ; URL, ST-terminated), then cyan + underline to match
    // the label ratatui drew, then reset, then the empty OSC 8 close.
    let mut stdout = io::stdout();
    queue!(stdout, MoveTo(x, y))?;
    write!(
        stdout,
        "\x1b]8;;{REPO_URL}\x1b\\\x1b[36;4m{REPO_LABEL}\x1b[0m\x1b]8;;\x1b\\"
    )?;
    stdout.flush()
}

/// Finds the `(x, y)` of the first cell of [`REPO_LABEL`] on the frame's bottom
/// row, or `None` when it was truncated off a narrow terminal.
fn find_label_start(buffer: &Buffer) -> Option<(u16, u16)> {
    let area = buffer.area;
    if area.width == 0 || area.height == 0 {
        return None;
    }
    // The controls footer is the bottom-most row of the frame.
    let y = area.bottom() - 1;
    // REPO_LABEL is ASCII, so every byte index is a char boundary and each cell
    // holds exactly one of its characters.
    let len = REPO_LABEL.len();
    let last_x = area.right().checked_sub(len as u16)?;
    for x in area.left()..=last_x {
        let matches = (0..len).all(|i| {
            buffer
                .cell((x + i as u16, y))
                .is_some_and(|cell| cell.symbol() == &REPO_LABEL[i..=i])
        });
        if matches {
            return Some((x, y));
        }
    }
    None
}

/// Handle terminal events and return the action to take.
///
/// Blocks up to 100 ms for the first event, then drains every event already
/// buffered. Draining matters for resize: a window drag emits a burst of
/// `Resize` events, and collapsing them into a single `Resize` action keeps
/// the redraw in step with the drag instead of lagging one frame per event.
/// Returns [`InputAction::Continue`] when the poll times out with no event.
///
/// # Errors
///
/// Returns an error if polling for or reading a terminal event fails (an
/// underlying crossterm I/O error on the event source).
pub fn handle_input() -> anyhow::Result<InputAction> {
    if !event::poll(Duration::from_millis(100))? {
        return Ok(InputAction::Continue);
    }

    let mut resized = false;
    let mut nav = NavDelta::default();
    loop {
        match event::read()? {
            // Windows emits Press/Repeat/Release for a single keystroke while
            // Unix only emits Press; drop Release so one keypress isn't counted
            // twice (which would double every nav step / page jump).
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('q')
                    || key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return Ok(InputAction::Quit);
                }
                if key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R') {
                    return Ok(InputAction::Refresh);
                }
                if key.code == KeyCode::Char('m') || key.code == KeyCode::Char('M') {
                    return Ok(InputAction::ToggleMerge);
                }
                // Navigation accumulates across the drained batch so a held key
                // collapses into a single net move per tick.
                match key.code {
                    KeyCode::Up => nav.lines -= 1,
                    KeyCode::Down => nav.lines += 1,
                    _ => {}
                }
            }
            Event::Resize(_, _) => resized = true,
            _ => {}
        }

        if !event::poll(Duration::from_millis(0))? {
            break;
        }
    }

    if nav.is_active() {
        Ok(InputAction::Navigate(nav))
    } else if resized {
        Ok(InputAction::Resize)
    } else {
        Ok(InputAction::Continue)
    }
}

/// A net navigation move accumulated from one [`handle_input`] batch.
///
/// `lines` is single-row steps (arrow keys), summed across the drained event
/// batch so a held key collapses into one net move per tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavDelta {
    pub lines: i64,
}

impl NavDelta {
    /// Whether this delta would move the selection at all.
    pub fn is_active(&self) -> bool {
        self.lines != 0
    }
}

/// Action the TUI event loop should take in response to user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// User asked to exit (`q`, `Esc`, or `Ctrl+C`).
    Quit,
    /// User asked to re-fetch and redraw (`r` / `R`).
    Refresh,
    /// User toggled provider-prefix merging (`m` / `M`); usage view only,
    /// ignored elsewhere.
    ToggleMerge,
    /// User scrolled / moved the selection; redraw without re-fetching.
    Navigate(NavDelta),
    /// Terminal was resized — redraw the current frame at the new size
    /// without re-fetching session data.
    Resize,
    /// No actionable event; keep the current frame and loop.
    Continue,
}

/// Tracks when the next periodic data refresh is due, plus a one-shot force flag.
pub struct RefreshState {
    last_refresh: Instant,
    force_refresh: bool,
    refresh_interval: Duration,
}

impl RefreshState {
    /// Creates a refresh state with the given interval in seconds, primed to refresh immediately.
    ///
    /// `last_refresh` is backdated by one interval and `force_refresh` is set,
    /// so the first [`should_refresh`](Self::should_refresh) returns `true` and
    /// the initial load is not delayed by one interval.
    pub fn new(refresh_secs: u64) -> Self {
        let refresh_interval = Duration::from_secs(refresh_secs);
        Self {
            last_refresh: Instant::now() - refresh_interval,
            force_refresh: true,
            refresh_interval,
        }
    }

    /// Returns whether a refresh is due (forced, or the interval has elapsed).
    pub fn should_refresh(&self) -> bool {
        self.force_refresh || self.last_refresh.elapsed() >= self.refresh_interval
    }

    /// Records that a refresh just happened, resetting the timer and clearing the force flag.
    pub fn mark_refreshed(&mut self) {
        self.last_refresh = Instant::now();
        self.force_refresh = false;
    }

    /// Forces the next [`should_refresh`](Self::should_refresh) to return `true`.
    pub fn force(&mut self) {
        self.force_refresh = true;
    }
}

/// Tracks per-row changes to drive temporary highlighting of recently-updated rows.
///
/// To bound memory it stores a hash of each row's data rather than a clone, and
/// records the [`Instant`] a row last changed. `max_tracked` caps the number of
/// retained entries (enforced by [`cleanup`](Self::cleanup), not by
/// [`track_update`](Self::track_update)).
pub struct UpdateTracker {
    last_update_times: std::collections::HashMap<String, Instant>,
    previous_hashes: std::collections::HashMap<String, u64>,
    max_tracked: usize,
    highlight_duration: Duration,
}

/// Hashes any `Hash` value with [`DefaultHasher`](std::collections::hash_map::DefaultHasher)
/// — used to store a compact fingerprint of a row instead of a full clone.
fn hash_of<T: std::hash::Hash>(data: &T) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

impl UpdateTracker {
    /// Creates a tracker retaining up to `max_tracked` rows and highlighting changes for `highlight_duration_millis`.
    pub fn new(max_tracked: usize, highlight_duration_millis: u64) -> Self {
        Self {
            last_update_times: std::collections::HashMap::new(),
            previous_hashes: std::collections::HashMap::new(),
            max_tracked,
            highlight_duration: Duration::from_millis(highlight_duration_millis),
        }
    }

    /// Records `data` for `key`, marking it updated if its hash differs from the previous one.
    ///
    /// A first-seen `key` counts as changed and gets a fresh timestamp. Uses
    /// [`DefaultHasher`](std::collections::hash_map::DefaultHasher) over `data`
    /// to avoid retaining a full copy. This never evicts entries — call
    /// [`cleanup`](Self::cleanup) to bound growth.
    pub fn track_update<T: std::hash::Hash>(&mut self, key: String, data: &T) {
        let hash = hash_of(data);

        let entry_changed = match self.previous_hashes.get(&key) {
            Some(&prev_hash) => prev_hash != hash,
            None => true,
        };

        if entry_changed {
            self.last_update_times.insert(key.clone(), Instant::now());
        }

        self.previous_hashes.insert(key, hash);
    }

    /// Records `data`'s hash for `key` as the baseline **without** marking it
    /// updated.
    ///
    /// Used when rows are relabeled but not actually changed — e.g. toggling the
    /// provider-merge view swaps `openai/gpt-5.5` for `gpt-5.5`. Priming the new
    /// key means the next [`track_update`](Self::track_update) compares against a
    /// real baseline instead of treating it as first-seen and green-flashing the
    /// whole table on the next refresh.
    pub fn prime<T: std::hash::Hash>(&mut self, key: String, data: &T) {
        self.previous_hashes.insert(key, hash_of(data));
    }

    /// Drops tracked entries whose key is no longer present, then enforces the `max_tracked` cap.
    ///
    /// Entries for keys not in `current_keys` are removed first. If more than
    /// `max_tracked` remain, an arbitrary subset is then dropped to fit the cap.
    /// Finally both maps are shrunk to release excess capacity.
    pub fn cleanup<I>(&mut self, current_keys: I)
    where
        I: IntoIterator<Item = String>,
    {
        let current_keys: std::collections::HashSet<String> = current_keys.into_iter().collect();

        self.previous_hashes
            .retain(|key, _| current_keys.contains(key));
        self.last_update_times
            .retain(|key, _| current_keys.contains(key));

        // If we exceed max_tracked, drop an arbitrary subset to fit the cap.
        // NOTE: HashMap iteration order is unspecified, so this is not "most recent".
        if self.previous_hashes.len() > self.max_tracked {
            let keys_to_remove: Vec<_> = self
                .previous_hashes
                .keys()
                .take(self.previous_hashes.len() - self.max_tracked)
                .cloned()
                .collect();
            for key in keys_to_remove {
                self.previous_hashes.remove(&key);
                self.last_update_times.remove(&key);
            }
        }

        // Release excess capacity to reduce memory footprint
        self.previous_hashes.shrink_to_fit();
        self.last_update_times.shrink_to_fit();
    }

    /// Returns whether `key` changed within the configured highlight window.
    ///
    /// `false` for keys never tracked or whose last change is older than
    /// `highlight_duration`.
    pub fn is_recently_updated(&self, key: &str) -> bool {
        self.last_update_times
            .get(key)
            .map(|update_time| {
                Instant::now().duration_since(*update_time) < self.highlight_duration
            })
            .unwrap_or(false)
    }
}

/// Scroll + selection state for a scrollable TUI table.
///
/// Bundles the ratatui [`TableState`] (drives auto-scroll + row highlight) and
/// [`ScrollbarState`] (drives the side scrollbar). The event loop calls
/// [`apply`](Self::apply) / [`sync`](Self::sync) to move and reconcile the
/// selection.
#[derive(Debug, Default)]
pub struct ScrollState {
    /// Selection + offset, fed to `render_stateful_widget`.
    pub table: TableState,
    /// Side scrollbar position/extent.
    pub scrollbar: ScrollbarState,
}

impl ScrollState {
    /// Creates an empty state with no selection (set on the first `sync`).
    pub fn new() -> Self {
        Self {
            table: TableState::default(),
            scrollbar: ScrollbarState::default(),
        }
    }

    /// Reconciles the selection with a freshly aggregated row set.
    ///
    /// Rows can be reordered or appear/disappear between refreshes, so the
    /// previously selected `prev_model` is matched by name first; failing that
    /// the existing index is clamped into range. `models` lists the selectable
    /// model names (excluding any pinned TOTAL row).
    pub fn sync(&mut self, prev_model: Option<&str>, models: &[String]) {
        if models.is_empty() {
            self.table.select(None);
            return;
        }
        let max = models.len() - 1;
        let idx = prev_model
            .and_then(|m| models.iter().position(|x| x == m))
            .unwrap_or_else(|| self.table.selected().unwrap_or(0).min(max));
        self.table.select(Some(idx.min(max)));
    }

    /// Applies a navigation delta, clamping the selection to `[0, selectable-1]`.
    ///
    /// `selectable` is the number of selectable rows (model rows, not the
    /// pinned TOTAL).
    pub fn apply(&mut self, nav: NavDelta, selectable: usize) {
        if selectable == 0 {
            self.table.select(None);
            return;
        }
        let max = (selectable - 1) as i64;
        let next = self.table.selected().unwrap_or(0) as i64 + nav.lines;
        self.table.select(Some(next.clamp(0, max) as usize));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::common::table::create_controls;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn find_label_start_locates_repo_label_on_bottom_row() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        create_controls(&[("m", " merge  ")]).render(area, &mut buf);

        let (x, y) = find_label_start(&buf).expect("repo label should be present");
        assert_eq!(y, 0);
        let got: String = (0..REPO_LABEL.len())
            .map(|i| buf.cell((x + i as u16, y)).unwrap().symbol())
            .collect();
        assert_eq!(got, REPO_LABEL);
    }

    #[test]
    fn find_label_start_is_none_when_truncated() {
        // Too narrow to fit the whole label → nothing to hyperlink.
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        create_controls(&[]).render(area, &mut buf);
        assert!(find_label_start(&buf).is_none());
    }
}
