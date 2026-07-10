//! Binary entry point for the `vibe_coding_tracker` (`vct`) CLI.
//!
//! Parses the subcommand with clap and dispatches to the library crate:
//! `analysis` and `usage` (each with TUI / table / text / JSON output and
//! a time-range filter), plus `version` and `update`. The heavy lifting
//! lives in [`vibe_coding_tracker`]; this file is wiring.
//!
//! Two things run *before* clap on purpose, and the ordering is
//! load-bearing — see [`main`].

use anyhow::{Context, Result};
use clap::Parser;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use serde_json::{Value, json};
use std::collections::HashMap;
use vibe_coding_tracker::cli::{Cli, Commands, ConfigAction, resolve_time_range_with_default};

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
use vibe_coding_tracker::pricing::{ModelPricingMap, fetch_model_pricing};
use vibe_coding_tracker::usage::{CostSource, get_usage_from_directories_with, resolve_model_cost};
use vibe_coding_tracker::utils::extract_token_counts;
use vibe_coding_tracker::{get_version_info, parse_session_file};

/// Parses the CLI and runs the selected subcommand.
///
/// Two steps run before `Cli::parse()` and must stay in this order:
/// `tune_system_allocator()` caps glibc arenas before the first allocation
/// (a Rayon worker), and the `--version` / `-V` branch is handled by hand so
/// the bare flag prints just [`vibe_coding_tracker::VERSION`] without going
/// through clap (the `version` *subcommand* still renders the full table).
///
/// # Errors
///
/// Propagates any failure from the dispatched subcommand: session parsing,
/// JSON (de)serialization, file writes for `--output`, terminal/TUI errors,
/// or the network and binary-replacement errors raised by `update`. Pricing
/// fetch failure in `usage --json` is downgraded to a warning rather than an
/// error, so costs are reported as unavailable instead of aborting.
fn main() -> Result<()> {
    // Cap per-thread glibc arenas and pin the trim threshold before any
    // allocation happens under a Rayon worker. See `tune_system_allocator`
    // for why this matters on long TUI sessions.
    vibe_coding_tracker::utils::tune_system_allocator();

    env_logger::init();

    if matches!(
        std::env::args_os().nth(1).and_then(|arg| arg.into_string().ok()),
        Some(arg) if arg == "--version" || arg == "-V"
    ) {
        println!("{}", vibe_coding_tracker::VERSION);
        return Ok(());
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Analysis {
            path,
            output,
            json,
            text,
            table,
            daily,
            weekly,
            monthly,
            all,
        } => {
            match path {
                Some(file_path) => {
                    let result = parse_session_file(&file_path)?;

                    if let Some(output_path) = output {
                        vibe_coding_tracker::utils::save_json_pretty(&output_path, &result)?;
                        println!("Analysis result saved to: {}", output_path.display());
                    } else {
                        let json_str = serde_json::to_string_pretty(&result)?;
                        println!("{}", json_str);
                    }
                }
                None => {
                    // Settings are only needed for the batch (all-sessions) path,
                    // so `analysis --path`, `version`, `fetch`, etc. never read or
                    // create `~/.vct/config.toml`.
                    let config = vibe_coding_tracker::config::load();
                    let time_range = resolve_time_range_with_default(
                        daily,
                        weekly,
                        monthly,
                        all,
                        config.general.default_time_range,
                    );
                    let analysis_data =
                        vibe_coding_tracker::analysis::aggregate_sessions_by_model_with(
                            time_range,
                            config.providers,
                        )?;

                    if let Some(output_path) = output {
                        let json_value = serde_json::to_value(&analysis_data.rows)?;
                        vibe_coding_tracker::utils::save_json_pretty(&output_path, &json_value)?;
                        println!("Analysis result saved to: {}", output_path.display());
                    } else if json {
                        let json_str = serde_json::to_string_pretty(&analysis_data.rows)?;
                        println!("{}", json_str);
                    } else if text {
                        vibe_coding_tracker::display::analysis::display_analysis_text(
                            &analysis_data,
                        );
                    } else if table {
                        vibe_coding_tracker::display::analysis::display_analysis_table(
                            &analysis_data,
                        );
                    } else {
                        vibe_coding_tracker::display::analysis::display_analysis_interactive(
                            analysis_data,
                            time_range,
                            config.providers,
                            config.analysis.refresh_secs(),
                        )?;
                    }
                }
            }
        }

        Commands::Usage {
            output,
            json,
            text,
            table,
            merge_providers,
            daily,
            weekly,
            monthly,
            all,
        } => {
            let config = vibe_coding_tracker::config::load();
            let time_range = resolve_time_range_with_default(
                daily,
                weekly,
                monthly,
                all,
                config.general.default_time_range,
            );
            // A `--merge-providers` flag forces merging on; otherwise the saved
            // preference decides. The TUI's `m` toggle persists back to config.
            let merge = merge_providers || config.usage.merge_models;
            let usage_from_dirs = |tr| get_usage_from_directories_with(tr, config.providers);

            if json || output.is_some() {
                let usage_data = usage_from_dirs(time_range)?;
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
                let enriched_data = build_enriched_json(&usage_data, &pricing_map)?;

                if let Some(output_path) = output {
                    let json_value = serde_json::to_value(&enriched_data)?;
                    vibe_coding_tracker::utils::save_json_pretty(&output_path, &json_value)?;
                    println!("Usage result saved to: {}", output_path.display());
                } else {
                    let json_str = serde_json::to_string_pretty(&enriched_data)?;
                    println!("{}", json_str);
                }
            } else if text {
                let usage_data = usage_from_dirs(time_range)?;
                display_usage_text(&usage_data, merge);
            } else if table {
                let usage_data = usage_from_dirs(time_range)?;
                display_usage_table(&usage_data, merge);
            } else {
                display_usage_interactive(
                    time_range,
                    merge,
                    config.usage.quota_panels.clone(),
                    config.providers,
                    config.usage.refresh_secs(),
                )?;
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
                println!("{}", "Vibe Coding Tracker".bright_cyan().bold());
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

        Commands::Fetch {
            provider,
            text,
            table,
            ..
        } => {
            vibe_coding_tracker::fetch::run(provider, text, table)?;
        }

        Commands::Config { action } => {
            run_config(action.unwrap_or(ConfigAction::Show))?;
        }
    }

    Ok(())
}

/// Handles the `config` subcommand: print the path, show current settings, or
/// open the file in the user's editor.
fn run_config(action: ConfigAction) -> Result<()> {
    let path = vibe_coding_tracker::utils::get_config_path()?;
    match action {
        ConfigAction::Path => println!("{}", path.display()),
        ConfigAction::Show => {
            // Ensure the file exists (first-run creation) before reading it back.
            let _ = vibe_coding_tracker::config::load();
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            println!("{}", "Vibe Coding Tracker settings".bright_cyan().bold());
            println!("{}", path.display().dimmed());
            println!();
            print!("{}", contents);
        }
        ConfigAction::Edit => {
            // Ensure the file exists before handing it to the editor.
            let _ = vibe_coding_tracker::config::load();
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| default_editor().to_string());
            // `$EDITOR` / `$VISUAL` often carry arguments (`code --wait`, `vim -f`),
            // so split into program + args rather than treating the whole string as
            // one executable name.
            let mut parts = editor.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty editor command"))?;
            let status = std::process::Command::new(program)
                .args(parts)
                .arg(&path)
                .status()
                .with_context(|| format!("Failed to launch editor '{}'", editor))?;
            if !status.success() {
                eprintln!("Editor exited with status: {}", status);
            }
        }
    }
    Ok(())
}

/// The fallback editor when neither `$VISUAL` nor `$EDITOR` is set.
fn default_editor() -> &'static str {
    if cfg!(windows) { "notepad" } else { "vi" }
}

