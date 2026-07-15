//! Binary entry point for the `vibe_coding_tracker` (`vct`) CLI.
//!
//! Parses the subcommand with clap and dispatches to the library crate:
//! batch `analysis` and `usage` views (TUI / table / text / JSON with a
//! time-range filter), complete single-file analysis, plus `version` and
//! `update`. The heavy lifting lives in [`vibe_coding_tracker`]; this file is
//! wiring.
//!
//! Two things run *before* clap on purpose, and the ordering is
//! load-bearing — see [`main`].

use anyhow::{Context, Result, bail};
use clap::Parser;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use vibe_coding_tracker::analysis::aggregator::project_session_file;
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
    display_usage_interactive_with_pool, display_usage_table, display_usage_text,
};
use vibe_coding_tracker::get_version_info;
use vibe_coding_tracker::pricing::{ModelPricingMap, fetch_model_pricing};
use vibe_coding_tracker::session::{ParseMode, parse_session_file_typed_with_mode_and_diagnostics};
use vibe_coding_tracker::summary_cache::build_scan_pool;
use vibe_coding_tracker::usage::{
    CostSource, get_usage_from_directories_with_diagnostics, resolve_model_cost,
};
use vibe_coding_tracker::utils::extract_token_counts;

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
/// JSON (de)serialization, terminal/TUI errors, or the network and
/// binary-replacement errors raised by `update`. Pricing
/// fetch failure in `usage --json` is downgraded to a warning rather than an
/// error, so costs are reported as unavailable instead of aborting.
fn main() -> Result<()> {
    // Cap per-thread glibc arenas and pin the trim threshold before any
    // allocation happens under a Rayon worker. See `tune_system_allocator`
    // for why this matters on long TUI sessions.
    vibe_coding_tracker::utils::tune_system_allocator();

    // Install the file logger (default level `warn`) before anything can log or
    // panic. It writes only to `~/.vct/logs/`, never the terminal.
    vibe_coding_tracker::logging::init();

    if matches!(
        std::env::args_os().nth(1).and_then(|arg| arg.into_string().ok()),
        Some(arg) if arg == "--version" || arg == "-V"
    ) {
        println!("{}", vibe_coding_tracker::VERSION);
        return Ok(());
    }

    let result = run();
    if let Err(error) = &result {
        // Record the final error before anyhow prints it to stderr and the
        // process exits — the log file is the durable record of the failure.
        log::error!("command failed: {error:#}");
    }
    result
}

