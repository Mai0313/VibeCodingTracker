//! Terminal scaffolding for the interactive TUI.
//!
//! Covers entering / leaving raw alternate-screen mode, the polling input
//! loop ([`handle_input`]), the worker that drives periodic refreshes
//! ([`RefreshWorker`]), and recently-changed row highlighting ([`UpdateTracker`]).

use crate::display::common::table::{REPO_LABEL, REPO_URL};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    buffer::Buffer,
    layout::{Constraint, Flex, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, ScrollbarState, TableState},
};
use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// Whether the alternate-screen TUI currently owns the terminal.
///
/// Set while [`setup_terminal`] .. [`restore_terminal`] is active so the panic
/// hook ([`force_restore_terminal`]) knows whether it must undo raw mode. A
/// bare atomic (not tied to the `Terminal` handle) is what lets the panic hook,
/// which has no access to that handle, restore the screen.
static IN_TUI: AtomicBool = AtomicBool::new(false);
static TUI_OWNER: std::sync::Mutex<Option<thread::ThreadId>> = std::sync::Mutex::new(None);
static TERMINAL_PANIC_HOOK: Once = Once::new();
const LOADING_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_DRAINED_EVENTS: usize = 64;

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
    ensure_terminal_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // We navigate with the keyboard only, so mouse reporting is left OFF. This
    // also keeps the terminal's native drag-to-select / copy working untouched.
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
        return Err(error.into());
    }
    IN_TUI.store(true, Ordering::SeqCst);
    set_tui_owner(Some(thread::current().id()));
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            force_restore_terminal();
            Err(error.into())
        }
    }
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
    let mut failures = Vec::new();
    if let Err(error) = disable_raw_mode() {
        failures.push(format!("disable raw mode: {error}"));
    }
    if let Err(error) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        failures.push(format!("leave alternate screen: {error}"));
    }
    if let Err(error) = terminal.show_cursor() {
        failures.push(format!("show cursor: {error}"));
    }
    IN_TUI.store(false, Ordering::SeqCst);
    set_tui_owner(None);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; ")))
    }
}

/// RAII owner for an active terminal session.
///
/// Every exit path attempts all cleanup steps. Explicitly calling
/// [`Self::close`] surfaces cleanup failures; dropping remains best-effort so
/// an earlier application error is not replaced during unwinding.
pub struct TerminalSession {
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
}

impl TerminalSession {
    /// Enters raw alternate-screen mode with rollback on partial setup failure.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            terminal: Some(setup_terminal()?),
        })
    }

    /// Mutable access to the ratatui terminal.
    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        self.terminal.as_mut().expect("terminal session is open")
    }

    /// Restores the terminal now, attempting every cleanup step.
    pub fn close(&mut self) -> anyhow::Result<()> {
        let Some(mut terminal) = self.terminal.take() else {
            return Ok(());
        };
        restore_terminal(&mut terminal)
    }

    /// Restores the terminal and preserves both application and cleanup errors.
    pub fn finish<T>(&mut self, result: anyhow::Result<T>) -> anyhow::Result<T> {
        combine_terminal_results(result, self.close())
    }
}

fn combine_terminal_results<T>(
    result: anyhow::Result<T>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => Err(anyhow::anyhow!(
            "{error:#}; terminal cleanup failed: {cleanup_error:#}"
        )),
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Some(mut terminal) = self.terminal.take() {
            let _ = restore_terminal(&mut terminal);
        }
    }
}

enum RefreshCommand {
    Run,
    Shutdown,
}

/// Failure returned by a background refresh worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshWorkerError {
    /// The loader returned an application error and can be retried.
    Load(String),
    /// The worker thread exited, so future refresh requests cannot run.
    Disconnected,
}

impl std::fmt::Display for RefreshWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(error) => formatter.write_str(error),
            Self::Disconnected => formatter.write_str("refresh worker disconnected"),
        }
    }
}

/// Single background loader with one active and one coalesced pending refresh.
pub struct RefreshWorker<T> {
    command_tx: SyncSender<RefreshCommand>,
    result_rx: Receiver<std::result::Result<T, RefreshWorkerError>>,
    shutdown: std::sync::Arc<AtomicBool>,
    active: bool,
    pending: bool,
    disconnected: bool,
    interval: Duration,
    next_due: Instant,
}