/// Builds the `usage --json` payload, joining each model's token counts with
/// its priced cost.
///
/// For every model it resolves the USD cost via
/// [`resolve_model_cost`](vibe_coding_tracker::usage::resolve_model_cost) and
/// emits a JSON object with `model`, `usage`, `cost_usd`, and (when a non-exact
/// LiteLLM key was used) `matched_model`. OpenCode models without an exact
/// price report OpenCode's own stored cost only for the OpenCode portion of a
/// merged row rather than applying it to other providers with the same model.
///
/// # Errors
///
/// Returns an error only if a usage value cannot be serialized into the
/// resulting JSON object.
fn build_enriched_json(
    usage_data: &vibe_coding_tracker::UsageData,
    pricing_map: &ModelPricingMap,
) -> Result<Vec<Value>> {
    let mut enriched_data = Vec::with_capacity(usage_data.models.len());

    for (model, usage) in usage_data.models.iter() {
        let (cost, matched_model) = resolve_enriched_model_cost(model, usage_data, pricing_map)
            .unwrap_or_else(|| price_usage_value(model, usage, pricing_map, CostSource::Litellm));

        let mut entry = json!({
            "model": model,
            "usage": usage,
            "cost_usd": cost
        });

        if let Some(matched) = &matched_model {
            entry["matched_model"] = json!(matched);
        }

        enriched_data.push(entry);
    }

    Ok(enriched_data)
}

