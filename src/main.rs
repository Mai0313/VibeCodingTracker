use anyhow::Result;
use codex_usage::cli::{Cli, Commands};
use codex_usage::pricing::{calculate_cost, fetch_model_pricing, get_model_pricing};
use codex_usage::usage::{display_usage_interactive, display_usage_table, display_usage_text, get_usage_from_directories};
use codex_usage::{analyze_jsonl_file, get_version_info};
use comfy_table::{presets::UTF8_FULL, Cell, CellAlignment, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;
use serde_json::json;
use std::collections::HashMap;

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

        Commands::Usage { json, text, table } => {
            if json {
                let usage_data = get_usage_from_directories()?;
                // Fetch pricing data for JSON output with costs
                let pricing_map = fetch_model_pricing().unwrap_or_default();

                // Build enriched JSON output with costs
                let mut enriched_data: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

                for (date, models) in &usage_data {
                    let mut date_entries = Vec::new();

                    for (model, usage) in models {
                        let mut entry = json!({
                            "model": model,
                            "usage": usage
                        });

                        // Calculate cost
                        if let Some(usage_obj) = usage.as_object() {
                            let mut input_tokens = 0i64;
                            let mut output_tokens = 0i64;
                            let mut cache_read = 0i64;
                            let mut cache_creation = 0i64;

                            // Claude usage
                            if let Some(input) =
                                usage_obj.get("input_tokens").and_then(|v| v.as_i64())
                            {
                                input_tokens = input;
                            }
                            if let Some(output) =
                                usage_obj.get("output_tokens").and_then(|v| v.as_i64())
                            {
                                output_tokens = output;
                            }
                            if let Some(cr) = usage_obj
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_i64())
                            {
                                cache_read = cr;
                            }
                            if let Some(cc) = usage_obj
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_i64())
                            {
                                cache_creation = cc;
                            }

                            // Codex usage
                            if let Some(total_usage) = usage_obj
                                .get("total_token_usage")
                                .and_then(|v| v.as_object())
                            {
                                if let Some(input) =
                                    total_usage.get("input_tokens").and_then(|v| v.as_i64())
                                {
                                    input_tokens = input;
                                }
                                if let Some(output) =
                                    total_usage.get("output_tokens").and_then(|v| v.as_i64())
                                {
                                    output_tokens = output;
                                }
                                if let Some(reasoning) = total_usage
                                    .get("reasoning_output_tokens")
                                    .and_then(|v| v.as_i64())
                                {
                                    output_tokens += reasoning;
                                }
                                if let Some(cr) = total_usage
                                    .get("cached_input_tokens")
                                    .and_then(|v| v.as_i64())
                                {
                                    cache_read = cr;
                                }
                            }

                            let pricing_result = get_model_pricing(model, &pricing_map);
                            let cost = calculate_cost(
                                input_tokens,
                                output_tokens,
                                cache_read,
                                cache_creation,
                                &pricing_result.pricing,
                            );

                            if let Some(entry_obj) = entry.as_object_mut() {
                                entry_obj.insert("cost_usd".to_string(), json!(cost));
                                if let Some(matched) = &pricing_result.matched_model {
                                    entry_obj.insert("matched_model".to_string(), json!(matched));
                                }
                            }
                        }

                        date_entries.push(entry);
                    }

                    enriched_data.insert(date.clone(), date_entries);
                }

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
                println!("{}", "🚀 Codex Usage Analyzer".bright_cyan().bold());
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
    }

    Ok(())
}