impl<T: Send + 'static> RefreshWorker<T> {
    /// Spawns a detached worker that owns `loader` and all of its mutable cache.
    pub fn new<F>(refresh_secs: u64, mut loader: F) -> Self
    where
        F: FnMut() -> anyhow::Result<T> + Send + 'static,
    {
        Self::new_with_init(refresh_secs, move || move || loader())
    }

    /// Spawns a worker whose stateful loader is constructed inside the thread.
    ///
    /// This supports thread-confined state such as `Rc`-backed pricing maps:
    /// only the Send initializer crosses the thread boundary.
    pub fn new_with_init<I, F>(refresh_secs: u64, init: I) -> Self
    where
        I: FnOnce() -> F + Send + 'static,
        F: FnMut() -> anyhow::Result<T> + 'static,
    {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let worker_shutdown = std::sync::Arc::clone(&shutdown);
        thread::spawn(move || {
            let mut loader = init();
            while let Ok(command) = command_rx.recv() {
                if worker_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                match command {
                    RefreshCommand::Run => {
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut loader))
                                .map_err(|_| {
                                    RefreshWorkerError::Load("refresh loader panicked".to_string())
                                })
                                .and_then(|result| {
                                    result.map_err(|error| {
                                        RefreshWorkerError::Load(format!("{error:#}"))
                                    })
                                });
                        if result_tx.send(result).is_err() {
                            break;
                        }
                    }
                    RefreshCommand::Shutdown => break,
                }
            }
        });
        Self {
            command_tx,
            result_rx,
            shutdown,
            active: false,
            pending: false,
            disconnected: false,
            interval: Duration::from_secs(refresh_secs.max(1)),
            next_due: Instant::now(),
        }
    }

    /// Starts the initial load or coalesces a request behind the active load.
    pub fn request(&mut self) {
        if self.disconnected {
            return;
        }
        if self.active {
            self.pending = true;
            return;
        }
        if self.command_tx.send(RefreshCommand::Run).is_ok() {
            self.active = true;
        }
    }

    /// Starts an automatic refresh when the completion-based deadline is due.
    ///
    /// Returns `true` only when this call dispatched a new load, allowing the
    /// event loop to render its refreshing state immediately.
    pub fn request_if_due(&mut self) -> bool {
        if !self.disconnected && !self.active && Instant::now() >= self.next_due {
            self.request();
            return self.active;
        }
        false
    }

    /// Starts the automatic timer without dispatching a load immediately.
    pub fn defer_until_interval(&mut self) {
        self.next_due = Instant::now() + self.interval;
    }

    /// Returns a completed load without blocking.
    ///
    /// The next automatic deadline starts now, after completion. If input was
    /// coalesced while the job ran, the pending job is dispatched immediately.
    pub fn try_result(&mut self) -> Option<std::result::Result<T, RefreshWorkerError>> {
        let result = match self.result_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) if self.disconnected => return None,
            Err(TryRecvError::Disconnected) => {
                self.disconnected = true;
                self.pending = false;
                Err(RefreshWorkerError::Disconnected)
            }
        };
        self.active = false;
        self.next_due = Instant::now() + self.interval;
        if self.pending {
            self.pending = false;
            self.request();
        }
        Some(result)
    }

    /// Whether a loader is currently running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    #[cfg(test)]
    fn has_pending(&self) -> bool {
        self.pending
    }
}

/// Draws a centered loading spinner that also works on very small terminals.
pub fn render_loading_frame(
    terminal: &mut Terminal<impl Backend>,
    spinner_index: usize,
) -> anyhow::Result<()> {
    terminal.draw(|frame| render_loading(frame, spinner_index, "Loading sessions..."))?;
    Ok(())
}

/// Resolves the footer status consistently for every redraw path.
pub fn refresh_status(active: bool, failure_until: Option<Instant>) -> Option<&'static str> {
    if active {
        Some("Refreshing...")
    } else if failure_until.is_some_and(|until| Instant::now() < until) {
        Some("Refresh failed")
    } else {
        None
    }
}

fn render_loading(frame: &mut Frame, spinner_index: usize, message: &str) {
    let area = frame.area();
    let [middle] = Layout::vertical([Constraint::Length(3)])
        .flex(Flex::Center)
        .areas(area);
    let text = Line::from(format!(
        "{} {message}",
        LOADING_SPINNER[spinner_index % LOADING_SPINNER.len()]
    ));
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(Color::Cyan))
            .centered()
            .block(Block::default().borders(Borders::ALL)),
        middle,
    );
}