/// Parses the CLI and dispatches the selected subcommand.
fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Analysis {
            file,
            json,
            text,
            table,
            daily,
            weekly,
            monthly,
            all,
        } => {
            match file {
                Some(file_path) => {
                    let complete_json = json || (!text && !table);
                    let mode = if complete_json {
                        ParseMode::Full
                    } else {
                        ParseMode::UsageOnly
                    };
                    if complete_json {
                        let (analysis, diagnostics) =
                            parse_session_file_typed_with_mode_and_diagnostics(&file_path, mode)?;
                        if diagnostics.skipped_records() > 0 {
                            eprintln!(
                                "Warning: Skipped {} malformed or unsupported analyzer records while parsing {}. Successful results are still shown.",
                                diagnostics.skipped_records(),
                                file_path.display()
                            );
                        }
                        write_pretty_json(&analysis)?;
                    } else {
                        let (_, projected, diagnostics) = project_session_file(&file_path, mode)?;
                        if diagnostics.skipped_records() > 0 {
                            eprintln!(
                                "Warning: Skipped {} malformed or unsupported analyzer records while parsing {}. Successful results are still shown.",
                                diagnostics.skipped_records(),
                                file_path.display()
                            );
                        }
                        if text {
                            vibe_coding_tracker::display::analysis::display_analysis_text(
                                &projected,
                            );
                        } else {
                            vibe_coding_tracker::display::analysis::display_analysis_table(
                                &projected,
                            );
                        }
                    }
                }
                None => {
                    // Settings are only needed for the batch (all-sessions) path,
                    // so `analysis FILE`, `version`, `fetch`, etc. never read or
                    // create `~/.vct/config.toml`.
                    let config = vibe_coding_tracker::config::load();
                    vibe_coding_tracker::logging::apply(&config.logging);
                    let time_range = resolve_time_range_with_default(
                        daily,
                        weekly,
                        monthly,
                        all,
                        config.general.default_time_range,
                    );
                    let scan_pool =
                        Arc::new(build_scan_pool(config.performance.resolved_scan_threads())?);
                    if json {
                        let dataset = scan_pool.install(|| {
                            vibe_coding_tracker::analysis::collect_analysis_sessions_with(
                                time_range,
                                config.providers,
                                ParseMode::Full,
                            )
                        })?;
                        report_analysis_collection(&dataset.diagnostics)?;
                        write_pretty_json(&dataset)?;
                    } else if text || table {
                        let aggregation = scan_pool.install(|| {
                            vibe_coding_tracker::analysis::aggregate_sessions_by_model_with_diagnostics(
                                time_range,
                                config.providers,
                            )
                        })?;
                        report_analysis_collection(&aggregation.diagnostics)?;

                        if text {
                            vibe_coding_tracker::display::analysis::display_analysis_text(
                                &aggregation.data,
                            );
                        } else {
                            vibe_coding_tracker::display::analysis::display_analysis_table(
                                &aggregation.data,
                            );
                        }
                    } else {
                        vibe_coding_tracker::display::analysis::display_analysis_interactive_loading_with_pool(
                            time_range,
                            config.providers,
                            config.analysis.refresh_secs(),
                            scan_pool,
                        )?;
                    }
                }
            }
        }

        Commands::Usage {
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
            vibe_coding_tracker::logging::apply(&config.logging);
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
            let scan_pool = Arc::new(build_scan_pool(config.performance.resolved_scan_threads())?);
            let usage_from_dirs = |tr| {
                scan_pool
                    .install(|| get_usage_from_directories_with_diagnostics(tr, config.providers))
            };

            if json {
                let usage = usage_from_dirs(time_range)?;
                report_usage_collection(&usage.diagnostics)?;
                let pricing_map = match fetch_model_pricing() {
                    Ok(map) => map,
                    Err(e) => {
                        log::warn!("failed to fetch pricing data: {e}; costs unavailable");
                        eprintln!(
                            "Warning: Failed to fetch pricing data: {}. Costs will be unavailable.",
                            e
                        );
                        ModelPricingMap::new(HashMap::new())
                    }
                };
                let enriched_data = build_enriched_json(&usage.data, &pricing_map)?;
                write_pretty_json(&enriched_data)?;
            } else if text {
                let usage = usage_from_dirs(time_range)?;
                report_usage_collection(&usage.diagnostics)?;
                display_usage_text(&usage.data, merge);
            } else if table {
                let usage = usage_from_dirs(time_range)?;
                report_usage_collection(&usage.diagnostics)?;
                display_usage_table(&usage.data, merge);
            } else {
                // `config` is not used after this, so hand the panel list off by
                // move; read both cadences first so the borrows end before the
                // partial move out of `config.usage`.
                let refresh = config.usage.refresh_secs();
                let quota_refresh = config.usage.quota_refresh_secs();
                display_usage_interactive_with_pool(
                    time_range,
                    merge,
                    config.usage.quota.panels,
                    config.providers,
                    refresh,
                    quota_refresh,
                    scan_pool,
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

/// Handles the `config` subcommand: print the path, show current settings, open
/// the file in the user's editor, or print the JSON schema.
fn run_config(action: ConfigAction) -> Result<()> {
    match action {
        // Pure stdout: schema generation needs no config file, so it must not
        // resolve (and thereby create) ~/.vct — keep it usable on a read-only home.
        ConfigAction::Schema => print!("{}", vibe_coding_tracker::config::schema_json()),
        ConfigAction::Path => {
            println!(
                "{}",
                vibe_coding_tracker::utils::get_config_path()?.display()
            );
        }
        ConfigAction::Show => {
            let path = vibe_coding_tracker::utils::get_config_path()?;
            // Ensure the file exists (first-run creation) before reading it back.
            let _ = vibe_coding_tracker::config::load();
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            println!("{}", "Vibe Coding Tracker settings".bright_cyan().bold());
            println!("{}", path.display().dimmed());
            println!();
            print!("{}", contents);
        }
        ConfigAction::Edit => {
            let path = vibe_coding_tracker::utils::get_config_path()?;
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
            // Surface an aborted / failed editor as a non-zero CLI exit so scripts
            // (and users) can tell the edit did not complete cleanly.
            if !status.success() {
                anyhow::bail!("Editor '{}' exited with {}", editor, status);
            }
        }
        ConfigAction::Migrate => {
            use vibe_coding_tracker::config::MigrationStatus;
            let path = vibe_coding_tracker::utils::get_config_path()?;
            match vibe_coding_tracker::config::migrate_config_file(&path)? {
                MigrationStatus::Created => {
                    println!("Created a new config at {}", path.display());
                }
                MigrationStatus::Migrated => {
                    println!("Migrated config to the latest format: {}", path.display());
                }
                MigrationStatus::AlreadyCurrent => {
                    println!("Config is already up to date: {}", path.display());
                }
            }
        }
    }
    Ok(())
}

/// Writes one pretty-printed JSON value to stdout followed by a newline.
fn write_pretty_json(value: &impl Serialize) -> Result<()> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    Ok(())
}

/// Rejects a completely failed noninteractive scan and reports partial data.
fn report_analysis_collection(
    diagnostics: &vibe_coding_tracker::analysis::AnalysisCollectionDiagnostics,
) -> Result<()> {
    let Some(first) = diagnostics.failures.first() else {
        return Ok(());
    };
    if diagnostics.all_failed() {
        bail!(
            "failed to parse all {} analysis sources; first failure: {} {}: {}",
            diagnostics.candidates,
            first.provider,
            first.source.display(),
            first.error
        );
    }
    if diagnostics.partially_failed() {
        eprintln!(
            "Warning: Encountered {} analysis source failures while scanning {} candidates. Successful results are still shown. First failure: {} {}: {}",
            diagnostics.failures.len(),
            diagnostics.candidates,
            first.provider,
            first.source.display(),
            first.error
        );
    }
    Ok(())
}

/// Rejects a completely failed noninteractive usage scan and reports partial data.
fn report_usage_collection(
    diagnostics: &vibe_coding_tracker::usage::UsageCollectionDiagnostics,
) -> Result<()> {
    let Some(first) = diagnostics.failures.first() else {
        return Ok(());
    };
    if diagnostics.all_failed() {
        bail!(
            "failed to read all {} usage sources; first failure: {} {}: {}",
            diagnostics.candidates,
            first.provider,
            first.source.display(),
            first.error
        );
    }
    if diagnostics.partially_failed() {
        eprintln!(
            "Warning: Encountered {} usage source failures while scanning {} candidates. Successful results are still shown. First failure: {} {}: {}",
            diagnostics.failures.len(),
            diagnostics.candidates,
            first.provider,
            first.source.display(),
            first.error
        );
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
/// Rows are sorted by ascending cost, then model name, matching the other
/// usage renderers and keeping scripted output deterministic.
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

    enriched_data.sort_by(|left, right| {
        left["cost_usd"]
            .as_f64()
            .unwrap_or_default()
            .partial_cmp(&right["cost_usd"].as_f64().unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left["model"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["model"].as_str().unwrap_or_default())
            })
    });

    Ok(enriched_data)
}

/// Resolves cost for one merged JSON row from provider-scoped usage pieces.
fn resolve_enriched_model_cost(
    model: &str,
    usage_data: &vibe_coding_tracker::UsageData,
    pricing_map: &ModelPricingMap,
) -> Option<(f64, Option<String>)> {
    usage_data.price_merged_model(model, pricing_map)
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
    fn json_rows_include_grok_source_cost() {
        clear_pricing_cache();
        let mut raw_pricing = HashMap::new();
        raw_pricing.insert(
            "shared-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);
        let mut models = UsageResult::default();
        models.insert("shared-model".to_string(), json!({"input_tokens": 200}));
        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));
        per_provider
            .grok
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));
        let usage_data = vibe_coding_tracker::UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays::default(),
            stored_costs: vibe_coding_tracker::usage::StoredCosts::default(),
            pricing_ledger: vibe_coding_tracker::usage::UsagePricingLedger::default(),
        };

        let rows = build_enriched_json(&usage_data, &pricing_map).unwrap();

        assert!((rows[0]["cost_usd"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    }

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
        models.insert("shared-20250715".to_string(), json!({"input_tokens": 200}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-20250715".to_string(), json!({"input_tokens": 100}));
        per_provider
            .opencode
            .insert("shared-20250715".to_string(), json!({"input_tokens": 100}));

        let mut stored_costs = vibe_coding_tracker::usage::StoredCosts::default();
        stored_costs
            .opencode
            .insert("shared-20250715".to_string(), 7.0);

        let usage_data = vibe_coding_tracker::UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays::default(),
            stored_costs,
            pricing_ledger: vibe_coding_tracker::usage::UsagePricingLedger::default(),
        };

        let rows = build_enriched_json(&usage_data, &pricing_map).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["cost_usd"].as_f64().unwrap(), 8.0);
        assert_eq!(rows[0]["matched_model"].as_str().unwrap(), "shared");
    }

    #[test]
    fn json_rows_are_sorted_by_cost_then_model() {
        clear_pricing_cache();
        let mut raw_pricing = HashMap::new();
        for model in ["model-a", "model-b", "model-c"] {
            raw_pricing.insert(
                model.to_string(),
                ModelPricing {
                    input_cost_per_token: 0.01,
                    ..Default::default()
                },
            );
        }
        let pricing_map = ModelPricingMap::new(raw_pricing);
        let mut models = UsageResult::default();
        models.insert("model-c".to_string(), json!({"input_tokens": 200}));
        models.insert("model-b".to_string(), json!({"input_tokens": 100}));
        models.insert("model-a".to_string(), json!({"input_tokens": 100}));
        let usage_data = vibe_coding_tracker::UsageData {
            models,
            per_provider: PerProviderUsage::default(),
            provider_days: ProviderActiveDays::default(),
            stored_costs: vibe_coding_tracker::usage::StoredCosts::default(),
            pricing_ledger: vibe_coding_tracker::usage::UsagePricingLedger::default(),
        };

        let rows = build_enriched_json(&usage_data, &pricing_map).unwrap();
        let models: Vec<_> = rows
            .iter()
            .map(|row| row["model"].as_str().unwrap())
            .collect();

        assert_eq!(models, ["model-a", "model-b", "model-c"]);
    }
}
