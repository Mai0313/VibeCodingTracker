use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Vibe Coding Tracker - AI coding assistant usage analyzer
#[derive(Parser, Debug)]
#[command(name = "vibe_coding_tracker")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Analyze JSONL conversation files (single file or all sessions)
    Analysis {
        /// Path to the JSONL file to analyze (if not provided, analyzes all sessions)
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Optional output path to save analysis result as JSON
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Group results by provider (claude/codex/gemini)
        #[arg(long)]
        all: bool,
    },

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
