use anyhow::Result;
use clap::Parser;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use serde_json::{Value, json};
use std::collections::HashMap;
use vibe_coding_tracker::cli::{Cli, Commands, resolve_time_range};

// mimalloc is opt-in behind the `mimalloc` cargo feature. The default build
// uses the system allocator because mimalloc's lazy purge retains freed
// pages — the RSS difference on the TUI loops (repeated parse of session
// directories) was roughly 10× in favour of the system allocator. Users who
// want mimalloc's speed for short one-shot runs can rebuild with
// `--features mimalloc`.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use vibe_coding_tracker::display::usage::{
    display_usage_interactive, display_usage_table, display_usage_text,
};
use vibe_coding_tracker::models::UsageResult;
use vibe_coding_tracker::pricing::{ModelPricingMap, calculate_cost, fetch_model_pricing};
use vibe_coding_tracker::usage::get_usage_from_directories;
use vibe_coding_tracker::utils::extract_token_counts;
use vibe_coding_tracker::{get_version_info, parse_session_file};

fn main() -> Result<()> {
    // Cap per-thread glibc arenas and pin the trim threshold before any
    // allocation happens under a Rayon worker. See `tune_system_allocator`
    // for why this matters on long TUI sessions.
    vibe_coding_tracker::utils::tune_system_allocator();

    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Analysis {
            path,
            output,
            by_provider,
            table,
            daily,
            weekly,
            monthly,
            ..
        } => {
            let time_range = resolve_time_range(daily, weekly, monthly);

            if by_provider {
                // Handle --by-provider flag: group by provider and output as JSON
                let grouped_data =
                    vibe_coding_tracker::analysis::analyze_all_sessions_by_provider(time_range)?;

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
                        let result = parse_session_file(&file_path)?;

                        if let Some(output_path) = output {
                            vibe_coding_tracker::utils::save_json_pretty(&output_path, &result)?;
                            println!("✅ Analysis result saved to: {}", output_path.display());
                        } else {
                            let json_str = serde_json::to_string_pretty(&result)?;
                            println!("{}", json_str);
                        }
                    }
                    None => {
                        let analysis_data =
                            vibe_coding_tracker::analysis::analyze_all_sessions(time_range)?;

                        if let Some(output_path) = output {
                            let json_value = serde_json::to_value(&analysis_data.rows)?;
                            vibe_coding_tracker::utils::save_json_pretty(
                                &output_path,
                                &json_value,
                            )?;
                            println!("✅ Analysis result saved to: {}", output_path.display());
                        } else if table {
                            vibe_coding_tracker::display::analysis::display_analysis_table(
                                &analysis_data,
                            );
                        } else {
                            vibe_coding_tracker::display::analysis::display_analysis_interactive(
                                &analysis_data,
                                time_range,
                            )?;
                        }
                    }
                }
            }
        }

        Commands::Usage {
            json,
            text,
            table,
            daily,
            weekly,
            monthly,
            ..
        } => {
            let time_range = resolve_time_range(daily, weekly, monthly);

            if json || text || table {
                let usage_data = get_usage_from_directories(time_range)?;

                if json {
                    let pricing_map = match fetch_model_pricing() {
                        Ok(map) => map,
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to fetch pricing data: {}. Costs will be unavailable.",
                                e
                            );
                            ModelPricingMap::new(HashMap::new())
                        }
                    };
                    let enriched_data = build_enriched_json(&usage_data.models, &pricing_map)?;
                    let json_str = serde_json::to_string_pretty(&enriched_data)?;
                    println!("{}", json_str);
                } else if text {
                    display_usage_text(&usage_data);
                } else {
                    display_usage_table(&usage_data);
                }
            } else {
                display_usage_interactive(time_range)?;
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
    usage_data: &UsageResult,
    pricing_map: &ModelPricingMap,
) -> Result<Vec<Value>> {
    let mut enriched_data = Vec::with_capacity(usage_data.len());

    for (model, usage) in usage_data.iter() {
        let counts = extract_token_counts(usage);

        let pricing_result = pricing_map.get(model);

        let cost = calculate_cost(
            counts.input_tokens,
            counts.output_tokens,
            counts.reasoning_tokens,
            counts.cache_read,
            counts.cache_creation_5m,
            counts.cache_creation_1h,
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

        enriched_data.push(entry);
    }

    Ok(enriched_data)
}
