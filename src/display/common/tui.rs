use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::{Duration, Instant};

/// Setup the terminal for TUI mode
pub fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to normal mode
pub fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Handle keyboard input and return whether to quit
pub fn handle_input() -> anyhow::Result<InputAction> {
    if event::poll(Duration::from_millis(100))? {
        if let Event::Key(key) = event::read()? {
            if key.code == KeyCode::Char('q')
                || key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                return Ok(InputAction::Quit);
            }
            if key.code == KeyCode::Char('r') || key.code == KeyCode::Char('R') {
                return Ok(InputAction::Refresh);
            }
        }
    }
    Ok(InputAction::Continue)
}

/// Action to take based on user input
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Quit,
    Refresh,
    Continue,
}

/// Refresh state tracker
pub struct RefreshState {
    last_refresh: Instant,
    force_refresh: bool,
    refresh_interval: Duration,
}

impl RefreshState {
    /// Create a new refresh state with the given interval in seconds
    pub fn new(refresh_secs: u64) -> Self {
        let refresh_interval = Duration::from_secs(refresh_secs);
        Self {
            last_refresh: Instant::now() - refresh_interval,
            force_refresh: true,
            refresh_interval,
        }
    }

    /// Check if it's time to refresh
    pub fn should_refresh(&self) -> bool {
        self.force_refresh || self.last_refresh.elapsed() >= self.refresh_interval
    }

    /// Mark that a refresh has occurred
    pub fn mark_refreshed(&mut self) {
        self.last_refresh = Instant::now();
        self.force_refresh = false;
    }

    /// Force the next refresh
    pub fn force(&mut self) {
        self.force_refresh = true;
    }
}

/// Update tracking for row highlighting (optimized to use hashes instead of full data clones)
pub struct UpdateTracker {
    last_update_times: std::collections::HashMap<String, Instant>,
    previous_hashes: std::collections::HashMap<String, u64>,
    max_tracked: usize,
    highlight_duration: Duration,
}

impl UpdateTracker {
    /// Create a new update tracker
    pub fn new(max_tracked: usize, highlight_duration_millis: u64) -> Self {
        Self {
            last_update_times: std::collections::HashMap::new(),
            previous_hashes: std::collections::HashMap::new(),
            max_tracked,
            highlight_duration: Duration::from_millis(highlight_duration_millis),
        }
    }

    /// Track an update for a given key and data (using hash for comparison)
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

    /// Clean up old entries based on current row keys
    pub fn cleanup<I>(&mut self, current_keys: I)
    where
        I: IntoIterator<Item = String>,
    {
        let current_keys: std::collections::HashSet<String> = current_keys.into_iter().collect();

        self.previous_hashes
            .retain(|key, _| current_keys.contains(key));
        self.last_update_times
            .retain(|key, _| current_keys.contains(key));

        // If we exceed max_tracked, keep only the most recent entries
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

    /// Check if a key was recently updated
    pub fn is_recently_updated(&self, key: &str) -> bool {
        self.last_update_times
            .get(key)
            .map(|update_time| {
                Instant::now().duration_since(*update_time) < self.highlight_duration
            })
            .unwrap_or(false)
    }
}
