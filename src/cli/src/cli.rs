//! clap command-line surface and the time-range filter it resolves to.
//!
//! The `///` comments on [`Commands`] and their arguments are what clap
//! renders as `--help` text, so they read as user-facing prose. [`Cli`] is
//! the parsed top-level structure; [`resolve_time_range`] collapses the
//! mutually-exclusive `--daily` / `--weekly` / `--monthly` / `--all` flags
//! into a single [`TimeRange`].

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

// `TimeRange` and the period-flag resolvers are core domain logic (no clap
// types), so they live in `models::filter`; re-exported here for the clap layer
// and library callers that reach them through `cli`.
pub use vibe_coding_tracker::models::resolve_time_range_with_default;

/// A provider whose raw quota/usage API response `vct quota` can print.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum QuotaProvider {
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
#[command(author, version = vibe_coding_tracker::VERSION, about, long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands exposed by the CLI.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Analyze local session data (single file or all sessions).
    Analysis {
        /// JSONL or JSON session file to analyze; prints complete JSON by default.
        #[arg(
            value_name = "FILE",
            conflicts_with_all = ["daily", "weekly", "monthly", "all"]
        )]
        file: Option<PathBuf>,

        /// Output complete analysis data as JSON.
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
        /// Output usage data as JSON.
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
    ///
    /// The old name `fetch` is kept as a hidden alias for back-compat.
    #[command(alias = "fetch")]
    Quota {
        /// Which provider to query (claude | codex | copilot | cursor).
        provider: QuotaProvider,

        /// Output as pretty JSON (default).
        #[arg(long, group = "quota_format")]
        json: bool,

        /// Output as flattened plain text.
        #[arg(long, group = "quota_format")]
        text: bool,

        /// Output as a flattened key/value table.
        #[arg(long, group = "quota_format")]
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
    /// Rewrite a legacy-format config to the current on-disk layout in place.
    ///
    /// Loading the settings already migrates the file automatically; this forces
    /// the same pass so a schema-aware editor sees the upgraded file right away.
    Migrate,
}