impl<T> Drop for RefreshWorker<T> {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.command_tx.try_send(RefreshCommand::Shutdown);
    }
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
    set_tui_owner(None);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Whether a TUI currently owns the process terminal.
pub(crate) fn terminal_session_active() -> bool {
    IN_TUI.load(Ordering::SeqCst)
}

/// Whether the current thread owns the active TUI session.
pub(crate) fn current_thread_owns_terminal() -> bool {
    let owner = TUI_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    owner
        .as_ref()
        .is_some_and(|owner| *owner == thread::current().id())
}

/// Decides whether a panic must restore an active terminal session.
pub(crate) fn panic_requires_terminal_restore(
    tui_active: bool,
    owner_thread: bool,
    aborting: bool,
) -> bool {
    tui_active && (aborting || owner_thread)
}

fn panic_delegates_to_previous(tui_active: bool, restore_terminal: bool) -> bool {
    !tui_active || restore_terminal
}

/// Installs terminal panic protection once while preserving the previous hook.
///
/// Public display callers do not have to initialize this crate's logger first.
/// Owner-thread panics restore the terminal before the previous hook runs. A
/// caught background panic does not delegate while the TUI remains active, so
/// it cannot print through the alternate screen.
pub(crate) fn ensure_terminal_panic_hook() {
    TERMINAL_PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let tui_active = terminal_session_active();
            let restore_terminal = panic_requires_terminal_restore(
                tui_active,
                current_thread_owns_terminal(),
                cfg!(panic = "abort"),
            );
            if restore_terminal {
                force_restore_terminal();
            }
            log::error!("{info}");
            if panic_delegates_to_previous(tui_active, restore_terminal) {
                previous(info);
            }
        }));
    });
}

fn set_tui_owner(owner: Option<thread::ThreadId>) {
    *TUI_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = owner;
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
/// Blocks up to 100 ms for the first event, then drains a bounded batch of
/// already-buffered events. Batching collapses resize bursts without allowing a
/// continuous event stream to postpone redraw indefinitely.
/// Returns [`InputAction::Continue`] when the poll times out with no event.
///
/// # Errors
///
/// Returns an error if polling for or reading a terminal event fails (an
/// underlying crossterm I/O error on the event source).
pub fn handle_input() -> anyhow::Result<InputAction> {
    handle_input_from(&mut CrosstermEventSource)
}

trait EventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read(&mut self) -> io::Result<Event>;
}

struct CrosstermEventSource;

impl EventSource for CrosstermEventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        event::read()
    }
}

