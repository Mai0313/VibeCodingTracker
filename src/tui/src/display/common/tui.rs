//! Terminal scaffolding for the interactive TUI.
//!
//! Covers raw alternate-screen setup and restoration, bounded input polling ([`handle_input`], [`handle_key_input`]), background refresh workers ([`RefreshWorker`]), and row-change highlighting ([`UpdateTracker`]).

use crate::display::common::table::{REPO_LABEL, REPO_URL};
use crate::display::common::theme::{self, theme};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::Color,
    text::Line,
    widgets::{Block, Borders, Paragraph, ScrollbarState, TableState},
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Tracks whether the alternate-screen TUI currently owns the terminal for [`force_restore_terminal`].
static IN_TUI: AtomicBool = AtomicBool::new(false);
static TUI_OWNER: std::sync::Mutex<Option<thread::ThreadId>> = std::sync::Mutex::new(None);
static TERMINAL_PANIC_HOOK: Once = Once::new();
const LOADING_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MAX_DRAINED_EVENTS: usize = 64;

/// Enters raw mode, the alternate screen, and mouse capture, returning a ready [`Terminal`].
///
/// Pair with [`restore_terminal`] before exit to avoid leaving the terminal in raw or alternate-screen mode.
///
/// # Errors
///
/// Returns an error if enabling raw mode, switching to the alternate screen, enabling mouse capture, or creating the [`Terminal`] fails.
pub fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    ensure_terminal_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
        return Err(error.into());
    }
    // Probe before enabling mouse capture so interleaved SGR mouse packets on stdin cannot stall the OSC 10/11 reply; `probe` is a no-op after the first call.
    theme::probe();
    if let Err(error) = execute!(stdout, EnableMouseCapture) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen, Show);
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

/// Restores normal terminal mode (disables raw mode and mouse capture, leaves the alternate screen, and shows the cursor).
///
/// # Errors
///
/// Returns an error if any restoration step fails; all steps are still attempted, leaving the terminal partially restored on error.
pub fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = disable_raw_mode() {
        failures.push(format!("disable raw mode: {error}"));
    }
    if let Err(error) = execute!(terminal.backend_mut(), DisableMouseCapture) {
        failures.push(format!("disable mouse capture: {error}"));
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
/// Every exit path attempts all cleanup steps. [`Self::close`] and [`Self::finish`] surface cleanup errors; `Drop` ignores cleanup errors during unwinding.
pub struct TerminalSession {
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
}

impl TerminalSession {
    /// Enters raw alternate-screen mode with rollback on partial setup failure.
    ///
    /// # Errors
    ///
    /// Returns whatever [`setup_terminal`] returns.
    pub fn new() -> anyhow::Result<Self> {
        let terminal = setup_terminal()?;
        Ok(Self {
            terminal: Some(terminal),
        })
    }

    /// Returns mutable access to the underlying ratatui [`Terminal`].
    ///
    /// # Panics
    ///
    /// Panics after [`Self::close`] or [`Self::finish`] has closed the session.
    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        self.terminal.as_mut().expect("terminal session is open")
    }

    /// Temporarily restores the terminal for `run` (such as an external editor), then re-enters the TUI and clears for a full redraw.
    ///
    /// # Panics
    ///
    /// Panics after the session has been closed.
    ///
    /// # Errors
    ///
    /// Returns an error if leaving or re-entering terminal mode fails.
    pub fn suspend<T>(&mut self, run: impl FnOnce() -> T) -> anyhow::Result<T> {
        let terminal = self.terminal.as_mut().expect("terminal session is open");
        restore_terminal(terminal)?;
        let output = run();
        // Rebuild `Terminal` for empty buffers instead of calling `Terminal::clear`, which queries cursor position and fails on terminals that do not answer.
        self.terminal = None;
        let mut terminal = setup_terminal()?;
        // Explicitly clear the alternate screen because not every terminal clears it on re-entry and empty diff buffers skip blank cells.
        execute!(terminal.backend_mut(), Clear(ClearType::All))?;
        self.terminal = Some(terminal);
        Ok(output)
    }

    /// Restores the terminal immediately, attempting every cleanup step.
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
    /// The loader returned an error or panicked, and can be retried.
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

/// Minimum interval between disk writes of a refresh worker's session ledger.
pub const LEDGER_SAVE_INTERVAL: Duration = Duration::from_secs(60);

/// Returns sorted `(path, mtime, len)` stamps for every remote provider snapshot under `<cache_dir>/remotes/`.
///
/// Fast-paths to an empty vector with a single directory check when no remote snapshots directory exists.
pub(crate) fn remote_snapshot_stamps(cache_dir: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let remotes_root = cache_dir.join(vct_core::ledger::REMOTES_DIR_NAME);
    let Ok(hosts) = std::fs::read_dir(&remotes_root) else {
        return Vec::new();
    };
    let mut stamps = Vec::new();
    for host in hosts.flatten() {
        let sessions_dir = host.path().join(vct_core::ledger::LEDGER_DIR_NAME);
        let Ok(files) = std::fs::read_dir(&sessions_dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.is_file()
            {
                stamps.push((path, meta.modified().ok(), meta.len()));
            }
        }
    }
    stamps.sort_by(|a, b| a.0.cmp(&b.0));
    stamps
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

    /// Spawns a worker whose stateful loader is constructed on the worker thread.
    ///
    /// Only the `Send` initializer `init` crosses the thread boundary, allowing thread-confined state inside `F`.
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
                        // Catch panics on unwinding profiles (dev/test) so the worker stays alive for retries; release/dist use `panic = "abort"` and terminate before this runs.
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

    /// Dispatches a load, or coalesces the request behind the running one.
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
    /// Returns `true` only when this call dispatched a new load.
    pub fn request_if_due(&mut self) -> bool {
        if !self.disconnected && !self.active && Instant::now() >= self.next_due {
            self.request();
            return self.active;
        }
        false
    }

    /// Sets the automatic refresh interval (minimum 1 second), taking effect after the next completed load.
    pub fn set_interval(&mut self, refresh_secs: u64) {
        self.interval = Duration::from_secs(refresh_secs.max(1));
    }

    /// Returns a completed load without blocking, if one is ready.
    ///
    /// Resets the automatic refresh deadline from completion time and immediately dispatches any coalesced pending request.
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

    /// Returns whether a loader invocation is currently running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    #[cfg(test)]
    fn has_pending(&self) -> bool {
        self.pending
    }
}

/// Returns the footer status string for an active refresh or an unexpired failure window.
pub fn refresh_status(active: bool, failure_until: Option<Instant>) -> Option<&'static str> {
    if active {
        Some("Refreshing...")
    } else if failure_until.is_some_and(|until| Instant::now() < until) {
        Some("Refresh failed")
    } else {
        None
    }
}

