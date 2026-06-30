//! Terminal scaffolding for the interactive TUI.
//!
//! Covers entering / leaving raw alternate-screen mode, the polling input
//! loop ([`handle_input`]), and the state trackers that drive periodic
//! refreshes ([`RefreshState`]) and recently-changed row highlighting
//! ([`UpdateTracker`]).

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    widgets::{ScrollbarState, TableState},
};
use std::io;
use std::time::{Duration, Instant};

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
    // Mouse capture powers wheel scrolling. It also intercepts the terminal's
    // native drag-to-select; users can hold Shift to bypass it, or press `m` to
    // toggle capture off at runtime (see `handle_input` / `set_mouse_capture`).
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Toggles mouse capture on the live terminal (the `m` key handler).
///
/// # Errors
///
/// Returns an error if writing the enable/disable control sequence fails.
pub fn set_mouse_capture(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    enable: bool,
) -> anyhow::Result<()> {
    if enable {
        execute!(terminal.backend_mut(), EnableMouseCapture)?;
    } else {
        execute!(terminal.backend_mut(), DisableMouseCapture)?;
    }
    Ok(())
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
    // Disable mouse capture unconditionally; leaving it on makes the user's
    // terminal emit escape sequences on every scroll after we exit.
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
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
            Event::Key(key) => {
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
                    return Ok(InputAction::ToggleMouse);
                }
                // Navigation accumulates across the drained batch so a held key
                // or a wheel burst collapses into a single net move per tick.
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => nav.lines -= 1,
                    KeyCode::Down | KeyCode::Char('j') => nav.lines += 1,
                    KeyCode::PageUp => nav.pages -= 1,
                    KeyCode::PageDown => nav.pages += 1,
                    KeyCode::Home | KeyCode::Char('g') => nav.top = true,
                    KeyCode::End | KeyCode::Char('G') => nav.bottom = true,
                    _ => {}
                }
            }
            Event::Mouse(me) => match me.kind {
                // One row per wheel notch; the drain loop already collapses a
                // burst into a single net move.
                MouseEventKind::ScrollUp => nav.lines -= 1,
                MouseEventKind::ScrollDown => nav.lines += 1,
                _ => {}
            },
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
/// `lines` is single-row steps (arrow keys / wheel notches), `pages` is
/// page jumps (multiplied by the live viewport height by the consumer), and
/// `top` / `bottom` jump to the first / last row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavDelta {
    pub lines: i64,
    pub pages: i64,
    pub top: bool,
    pub bottom: bool,
}

impl NavDelta {
    /// Whether this delta would move the selection at all.
    pub fn is_active(&self) -> bool {
        self.lines != 0 || self.pages != 0 || self.top || self.bottom
    }
}

/// Action the TUI event loop should take in response to user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// User asked to exit (`q`, `Esc`, or `Ctrl+C`).
    Quit,
    /// User asked to re-fetch and redraw (`r` / `R`).
    Refresh,
    /// User asked to toggle mouse capture on/off (`m` / `M`).
    ToggleMouse,
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
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        let hash = hasher.finish();

        let entry_changed = match self.previous_hashes.get(&key) {
            Some(&prev_hash) => prev_hash != hash,
            None => true,
        };

        if entry_changed {
            self.last_update_times.insert(key.clone(), Instant::now());
        }

        self.previous_hashes.insert(key, hash);
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
/// [`ScrollbarState`] (drives the side scrollbar) with the last rendered body
/// height, so page jumps and the scrollbar track the live viewport. The
/// renderer updates [`viewport_rows`](Self::viewport_rows) on every draw; the
/// event loop calls [`apply`](Self::apply) / [`sync`](Self::sync) to move and
/// reconcile the selection.
#[derive(Debug, Default)]
pub struct ScrollState {
    /// Selection + offset, fed to `render_stateful_widget`.
    pub table: TableState,
    /// Side scrollbar position/extent.
    pub scrollbar: ScrollbarState,
    /// Body rows visible in the last render (used as the page-jump size).
    pub viewport_rows: u16,
}

impl ScrollState {
    /// Creates an empty state with no selection (set on the first `sync`).
    pub fn new() -> Self {
        Self {
            table: TableState::default(),
            scrollbar: ScrollbarState::default(),
            viewport_rows: 1,
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
    /// pinned TOTAL). Page jumps use the last rendered viewport height.
    pub fn apply(&mut self, nav: NavDelta, selectable: usize) {
        if selectable == 0 {
            self.table.select(None);
            return;
        }
        let max = (selectable - 1) as i64;
        let page = self.viewport_rows.max(1) as i64;
        let mut next = self.table.selected().unwrap_or(0) as i64 + nav.lines + nav.pages * page;
        if nav.top {
            next = 0;
        }
        if nav.bottom {
            next = max;
        }
        self.table.select(Some(next.clamp(0, max) as usize));
    }
}
