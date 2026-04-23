use clap::{Parser, Subcommand};

/// Time range filter for data queries
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum TimeRange {
    Daily,
    Weekly,
    Monthly,
    #[default]
    All,
}

impl TimeRange {
    /// Returns the cutoff date (inclusive) for filtering, or None for All
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

/// Resolve time range from CLI flags (default: All)
pub fn resolve_time_range(daily: bool, weekly: bool, monthly: bool) -> TimeRange {
    if daily {
        TimeRange::Daily
    } else if weekly {
        TimeRange::Weekly
    } else if monthly {
        TimeRange::Monthly
    } else {
        TimeRange::All
    }
}

/// Vibe Coding Tracker - AI coding assistant token usage tracker
#[derive(Parser, Debug)]
#[command(name = "vibe_coding_tracker")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Display token usage statistics
    Usage {
        /// Output raw JSON instead of table view
        #[arg(long)]
        json: bool,

        /// Output as plain text
        #[arg(long)]
        text: bool,

        /// Output as static table
        #[arg(long)]
        table: bool,

        /// Show only today's data
        #[arg(long, group = "period")]
        daily: bool,

        /// Show only this week's data
        #[arg(long, group = "period")]
        weekly: bool,

        /// Show only this month's data
        #[arg(long, group = "period")]
        monthly: bool,

        /// Show all data (default)
        #[arg(long, group = "period")]
        all: bool,
    },

    /// Display version information
    Version {
        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Output as plain text
        #[arg(long)]
        text: bool,
    },

    /// Update to the latest version from GitHub releases
    Update {
        /// Check for updates without installing
        #[arg(long)]
        check: bool,

        /// Force update without confirmation prompt
        #[arg(long, short)]
        force: bool,
    },
}