/// Draws a centered bordered loading spinner and `message` into `area`.
pub fn render_loading(frame: &mut Frame, area: Rect, spinner_index: usize, message: &str) {
    let [middle] = Layout::vertical([Constraint::Length(3)])
        .flex(Flex::Center)
        .areas(area);
    let text = Line::from(format!(
        "{} {message}",
        LOADING_SPINNER[spinner_index % LOADING_SPINNER.len()]
    ));
    frame.render_widget(
        Paragraph::new(text).centered().block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme().border()),
        ),
        middle,
    );
}

impl<T> Drop for RefreshWorker<T> {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.command_tx.try_send(RefreshCommand::Shutdown);
    }
}

/// Best-effort terminal restore for the panic hook when no [`Terminal`] handle is available.
///
/// No-ops unless the TUI currently owns the terminal; clears the ownership flag and ignores all I/O errors.
pub fn force_restore_terminal() {
    if !IN_TUI.swap(false, Ordering::SeqCst) {
        return;
    }
    set_tui_owner(None);
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

/// Returns whether a TUI session currently owns the process terminal.
pub(crate) fn terminal_session_active() -> bool {
    IN_TUI.load(Ordering::SeqCst)
}

/// Returns whether the current thread owns the active TUI session.
pub(crate) fn current_thread_owns_terminal() -> bool {
    let owner = TUI_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    owner
        .as_ref()
        .is_some_and(|owner| *owner == thread::current().id())
}

/// Returns whether a panic must restore an active terminal session.
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

/// Installs the terminal panic hook once while chaining to the previous hook.
///
/// Owner-thread panics (and aborting panics) restore the terminal before delegating to the previous hook; caught background panics skip delegation while the TUI remains active.
pub fn ensure_terminal_panic_hook() {
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

/// Layers an OSC 8 terminal hyperlink pointing to [`REPO_URL`] over the rendered [`REPO_LABEL`] on the bottom row of `buffer`.
///
/// Call after `terminal.draw()` on every frame; ratatui's cell buffer cannot hold raw escape sequences without breaking width accounting, and redraws repaint the footer as plain text. No-ops when [`REPO_LABEL`] is absent from the bottom row.
///
/// # Errors
///
/// Returns an error if writing or flushing the escape sequence to stdout fails.
pub fn overlay_repo_hyperlink(buffer: &Buffer) -> io::Result<()> {
    let Some((x, y)) = find_label_start(buffer) else {
        return Ok(());
    };

    // Emit OSC 8 open, dim + underline matching the ratatui cell, reset, and OSC 8 close.
    let color = match theme().dim {
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        _ => "90".to_string(),
    };
    let mut stdout = io::stdout();
    queue!(stdout, MoveTo(x, y))?;
    write!(
        stdout,
        "\x1b]8;;{REPO_URL}\x1b\\\x1b[{color};4m{REPO_LABEL}\x1b[0m\x1b]8;;\x1b\\"
    )?;
    stdout.flush()
}

/// Returns the `(x, y)` cell coordinates of [`REPO_LABEL`] on the bottom row of `buffer`, or `None` if absent.
fn find_label_start(buffer: &Buffer) -> Option<(u16, u16)> {
    let area = buffer.area;
    if area.width == 0 || area.height == 0 {
        return None;
    }
    // The controls footer is the bottom-most row of the frame.
    let y = area.bottom() - 1;
    // `REPO_LABEL` is ASCII, so each byte corresponds to one single-width cell.
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

/// Polls for terminal events and returns the mapped [`InputAction`].
///
/// Blocks up to 100 ms for the first event, then drains up to 64 already-buffered events to coalesce resize and navigation bursts without starving redraws. Returns [`InputAction::Continue`] on timeout.
///
/// # Errors
///
/// Returns an error if polling or reading a terminal event fails.
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
    let mut scroll: Option<(u16, u16, i16)> = None;
    let mut drained = 0usize;
    loop {
        match source.read()? {
            // Ignore `KeyEventKind::Release` (emitted on Windows alongside Press/Repeat) so one keystroke is not counted twice.
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return Ok(InputAction::Quit);
                }
                match key.code {
                    // Esc backs out of what is open and never leaves the app.
                    KeyCode::Esc => return Ok(InputAction::Close),
                    KeyCode::Enter => return Ok(InputAction::Open),
                    KeyCode::Char(' ') => return Ok(InputAction::Space),
                    KeyCode::PageUp => return Ok(InputAction::Page(-1)),
                    KeyCode::PageDown => return Ok(InputAction::Page(1)),
                    KeyCode::Char('?') => return Ok(InputAction::Help),
                    KeyCode::Tab => return Ok(InputAction::NextTab),
                    KeyCode::BackTab => return Ok(InputAction::PrevTab),
                    KeyCode::Char('[') => return Ok(InputAction::StepRange(-1)),
                    KeyCode::Char(']') => return Ok(InputAction::StepRange(1)),
                    KeyCode::Left => return Ok(InputAction::StepSort(-1)),
                    KeyCode::Right => return Ok(InputAction::StepSort(1)),
                    _ => {}
                }
                if key.code == KeyCode::Char('p') || key.code == KeyCode::Char('P') {
                    return Ok(InputAction::TogglePane);
                }
                if key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R') {
                    return Ok(InputAction::Refresh);
                }
                // Accumulate arrow navigation across the drained batch so held keys collapse into one net move per tick.
                match key.code {
                    KeyCode::Up => nav.lines -= 1,
                    KeyCode::Down => nav.lines += 1,
                    _ => {}
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    return Ok(InputAction::Click {
                        column: mouse.column,
                        row: mouse.row,
                    });
                }
                MouseEventKind::ScrollUp => {
                    let prev = scroll.map_or(0, |(_, _, lines)| lines);
                    scroll = Some((mouse.column, mouse.row, prev.saturating_sub(1)));
                }
                MouseEventKind::ScrollDown => {
                    let prev = scroll.map_or(0, |(_, _, lines)| lines);
                    scroll = Some((mouse.column, mouse.row, prev.saturating_add(1)));
                }
                _ => {}
            },
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
    } else if let Some((column, row, lines)) = scroll
        && lines != 0
    {
        Ok(InputAction::Scroll { column, row, lines })
    } else if resized {
        Ok(InputAction::Resize)
    } else {
        Ok(InputAction::Continue)
    }
}