fn handle_input_from(source: &mut impl EventSource) -> anyhow::Result<InputAction> {
    if !source.poll(Duration::from_millis(100))? {
        return Ok(InputAction::Continue);
    }

    let mut resized = false;
    let mut nav = NavDelta::default();
    let mut drained = 0usize;
    loop {
        match source.read()? {
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

        drained += 1;
        if drained >= MAX_DRAINED_EVENTS || !source.poll(Duration::from_millis(0))? {
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
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, mpsc};

    struct FakeEventSource {
        events: VecDeque<Event>,
        reads: usize,
    }

    impl FakeEventSource {
        fn new(events: impl IntoIterator<Item = Event>) -> Self {
            Self {
                events: events.into_iter().collect(),
                reads: 0,
            }
        }
    }

    impl EventSource for FakeEventSource {
        fn poll(&mut self, _timeout: Duration) -> io::Result<bool> {
            Ok(!self.events.is_empty())
        }

        fn read(&mut self) -> io::Result<Event> {
            self.reads += 1;
            self.events
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no fake event"))
        }
    }

    #[test]
    fn background_unwind_panic_keeps_live_tui_owned() {
        assert!(!panic_requires_terminal_restore(true, false, false));
        assert!(panic_requires_terminal_restore(true, true, false));
        assert!(panic_requires_terminal_restore(true, false, true));
        assert!(!panic_requires_terminal_restore(false, true, true));
        assert!(!panic_delegates_to_previous(true, false));
        assert!(panic_delegates_to_previous(true, true));
        assert!(panic_delegates_to_previous(false, false));
    }

    #[test]
    fn terminal_result_preserves_application_and_cleanup_errors() {
        let error = combine_terminal_results::<()>(
            Err(anyhow::anyhow!("draw failed")),
            Err(anyhow::anyhow!("restore failed")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("draw failed"));
        assert!(error.contains("terminal cleanup failed: restore failed"));
    }

    #[test]
    fn active_refresh_takes_precedence_over_recent_failure() {
        let failure_until = Some(Instant::now() + Duration::from_secs(1));
        assert_eq!(refresh_status(true, failure_until), Some("Refreshing..."));
        assert_eq!(refresh_status(false, failure_until), Some("Refresh failed"));
        assert_eq!(
            refresh_status(false, Some(Instant::now() - Duration::from_secs(1))),
            None
        );
    }

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

    #[test]
    fn refresh_worker_coalesces_repeated_requests() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_calls = Arc::clone(&calls);
        let mut worker = RefreshWorker::new(60, move || {
            let call = worker_calls.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            started_tx.send(call).unwrap();
            if call == 1 {
                release_rx.recv().unwrap();
            }
            Ok(call)
        });

        worker.request();
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        worker.request();
        worker.request();
        worker.request();
        assert!(worker.has_pending());
        release_tx.send(()).unwrap();

        let first = wait_for_result(&mut worker).unwrap();
        assert_eq!(first, 1);
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        let second = wait_for_result(&mut worker).unwrap();
        assert_eq!(second, 2);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[test]
    fn automatic_refresh_reports_when_it_dispatches() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut worker = RefreshWorker::new(60, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(1)
        });

        assert!(worker.request_if_due());
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(worker.is_active());
        assert!(!worker.request_if_due());
        release_tx.send(()).unwrap();
        assert_eq!(wait_for_result(&mut worker), Ok(1));
    }

    #[test]
    fn input_drain_is_bounded_during_resize_burst() {
        let events = (0..MAX_DRAINED_EVENTS + 10).map(|index| {
            Event::Resize(
                80 + u16::try_from(index).unwrap(),
                24 + u16::try_from(index).unwrap(),
            )
        });
        let mut source = FakeEventSource::new(events);

        assert_eq!(handle_input_from(&mut source).unwrap(), InputAction::Resize);
        assert_eq!(source.reads, MAX_DRAINED_EVENTS);
        assert_eq!(source.events.len(), 10);
    }

    #[test]
    fn active_slow_loader_does_not_block_quit_or_resize_input() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut worker = RefreshWorker::new(60, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(1)
        });
        worker.request();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let mut resize = FakeEventSource::new([Event::Resize(100, 30)]);
        assert_eq!(handle_input_from(&mut resize).unwrap(), InputAction::Resize);

        let mut quit = FakeEventSource::new([Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        ))]);
        assert_eq!(handle_input_from(&mut quit).unwrap(), InputAction::Quit);
        assert!(worker.is_active());

        release_tx.send(()).unwrap();
        assert_eq!(wait_for_result(&mut worker), Ok(1));
    }

    #[test]
    fn dropping_worker_cancels_a_queued_initial_load() {
        let (release_init_tx, release_init_rx) = mpsc::channel();
        let (called_tx, called_rx) = mpsc::channel();
        let mut worker = RefreshWorker::new_with_init(60, move || {
            release_init_rx.recv().unwrap();
            move || {
                called_tx.send(()).unwrap();
                Ok(())
            }
        });

        worker.request();
        drop(worker);
        release_init_tx.send(()).unwrap();
        assert!(called_rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn loader_panic_is_reported_once_and_worker_can_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let mut worker = RefreshWorker::new(60, move || {
            if worker_calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                panic!("synthetic loader panic");
            }
            Ok(7)
        });

        worker.request();
        assert_eq!(
            wait_for_result(&mut worker),
            Err(RefreshWorkerError::Load(
                "refresh loader panicked".to_string()
            ))
        );
        worker.request();
        assert_eq!(wait_for_result(&mut worker), Ok(7));
    }

    fn wait_for_result(worker: &mut RefreshWorker<usize>) -> Result<usize, RefreshWorkerError> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(result) = worker.try_result() {
                return result;
            }
            assert!(Instant::now() < deadline, "worker result timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
