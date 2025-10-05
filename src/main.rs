use anyhow::Result;
use clap::Parser;
use comfy_table::{presets::UTF8_FULL, Cell, CellAlignment, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use std::collections::HashMap;
use vibe_coding_tracker::cli::{Cli, Commands};
use vibe_coding_tracker::pricing::{
    calculate_cost, fetch_model_pricing, get_model_pricing, ModelPricing,
};
use vibe_coding_tracker::usage::{
    display_usage_interactive, display_usage_table, display_usage_text, get_usage_from_directories,
};
use vibe_coding_tracker::utils::extract_token_counts;
use vibe_coding_tracker::{analyze_jsonl_file, get_version_info, DateUsageResult};

fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Analysis { path, output } => {
            match path {
                Some(file_path) => {
                    // Single file analysis
                    let result = analyze_jsonl_file(&file_path)?;

                    // Save to output file if specified
                    if let Some(output_path) = output {
                        vibe_coding_tracker::utils::save_json_pretty(&output_path, &result)?;
                        println!("✅ Analysis result saved to: {}", output_path.display());
                    } else {
                        // Print to stdout if no output file specified
                        let json_str = serde_json::to_string_pretty(&result)?;
                        println!("{}", json_str);
                    }
                }
                None => {
                    // Batch analysis of all sessions
                    let analysis_data = vibe_coding_tracker::analysis::analyze_all_sessions()?;

                    if let Some(output_path) = output {
                        // Save to JSON file
                        let json_value = serde_json::to_value(&analysis_data)?;
                        vibe_coding_tracker::utils::save_json_pretty(&output_path, &json_value)?;
                        println!("✅ Analysis result saved to: {}", output_path.display());
                    } else {
                        // Display interactive table
                        vibe_coding_tracker::analysis::display_analysis_interactive(
                            &analysis_data,
                        )?;
                    }
                }
            }
        }

        Commands::Usage { json, text, table } => {
            if json {
                let usage_data = get_usage_from_directories()?;
                let pricing_map = match fetch_model_pricing() {
                    Ok(map) => map,
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to fetch pricing data: {}. Costs will be unavailable.",
                            e
                        );
                        HashMap::new()
                    }
                };
                let enriched_data = build_enriched_json(&usage_data, &pricing_map)?;
                let json_str = serde_json::to_string_pretty(&enriched_data)?;
                println!("{}", json_str);
            } else if text {
                let usage_data = get_usage_from_directories()?;
                display_usage_text(&usage_data);
            } else if table {
                let usage_data = get_usage_from_directories()?;
                display_usage_table(&usage_data);
            } else {
                // Default: Display interactive table
                display_usage_interactive()?;
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
                println!("{}", "🚀 Vibe Coding Tracker".bright_cyan().bold());
                println!();

                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL)
                    .set_content_arrangement(ContentArrangement::Dynamic)
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

        Commands::Update { check, force } => {
            if check {
                // Only check for updates without installing
                vibe_coding_tracker::update::check_update()?;
            } else {
                // Perform update with optional force flag
                vibe_coding_tracker::update::update_interactive(force)?;
            }
        }
    }

    Ok(())
}

/// Build enriched JSON output with costs for usage data
fn build_enriched_json(
    usage_data: &DateUsageResult,
    pricing_map: &HashMap<String, ModelPricing>,
) -> Result<HashMap<String, Vec<Value>>> {
    let mut enriched_data: HashMap<String, Vec<Value>> = HashMap::new();

    for (date, models) in usage_data {
        let mut date_entries = Vec::new();

        for (model, usage) in models {
            let mut entry = json!({
                "model": model,
                "usage": usage
            });

            // Extract token counts and calculate cost
            let counts = extract_token_counts(usage);
            let pricing_result = get_model_pricing(model, pricing_map);
            let cost = calculate_cost(
                counts.input_tokens,
                counts.output_tokens,
                counts.cache_read,
                counts.cache_creation,
                &pricing_result.pricing,
            );

            if let Some(entry_obj) = entry.as_object_mut() {
                entry_obj.insert("cost_usd".to_string(), json!(cost));
                if let Some(matched) = &pricing_result.matched_model {
                    entry_obj.insert("matched_model".to_string(), json!(matched));
                }
            }

            date_entries.push(entry);
        }

        enriched_data.insert(date.clone(), date_entries);
    }

    Ok(enriched_data)
}