/// Polls up to 100 ms for an event and returns key presses unmapped as [`InputAction::Key`].
///
/// Maps `Ctrl+C` to [`InputAction::Quit`], left clicks to [`InputAction::Click`], and wheel ticks to [`InputAction::Scroll`], while draining buffered resize and cursor-motion events.
///
/// # Errors
///
/// Returns an error if polling or reading a terminal event fails.
pub fn handle_key_input() -> anyhow::Result<InputAction> {
    handle_key_input_from(&mut CrosstermEventSource)
}

fn handle_key_input_from(source: &mut impl EventSource) -> anyhow::Result<InputAction> {
    if !source.poll(Duration::from_millis(100))? {
        return Ok(InputAction::Continue);
    }
    let mut resized = false;
    let mut scroll: Option<(u16, u16, i16)> = None;
    let mut drained = 0usize;
    loop {
        match source.read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(InputAction::Quit);
                }
                return Ok(InputAction::Key(key.code));
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    return Ok(InputAction::Click {
                        column: mouse.column,
                        row: mouse.row,
                    });
                }
                MouseEventKind::ScrollUp => {
                    let prev = scroll.map_or(0, |(_, _, lines)| lines);
                    scroll = Some((mouse.column, mouse.row, prev.saturating_sub(1)));
                }
                MouseEventKind::ScrollDown => {
                    let prev = scroll.map_or(0, |(_, _, lines)| lines);
                    scroll = Some((mouse.column, mouse.row, prev.saturating_add(1)));
                }
                _ => {}
            },
            Event::Resize(_, _) => resized = true,
            _ => {}
        }

        drained += 1;
        if drained >= MAX_DRAINED_EVENTS || !source.poll(Duration::from_millis(0))? {
            break;
        }
    }
    if let Some((column, row, lines)) = scroll
        && lines != 0
    {
        Ok(InputAction::Scroll { column, row, lines })
    } else if resized {
        Ok(InputAction::Resize)
    } else {
        Ok(InputAction::Continue)
    }
}

