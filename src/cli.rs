//! clap command-line surface and the time-range filter it resolves to.
//!
//! The `///` comments on [`Commands`] and their arguments are what clap
//! renders as `--help` text, so they read as user-facing prose. [`Cli`] is
//! the parsed top-level structure; [`resolve_time_range`] collapses the
//! mutually-exclusive `--daily` / `--weekly` / `--monthly` / `--all` flags
//! into a single [`TimeRange`].

use clap::{Parser, Subcommand, ValueEnum};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Time window applied when aggregating sessions.
///
/// Each variant maps to a cutoff date via [`TimeRange::cutoff_date`];
/// [`TimeRange::All`] is the default and disables filtering. Serializes as a
/// lowercase string (`"daily"` / `"weekly"` / `"monthly"` / `"all"`) so it can
/// be reused verbatim as the `config.general.default_time_range` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TimeRange {
    /// Today only.
    Daily,
    /// The current week, starting Monday.
    Weekly,
    /// The current calendar month, from the 1st.
    Monthly,
    /// No filtering — every session.
    #[default]
    All,
}

impl TimeRange {
    /// Returns the inclusive cutoff date for this range, or `None` for
    /// [`TimeRange::All`].
    ///
    /// The cutoff is computed against today in the system local timezone:
    /// `Weekly` anchors on the most recent Monday and `Monthly` on the first
    /// of the month. Sessions on or after the returned date are kept.
    ///
    /// # Panics
    ///
    /// Does not panic: the `with_day(1)` used for `Monthly` is always valid
    /// because day 1 exists in every month.
    pub fn cutoff_date(&self) -> Option<chrono::NaiveDate> {
        use chrono::{Datelike, Local};
        let today = Local::now().date_naive();
        match self {
            TimeRange::All => None,
            TimeRange::Daily => Some(today),
            TimeRange::Weekly => {
                let days_since_monday = today.weekday().num_days_from_monday() as i64;
                Some(today - chrono::Duration::days(days_since_monday))
            }
            TimeRange::Monthly => Some(today.with_day(1).unwrap()),
        }
    }
}

/// Collapses the period flags into a single [`TimeRange`].
///
/// Checks `daily`, then `weekly`, then `monthly`, returning the first that is
/// set; falls back to [`TimeRange::All`] when none are. The flags are mutually
/// exclusive at the clap layer (shared `period` group), so at most one is ever
/// true here.
pub fn resolve_time_range(daily: bool, weekly: bool, monthly: bool) -> TimeRange {
    resolve_time_range_with_default(daily, weekly, monthly, false, TimeRange::All)
}

/// Collapses the period flags into a [`TimeRange`], falling back to `default`
/// when the user passed none of them.
///
/// An explicit flag always wins (`--all` maps to [`TimeRange::All`]); only when
/// every flag is unset does `default` (from `config.general.default_time_range`)
/// apply. The flags share clap's `period` group, so at most one is ever true.
pub fn resolve_time_range_with_default(
    daily: bool,
    weekly: bool,
    monthly: bool,
    all: bool,
    default: TimeRange,
) -> TimeRange {
    if daily {
        TimeRange::Daily
    } else if weekly {
        TimeRange::Weekly
    } else if monthly {
        TimeRange::Monthly
    } else if all {
        TimeRange::All
    } else {
        default
    }
}

/// A provider whose raw quota/usage API response `vct fetch` can print.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum FetchProvider {
    /// Claude Code (`GET /api/oauth/usage`).
    Claude,
    /// OpenAI Codex (ChatGPT `wham/usage`).
    Codex,
    /// GitHub Copilot CLI (`GET /copilot_internal/user`).
    Copilot,
    /// Cursor CLI (`GET /api/usage-summary`).
    Cursor,
}

/// Vibe Coding Tracker - AI coding assistant usage analyzer.
#[derive(Parser, Debug)]
#[command(name = "vibe_coding_tracker")]
#[command(author, version = crate::VERSION, about, long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands exposed by the CLI.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Analyze JSONL conversation files (single file or all sessions).
    Analysis {
        /// Path to the JSONL file to analyze (if not provided, analyzes all sessions).
        #[arg(short, long, conflicts_with_all = ["json", "text", "table"])]
        path: Option<PathBuf>,

        /// Optional output path to save analysis result as JSON.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output raw JSON instead of table view.
        #[arg(long, group = "analysis_format")]
        json: bool,

        /// Output as plain text.
        #[arg(long, group = "analysis_format")]
        text: bool,

        /// Output as static table (instead of interactive TUI).
        #[arg(long, group = "analysis_format")]
        table: bool,

        /// Show only today's data.
        #[arg(long, group = "period")]
        daily: bool,

        /// Show only this week's data.
        #[arg(long, group = "period")]
        weekly: bool,

        /// Show only this month's data.
        #[arg(long, group = "period")]
        monthly: bool,

        /// Show all data (default).
        #[arg(short, long, group = "period")]
        all: bool,
    },

    /// Display token usage statistics.
    Usage {
        /// Optional output path to save usage result as JSON.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output raw JSON instead of table view.
        #[arg(long, group = "usage_format")]
        json: bool,

        /// Output as plain text.
        #[arg(long, group = "usage_format")]
        text: bool,

        /// Output as static table.
        #[arg(long, group = "usage_format")]
        table: bool,

        /// Merge models that share a base name across provider prefixes
        /// (e.g. `openai/gpt-5.5` + `azure/gpt-5.5`). In the TUI this seeds the
        /// initial state; press `m` to toggle. Ignored for `--json`.
        #[arg(long)]
        merge_providers: bool,

        /// Show only today's data.
        #[arg(long, group = "period")]
        daily: bool,

        /// Show only this week's data.
        #[arg(long, group = "period")]
        weekly: bool,

        /// Show only this month's data.
        #[arg(long, group = "period")]
        monthly: bool,

        /// Show all data (default).
        #[arg(short, long, group = "period")]
        all: bool,
    },

    /// Display version information.
    Version {
        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Output as plain text.
        #[arg(long)]
        text: bool,
    },

    /// Update to the latest version from GitHub releases.
    Update {
        /// Check for updates without installing.
        #[arg(long)]
        check: bool,

        /// Force update without confirmation prompt.
        #[arg(long, short)]
        force: bool,
    },

    /// Fetch a provider's raw quota/usage API response.
    Fetch {
        /// Which provider to fetch (claude | codex | copilot | cursor).
        provider: FetchProvider,

        /// Output as pretty JSON (default).
        #[arg(long, group = "fetch_format")]
        json: bool,

        /// Output as flattened plain text.
        #[arg(long, group = "fetch_format")]
        text: bool,

        /// Output as a flattened key/value table.
        #[arg(long, group = "fetch_format")]
        table: bool,
    },

    /// Show or edit the persistent settings file (`~/.vct/config.toml`).
    Config {
        /// What to do; defaults to showing the current settings.
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
}

/// Actions for the `config` subcommand.
#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ConfigAction {
    /// Print the config file path.
    Path,
    /// Print the current settings (default).
    Show,
    /// Open the config file in `$VISUAL` / `$EDITOR`.
    Edit,
    /// Print the JSON schema for the settings file.
    ///
    /// Redirect it to regenerate the committed schema:
    /// `vct config schema > vct.schema.json`.
    Schema,
}
