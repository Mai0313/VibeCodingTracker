use anyhow::Result;
use codex_usage::cli::{Cli, Commands};
use codex_usage::usage::{display_usage_table, get_usage_from_directories};
use codex_usage::{analyze_jsonl_file, get_version_info};
use comfy_table::{presets::UTF8_FULL, Cell, CellAlignment, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;

fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse_args();

    match cli.command {
        Commands::Analysis { path, output } => {
            // Analyze the JSONL file
            let result = analyze_jsonl_file(&path)?;

            // Save to output file if specified
            if let Some(output_path) = output {
                codex_usage::utils::save_json_pretty(&output_path, &result)?;
                println!("✅ Analysis result saved to: {}", output_path.display());
            } else {
                // Print to stdout if no output file specified
                let json_str = serde_json::to_string_pretty(&result)?;
                println!("{}", json_str);
            }
        }

        Commands::Usage { json } => {
            let usage_data = get_usage_from_directories()?;

            if json {
                // Output raw JSON
                let json_str = serde_json::to_string_pretty(&usage_data)?;
                println!("{}", json_str);
            } else {
                // Display table
                display_usage_table(&usage_data);
            }
        }

        Commands::Version { json, text } => {
            let version_info = get_version_info();

            if json {
                // JSON format
                let json_output = serde_json::json!({
                    "Version": version_info.version,
                    "Rust Version": version_info.rust_version,
                    "Cargo Version": version_info.cargo_version
                });
                println!("{}", serde_json::to_string_pretty(&json_output)?);
            } else if text {
                // Plain text format
                println!("Version: {}", version_info.version);
                println!("Rust Version: {}", version_info.rust_version);
                println!("Cargo Version: {}", version_info.cargo_version);
            } else {
                // Default pretty format with table
                println!("{}", "🚀 Codex Usage Analyzer".bright_cyan().bold());
                println!();

                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec![
                        Cell::new("Property")
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Left),
                        Cell::new("Value")
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Left),
                    ])
                    .add_row(vec![
                        Cell::new("Version")
                            .fg(Color::Green)
                            .set_alignment(CellAlignment::Left),
                        Cell::new(&version_info.version)
                            .fg(Color::White)
                            .set_alignment(CellAlignment::Left),
                    ])
                    .add_row(vec![
                        Cell::new("Rust Version")
                            .fg(Color::Green)
                            .set_alignment(CellAlignment::Left),
                        Cell::new(&version_info.rust_version)
                            .fg(Color::White)
                            .set_alignment(CellAlignment::Left),
                    ])
                    .add_row(vec![
                        Cell::new("Cargo Version")
                            .fg(Color::Green)
                            .set_alignment(CellAlignment::Left),
                        Cell::new(&version_info.cargo_version)
                            .fg(Color::White)
                            .set_alignment(CellAlignment::Left),
                    ]);

                println!("{table}");
            }
        }
    }

    Ok(())
}