/// Net vertical navigation delta accumulated from one [`handle_input`] batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavDelta {
    /// Net row offset (negative moves up, positive moves down).
    pub lines: i64,
}

impl NavDelta {
    /// Returns whether this delta is non-zero.
    pub fn is_active(&self) -> bool {
        self.lines != 0
    }
}

/// Primary mouse click or vertical scroll wheel input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseInput {
    /// Left mouse button pressed at `(column, row)`.
    Click { column: u16, row: u16 },
    /// Vertical scroll wheel movement (`negative` = up, `positive` = down).
    Scroll { column: u16, row: u16, lines: i16 },
}

/// Action the TUI event loop should take in response to user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// User asked to exit (`q` or `Ctrl+C`).
    Quit,
    /// User pressed `Esc`: close whatever is open.
    Close,
    /// User pressed `Enter`: open the selected row's details.
    Open,
    /// User pressed `Space` on a non-form tab (e.g. inline row expand).
    Space,
    /// User pressed `PageUp` (`-1`) or `PageDown` (`1`).
    Page(isize),
    /// User asked for the key list (`?`).
    Help,
    /// User clicked the primary mouse button at `(column, row)`.
    Click { column: u16, row: u16 },
    /// User spun the mouse wheel vertically at `(column, row)`.
    Scroll { column: u16, row: u16, lines: i16 },
    /// User moved to the next tab (`Tab`).
    NextTab,
    /// User moved to the previous tab (`Shift+Tab`).
    PrevTab,
    /// User stepped the time range back (`[`) or forward (`]`).
    StepRange(isize),
    /// User moved the sort one column left (`←`) or right (`→`).
    StepSort(isize),
    /// User toggled the Provider Usage panel (`p` / `P`).
    TogglePane,
    /// User asked to re-fetch and redraw (`r` / `R`).
    Refresh,
    /// User scrolled / moved the selection; redraw without re-fetching.
    Navigate(NavDelta),
    /// A key press, unmapped; only [`handle_key_input`] returns it.
    Key(KeyCode),
    /// Terminal was resized; redraw at the new size without re-fetching.
    Resize,
    /// No actionable event; keep the current frame and loop.
    Continue,
}

