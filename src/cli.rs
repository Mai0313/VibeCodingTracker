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
    /// Analyze a JSONL conversation file
    Analysis {
        /// Path to the JSONL file to analyze
        #[arg(short, long)]
        path: PathBuf,

        /// Optional output path to save analysis result as JSON
        #[arg(short, long)]
        output: Option<PathBuf>,
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
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
