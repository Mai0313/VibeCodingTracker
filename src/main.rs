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
        Commands::Analysis { path, output, all } => {
            if all {
                // Handle --all flag: group by provider and output as JSON
                let grouped_data =
                    vibe_coding_tracker::analysis::analyze_all_sessions_by_provider()?;

                if let Some(output_path) = output {
                    let json_value = serde_json::to_value(&grouped_data)?;
                    vibe_coding_tracker::utils::save_json_pretty(&output_path, &json_value)?;
                    println!("✅ Analysis result saved to: {}", output_path.display());
                } else {
                    // Output as JSON by default
                    let json_str = serde_json::to_string_pretty(&grouped_data)?;
                    println!("{}", json_str);
                }
            } else {
                match path {
                    Some(file_path) => {
                        let result = analyze_jsonl_file(&file_path)?;

                        if let Some(output_path) = output {
                            vibe_coding_tracker::utils::save_json_pretty(&output_path, &result)?;
                            println!("✅ Analysis result saved to: {}", output_path.display());
                        } else {
                            let json_str = serde_json::to_string_pretty(&result)?;
                            println!("{}", json_str);
                        }
                    }
                    None => {
                        let analysis_data = vibe_coding_tracker::analysis::analyze_all_sessions()?;

                        if let Some(output_path) = output {
                            let json_value = serde_json::to_value(&analysis_data)?;
                            vibe_coding_tracker::utils::save_json_pretty(
                                &output_path,
                                &json_value,
                            )?;
                            println!("✅ Analysis result saved to: {}", output_path.display());
                        } else {
                            vibe_coding_tracker::analysis::display_analysis_interactive(
                                &analysis_data,
                            )?;
                        }
                    }
                }
            }
        }

        Commands::Usage { json, text, table } => {
            if json || text || table {
                let usage_data = get_usage_from_directories()?;

                if json {
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
                    display_usage_text(&usage_data);
                } else {
                    display_usage_table(&usage_data);
                }
            } else {
                display_usage_interactive()?;
            }
        }

        Commands::Version { json, text } => {
            let version_info = get_version_info();

            if json {
                let json_output = serde_json::json!({
                    "Version": version_info.version,
                    "Rust Version": version_info.rust_version,
                    "Cargo Version": version_info.cargo_version
                });
                println!("{}", serde_json::to_string_pretty(&json_output)?);
            } else if text {
                println!("Version: {}", version_info.version);
                println!("Rust Version: {}", version_info.rust_version);
                println!("Cargo Version: {}", version_info.cargo_version);
            } else {
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
                vibe_coding_tracker::update::check_update()?;
            } else {
                vibe_coding_tracker::update::update_interactive(force)?;
            }
        }
    }

    Ok(())
}

fn build_enriched_json(
    usage_data: &DateUsageResult,
    pricing_map: &HashMap<String, ModelPricing>,
) -> Result<HashMap<String, Vec<Value>>> {
    let enriched_data = usage_data
        .iter()
        .map(|(date, models)| {
            let date_entries = models
                .iter()
                .map(|(model, usage)| {
                    let counts = extract_token_counts(usage);
                    let pricing_result = get_model_pricing(model, pricing_map);
                    let cost = calculate_cost(
                        counts.input_tokens,
                        counts.output_tokens,
                        counts.cache_read,
                        counts.cache_creation,
                        &pricing_result.pricing,
                    );

                    let mut entry = json!({
                        "model": model,
                        "usage": usage,
                        "cost_usd": cost
                    });

                    if let Some(matched) = &pricing_result.matched_model {
                        entry["matched_model"] = json!(matched);
                    }

                    entry
                })
                .collect();

            (date.clone(), date_entries)
        })
        .collect();

    Ok(enriched_data)
}
