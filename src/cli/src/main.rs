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

mod cli;

use crate::cli::{Cli, Commands, ConfigAction, QuotaProvider, resolve_time_range_with_default};
use anyhow::{Context, Result, bail};
use clap::Parser;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::{self, Write};
use std::sync::Arc;

// mimalloc is opt-in behind the `mimalloc` cargo feature. The default build
// uses the system allocator because mimalloc's lazy purge retains freed
// pages — the RSS difference on the TUI loops (repeated parse of session
// directories) was roughly 10× in favour of the system allocator. Users who
// want mimalloc's speed for short one-shot runs can rebuild with
// `--features mimalloc`.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use vct_tui::display::usage::{
    display_usage_interactive_with_pool, display_usage_table, display_usage_text,
};
use vibe_coding_tracker::get_version_info;
use vibe_coding_tracker::scan::build_scan_pool;
use vibe_coding_tracker::session::{ParseMode, parse_session_file_with_diagnostics};
use vibe_coding_tracker::usage::scan_usage_priced;

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
    // Terminal-restore-on-panic is a presentation concern owned by the display
    // layer, so the binary installs it here rather than from core `logging`.
    // Installed early so a panic before the TUI starts still restores cleanly.
    vct_tui::display::common::tui::ensure_terminal_panic_hook();

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
                    let (analysis, diagnostics) =
                        parse_session_file_with_diagnostics(&file_path, mode)?;
                    if diagnostics.skipped_records() > 0 {
                        eprintln!(
                            "Warning: Skipped {} malformed or unsupported analyzer records while parsing {}. Successful results are still shown.",
                            diagnostics.skipped_records(),
                            file_path.display()
                        );
                    }
                    if complete_json {
                        write_pretty_json(&analysis)?;
                    } else if text {
                        let projected =
                            vibe_coding_tracker::analysis::project_code_analysis(&analysis);
                        vct_tui::display::analysis::display_analysis_text(&projected);
                    } else {
                        let projected =
                            vibe_coding_tracker::analysis::project_code_analysis(&analysis);
                        vct_tui::display::analysis::display_analysis_table(&projected);
                    }
                }
                None => {
                    // Settings are only needed for the batch (all-sessions) path,
                    // so `analysis FILE`, `version`, `quota`, etc. never read or
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
                            vct_tui::display::analysis::display_analysis_text(&aggregation.data);
                        } else {
                            vct_tui::display::analysis::display_analysis_table(&aggregation.data);
                        }
                    } else {
                        vct_tui::display::analysis::display_analysis_interactive_loading_with_pool(
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

            if json {
                let scan = scan_usage_priced(time_range, config.providers, &scan_pool)?;
                if let Some(error) = &scan.pricing_error {
                    eprintln!(
                        "Warning: Failed to fetch pricing data: {error}. Costs will be unavailable."
                    );
                }
                report_usage_collection(&scan.collection.diagnostics)?;
                let priced = vibe_coding_tracker::usage::price_usage_data(
                    &scan.collection.data,
                    &scan.pricing,
                );
                write_pretty_json(&priced)?;
            } else if text {
                let scan = scan_usage_priced(time_range, config.providers, &scan_pool)?;
                report_usage_collection(&scan.collection.diagnostics)?;
                display_usage_text(&scan.collection.data, merge);
            } else if table {
                let scan = scan_usage_priced(time_range, config.providers, &scan_pool)?;
                report_usage_collection(&scan.collection.diagnostics)?;
                display_usage_table(&scan.collection.data, merge);
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

        Commands::Quota {
            provider,
            text,
            table,
            ..
        } => {
            run_quota(provider, text, table)?;
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

/// Runs `vct quota <provider>`: fetch the raw body and render it.
///
/// `text` / `table` pick the output format; neither set means pretty JSON.
/// Dispatching the clap [`QuotaProvider`] onto core's per-provider raw fetchers
/// is CLI glue, so it lives here rather than in the (display-free) core `quota`
/// module.
///
/// # Errors
///
/// Returns an error if credentials are missing, the request fails, or the API
/// answers a non-2xx status (the body is still printed first; a 401/403 appends
/// the provider's login hint).
fn run_quota(provider: QuotaProvider, text: bool, table: bool) -> Result<()> {
    use vct_tui::display::quota::{display_quota_table, display_quota_text, print_quota_json};
    use vibe_coding_tracker::quota::{
        CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, claude,
        copilot, cursor, http, wham,
    };

    let client = http::build_client()?;
    let (status, body) = match provider {
        QuotaProvider::Claude => claude::fetch_claude_raw(&client),
        QuotaProvider::Codex => wham::fetch_codex_raw(&client),
        QuotaProvider::Copilot => copilot::fetch_copilot_raw(&client),
        QuotaProvider::Cursor => cursor::fetch_cursor_raw(&client),
    }?;

    if text {
        display_quota_text(&body);
    } else if table {
        display_quota_table(&body);
    } else {
        print_quota_json(&body);
    }

    if !(200..300).contains(&status) {
        let (name, hint) = match provider {
            QuotaProvider::Claude => ("claude", CLAUDE_LOGIN_HINT),
            QuotaProvider::Codex => ("codex", CODEX_LOGIN_HINT),
            QuotaProvider::Copilot => ("copilot", COPILOT_LOGIN_HINT),
            QuotaProvider::Cursor => ("cursor", CURSOR_LOGIN_HINT),
        };
        // A rejected token (401/403) is the one case a re-login fixes, so only
        // then append the login hint; other statuses (429, 5xx, ...) just report.
        if status == 401 || status == 403 {
            bail!("HTTP {status} from {name} ({hint})");
        }
        bail!("HTTP {status} from {name}");
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
    diagnostics: &vibe_coding_tracker::analysis::ScanDiagnostics,
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
    diagnostics: &vibe_coding_tracker::usage::ScanDiagnostics,
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