/// Resolves cost for one merged JSON row from provider-scoped usage pieces.
fn resolve_enriched_model_cost(
    model: &str,
    usage_data: &vibe_coding_tracker::UsageData,
    pricing_map: &ModelPricingMap,
) -> Option<(f64, Option<String>)> {
    let mut total_cost = 0.0;
    let mut matched_model = None;
    let mut found = false;

    for usage in [
        &usage_data.per_provider.claude,
        &usage_data.per_provider.codex,
        &usage_data.per_provider.copilot,
        &usage_data.per_provider.gemini,
    ] {
        if let Some(raw_usage) = usage.get(model) {
            found = true;
            let (cost, matched) =
                price_usage_value(model, raw_usage, pricing_map, CostSource::Litellm);
            total_cost += cost;
            if matched_model.is_none() {
                matched_model = matched;
            }
        }
    }

    // OpenCode and Cursor both carry stored costs, but OpenCode prefers an exact
    // LiteLLM match while Cursor uses its dashboard cost verbatim. Their stored
    // costs are kept per provider so a colliding bare model name cannot
    // cross-contaminate.
    let stored = |m: &vibe_coding_tracker::constants::FastHashMap<String, f64>| {
        m.get(model).copied().unwrap_or(0.0)
    };
    for (usage, source) in [
        (
            &usage_data.per_provider.opencode,
            CostSource::OpenCodeStored(stored(&usage_data.stored_costs.opencode)),
        ),
        (
            &usage_data.per_provider.cursor,
            CostSource::CursorStored(stored(&usage_data.stored_costs.cursor)),
        ),
    ] {
        if let Some(raw_usage) = usage.get(model) {
            found = true;
            let (cost, matched) = price_usage_value(model, raw_usage, pricing_map, source);
            total_cost += cost;
            if matched_model.is_none() {
                matched_model = matched;
            }
        }
    }

    found.then_some((total_cost, matched_model))
}

/// Prices one raw usage value under `source`.
fn price_usage_value(
    model: &str,
    usage: &Value,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    let counts = extract_token_counts(usage);
    resolve_model_cost(model, &counts, pricing_map, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_coding_tracker::models::{PerProviderUsage, ProviderActiveDays, UsageResult};
    use vibe_coding_tracker::pricing::{ModelPricing, clear_pricing_cache};

    #[test]
    fn json_rows_price_opencode_fallback_only_for_opencode_tokens() {
        clear_pricing_cache();

        let mut raw_pricing = HashMap::new();
        raw_pricing.insert(
            "shared".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);

        let mut models = UsageResult::default();
        models.insert("shared-pro".to_string(), json!({"input_tokens": 200}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));
        per_provider
            .opencode
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));

        let mut stored_costs = vibe_coding_tracker::usage::StoredCosts::default();
        stored_costs.opencode.insert("shared-pro".to_string(), 7.0);

        let usage_data = vibe_coding_tracker::UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays::default(),
            stored_costs,
        };

        let rows = build_enriched_json(&usage_data, &pricing_map).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["cost_usd"].as_f64().unwrap(), 8.0);
        assert_eq!(rows[0]["matched_model"].as_str().unwrap(), "shared");
    }
}