impl InputAction {
    /// Returns the underlying [`MouseInput`] when this action originated from the mouse.
    pub fn mouse(self) -> Option<MouseInput> {
        match self {
            Self::Click { column, row } => Some(MouseInput::Click { column, row }),
            Self::Scroll { column, row, lines } => Some(MouseInput::Scroll { column, row, lines }),
            _ => None,
        }
    }
}

/// Tracks per-row hash changes to highlight recently updated rows.
///
/// Stores a hash and last-change [`Instant`] per key rather than cloning row data. The `max_tracked` cap is enforced by [`Self::cleanup`], not by [`Self::track_update`].
pub struct UpdateTracker {
    last_update_times: std::collections::HashMap<String, Instant>,
    previous_hashes: std::collections::HashMap<String, u64>,
    max_tracked: usize,
    highlight_duration: Duration,
}

/// Returns a compact [`DefaultHasher`](std::collections::hash_map::DefaultHasher) fingerprint of `data`.
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

    /// Records `data` for `key`, marking it updated when its hash differs from the previous value or `key` is first seen.
    ///
    /// Does not evict entries; call [`Self::cleanup`] to enforce `max_tracked`.
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

    /// Records `data`'s hash for `key` as a baseline without marking it updated.
    ///
    /// Prevents a key rename (such as toggling provider-prefix merging) from flashing the row as newly changed on the next [`Self::track_update`].
    pub fn prime<T: std::hash::Hash>(&mut self, key: String, data: &T) {
        self.previous_hashes.insert(key, hash_of(data));
    }

    /// Removes entries absent from `current_keys`, trims remaining entries to `max_tracked`, and shrinks the internal maps.
    ///
    /// When trimming to `max_tracked`, evicted keys are chosen in arbitrary `HashMap` iteration order.
    pub fn cleanup<I>(&mut self, current_keys: I)
    where
        I: IntoIterator<Item = String>,
    {
        let current_keys: std::collections::HashSet<String> = current_keys.into_iter().collect();

        self.previous_hashes
            .retain(|key, _| current_keys.contains(key));
        self.last_update_times
            .retain(|key, _| current_keys.contains(key));

        // HashMap iteration order is unspecified, so trimming is not LRU.
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

        self.previous_hashes.shrink_to_fit();
        self.last_update_times.shrink_to_fit();
    }

    /// Returns whether `key` changed within the configured highlight duration.
    pub fn is_recently_updated(&self, key: &str) -> bool {
        self.last_update_times
            .get(key)
            .map(|update_time| {
                Instant::now().duration_since(*update_time) < self.highlight_duration
            })
            .unwrap_or(false)
    }
}

/// Scroll and selection state for a scrollable TUI table, pairing [`TableState`] and [`ScrollbarState`].
#[derive(Debug, Default)]
pub struct ScrollState {
    /// Selection + offset, fed to `render_stateful_widget`.
    pub table: TableState,
    /// Side scrollbar position/extent.
    pub scrollbar: ScrollbarState,
}

impl ScrollState {
    /// Creates an empty state with no active selection.
    pub fn new() -> Self {
        Self {
            table: TableState::default(),
            scrollbar: ScrollbarState::default(),
        }
    }

    /// Reconciles the selection against a refreshed list of selectable `models` (excluding any pinned total row).
    ///
    /// Matches `prev_model` by name first; otherwise clamps the existing selection index into range, or clears it when `models` is empty.
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

    /// Applies `nav`, clamping the selection to `[0, selectable - 1]` (or clearing it when `selectable == 0`).
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
        create_controls(&[("m", "merge")], area.width).render(area, &mut buf);

        let (x, y) = find_label_start(&buf).expect("repo label should be present");
        assert_eq!(y, 0);
        let got: String = (0..REPO_LABEL.len())
            .map(|i| buf.cell((x + i as u16, y)).unwrap().symbol())
            .collect();
        assert_eq!(got, REPO_LABEL);
    }

    #[test]
    fn find_label_start_is_none_when_the_label_was_dropped() {
        // Too narrow for the label: the footer drops it whole, leaving nothing to hyperlink or clip.
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        create_controls(&[], area.width).render(area, &mut buf);
        assert!(find_label_start(&buf).is_none());

        let row: String = (0..area.width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect();
        assert!(
            row.contains("q quit"),
            "quitting stays discoverable: {row:?}"
        );
        assert!(!row.contains("Star"), "no half-drawn label: {row:?}");
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
    fn each_control_key_maps_to_its_own_action() {
        let press = |code| {
            let mut source = FakeEventSource::new([Event::Key(crossterm::event::KeyEvent::new(
                code,
                KeyModifiers::NONE,
            ))]);
            handle_input_from(&mut source).unwrap()
        };

        assert_eq!(press(KeyCode::Char('q')), InputAction::Quit);
        assert_eq!(press(KeyCode::Char('r')), InputAction::Refresh);
        assert_eq!(press(KeyCode::Char('m')), InputAction::Continue);
        // Esc is distinct from Quit so views with overlays can dismiss them without exiting.
        assert_eq!(press(KeyCode::Esc), InputAction::Close);
        assert_eq!(press(KeyCode::Char('1')), InputAction::Continue);
        assert_eq!(press(KeyCode::Char('3')), InputAction::Continue);
        assert_eq!(press(KeyCode::Tab), InputAction::NextTab);
        assert_eq!(press(KeyCode::BackTab), InputAction::PrevTab);
        assert_eq!(press(KeyCode::Enter), InputAction::Open);
        assert_eq!(press(KeyCode::Char(' ')), InputAction::Space);
        assert_eq!(press(KeyCode::PageUp), InputAction::Page(-1));
        assert_eq!(press(KeyCode::PageDown), InputAction::Page(1));
        assert_eq!(press(KeyCode::Char('?')), InputAction::Help);
        assert_eq!(press(KeyCode::Char('[')), InputAction::StepRange(-1));
        assert_eq!(press(KeyCode::Char(']')), InputAction::StepRange(1));
        assert_eq!(press(KeyCode::Left), InputAction::StepSort(-1));
        assert_eq!(press(KeyCode::Right), InputAction::StepSort(1));
        assert_eq!(press(KeyCode::Char('p')), InputAction::TogglePane);
        assert_eq!(press(KeyCode::Char('P')), InputAction::TogglePane);

        let mut ctrl_c = FakeEventSource::new([Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))]);
        assert_eq!(handle_input_from(&mut ctrl_c).unwrap(), InputAction::Quit);
    }

    #[test]
    fn primary_clicks_map_to_click_actions_across_both_input_modes() {
        let mouse = |kind, column, row| {
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        let mut normal = FakeEventSource::new([
            mouse(MouseEventKind::Moved, 4, 0),
            mouse(MouseEventKind::Down(MouseButton::Right), 8, 0),
            mouse(MouseEventKind::Down(MouseButton::Left), 12, 0),
        ]);
        assert_eq!(
            handle_input_from(&mut normal).unwrap(),
            InputAction::Click { column: 12, row: 0 }
        );

        let mut raw = FakeEventSource::new([
            mouse(MouseEventKind::Moved, 4, 0),
            mouse(MouseEventKind::Up(MouseButton::Left), 8, 0),
            mouse(MouseEventKind::Down(MouseButton::Left), 25, 0),
        ]);
        assert_eq!(
            handle_key_input_from(&mut raw).unwrap(),
            InputAction::Click { column: 25, row: 0 }
        );

        let mut wheel_normal = FakeEventSource::new([
            mouse(MouseEventKind::ScrollDown, 10, 5),
            mouse(MouseEventKind::ScrollDown, 10, 5),
            mouse(MouseEventKind::ScrollUp, 12, 6),
        ]);
        assert_eq!(
            handle_input_from(&mut wheel_normal).unwrap(),
            InputAction::Scroll {
                column: 12,
                row: 6,
                lines: 1,
            }
        );

        let mut wheel_raw = FakeEventSource::new([
            mouse(MouseEventKind::ScrollUp, 8, 4),
            mouse(MouseEventKind::ScrollUp, 8, 4),
        ]);
        assert_eq!(
            handle_key_input_from(&mut wheel_raw).unwrap(),
            InputAction::Scroll {
                column: 8,
                row: 4,
                lines: -2,
            }
        );
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
