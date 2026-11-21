//! Binary entry point for the `vibe_coding_tracker` (`vct`) CLI.
//!
//! Parses the command line with clap and dispatches to [`vct_core`] and [`vct_tui`]: batch and single-file `analysis`, `usage`, `quota`, `ledger`, `config`, `version`, and `update`.
//!
//! Process-wide allocator tuning, file logging, panic-hook installation, and CLI version-probe opt-in run before argument parsing; see [`main`].

mod cli;

use crate::cli::{
    Cli, Commands, ConfigAction, LedgerAction, QuotaProvider, resolve_time_range_with_default,
};
use anyhow::{Result, bail};
use clap::Parser;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::{self, BufWriter, Write};
use std::sync::Arc;

/// Capacity for buffered CLI stdout writers (pretty JSON and ledger export).
const STDOUT_BUFFER_BYTES: usize = 128 * 1024;

// Opt-in only: mimalloc's lazy page purge retains roughly 10× the RSS of the system allocator across repeated TUI scan loops.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use vct_core::get_version_info;
use vct_core::scan::build_scan_pool;
use vct_core::session::{ParseMode, parse_session_file_with_diagnostics};
use vct_core::usage::pipeline::{
    scan_daily_usage_priced_with_remote_hook, scan_usage_priced_with_remote_hook,
};
use vct_tui::display::app::{AppOptions, Tab, run_app};
use vct_tui::display::usage::{display_usage_table_with_pricing, display_usage_text_with_pricing};

/// Runs process-wide setup before argument parsing, short-circuits bare `--version`/`-V`, and dispatches via [`run`].
fn main() -> Result<()> {
    // Must precede the first allocation; see `tune_system_allocator`.
    vct_core::utils::tune_system_allocator();

    // Writes only to `~/.vct/logs/` (default level `warn`), never the terminal.
    vct_core::logging::init();
    // Installed here rather than in `vct_core::logging` because terminal restoration belongs to the display layer.
    vct_tui::display::common::tui::ensure_terminal_panic_hook();

    // Opt in to `<cli> --version` subprocess probing, which core disables by default for library callers and tests.
    vct_core::quota::enable_cli_version_detection();

    if matches!(
        std::env::args_os().nth(1).and_then(|arg| arg.into_string().ok()),
        Some(arg) if arg == "--version" || arg == "-V"
    ) {
        println!("{}", vct_core::VERSION);
        return Ok(());
    }

    let result = run();
    if let Err(error) = &result {
        // Persist the final error to the log file before anyhow prints it and exits.
        log::error!("command failed: {error:#}");
    }
    // A detached worker may still be saving; returning now would kill it mid-write.
    vct_core::utils::finish_atomic_writes();
    result
}

/// Parses the CLI and dispatches the selected subcommand.
fn run() -> Result<()> {
    let cli = Cli::parse();
    reject_unusable_invocation(&cli);
    // After the parse, so a `--help` or a rejected flag exits without touching
    // the user's `~/.vct` at all.
    vct_core::utils::remove_legacy_cache_files();
    let auto_update = spawn_cli_auto_update(&cli);
    let result = dispatch(cli);
    if let Some(handle) = auto_update {
        let _ = handle.join();
    }
    result
}

fn dispatch(cli: Cli) -> Result<()> {
    let Some(command) = cli.command else {
        return open_app(Tab::Usage, [false; 4], false);
    };
    match command {
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
                        let projected = vct_core::analysis::project_code_analysis(&analysis);
                        vct_tui::display::analysis::display_analysis_text(&projected);
                    } else {
                        let projected = vct_core::analysis::project_code_analysis(&analysis);
                        vct_tui::display::analysis::display_analysis_table(&projected);
                    }
                }
                None => {
                    // Only the batch (all-sessions) path takes the writable load,
                    // so `analysis FILE`, `version`, `quota`, etc. never create
                    // or rewrite `~/.vct/config.toml`.
                    let config = vct_core::config::load();
                    vct_core::logging::apply(&config.logging);
                    let time_range = resolve_time_range_with_default(
                        daily,
                        weekly,
                        monthly,
                        all,
                        config.general.default_time_range,
                    );
                    if !(json || text || table) {
                        return open_app(Tab::Analysis, [daily, weekly, monthly, all], false);
                    }
                    let scan_pool =
                        Arc::new(build_scan_pool(config.performance.resolved_scan_threads())?);
                    if json {
                        let dataset = scan_pool.install(|| {
                            vct_core::analysis::collect_analysis_sessions_with(
                                time_range,
                                config.providers,
                                ParseMode::Full,
                            )
                        })?;
                        report_analysis_collection(&dataset.diagnostics)?;
                        write_pretty_json(&dataset)?;
                    } else {
                        let remote_sync = start_remotes_sync(&config.remotes)?;
                        let aggregation = scan_pool.install(|| {
                            vct_core::analysis::aggregate_sessions_by_model_with_diagnostics_and_remote_hook(
                                time_range,
                                config.providers,
                                || remote_sync.finish(),
                            )
                        })?;
                        report_analysis_collection(&aggregation.diagnostics)?;

                        if text {
                            vct_tui::display::analysis::display_analysis_text(&aggregation.data);
                        } else {
                            vct_tui::display::analysis::display_analysis_table(&aggregation.data);
                        }
                    }
                }
            }
        }

        Commands::Usage {
            json,
            by_day,
            text,
            table,
            merge_providers,
            daily,
            weekly,
            monthly,
            all,
        } => {
            if !(json || text || table) {
                return open_app(Tab::Usage, [daily, weekly, monthly, all], merge_providers);
            }
            let config = vct_core::config::load();
            vct_core::logging::apply(&config.logging);
            let time_range = resolve_time_range_with_default(
                daily,
                weekly,
                monthly,
                all,
                config.general.default_time_range,
            );
            let merge = merge_providers || config.usage.merge_models;
            let scan_pool = Arc::new(build_scan_pool(config.performance.resolved_scan_threads())?);
            let remote_sync = start_remotes_sync(&config.remotes)?;

            if json && by_day {
                let scan = scan_daily_usage_priced_with_remote_hook(
                    time_range,
                    config.providers,
                    &scan_pool,
                    || remote_sync.finish(),
                )?;
                if let Some(error) = &scan.pricing_error {
                    eprintln!(
                        "Warning: Failed to fetch pricing data: {error}. Costs will be unavailable."
                    );
                }
                report_usage_collection(&scan.collection.diagnostics)?;
                let output = vct_core::usage::price_daily_usage_data(
                    &scan.collection.data,
                    &scan.pricing,
                    merge_providers,
                );
                write_pretty_json(&output)?;
            } else {
                let scan = scan_usage_priced_with_remote_hook(
                    time_range,
                    config.providers,
                    &scan_pool,
                    || remote_sync.finish(),
                )?;
                if json {
                    if let Some(error) = &scan.pricing_error {
                        eprintln!(
                            "Warning: Failed to fetch pricing data: {error}. Costs will be unavailable."
                        );
                    }
                    report_usage_collection(&scan.collection.diagnostics)?;
                    let priced =
                        vct_core::usage::price_usage_data(&scan.collection.data, &scan.pricing);
                    let priced = if merge_providers {
                        vct_core::usage::merge_priced_usage_rows(priced)
                    } else {
                        priced
                    };
                    write_pretty_json(&priced)?;
                } else if text {
                    report_usage_collection(&scan.collection.diagnostics)?;
                    display_usage_text_with_pricing(&scan.collection.data, &scan.pricing, merge);
                } else {
                    report_usage_collection(&scan.collection.diagnostics)?;
                    display_usage_table_with_pricing(
                        &scan.collection.data,
                        &scan.pricing,
                        scan.pricing_error.as_deref(),
                        merge,
                    );
                }
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
                    .load_style(UTF8_FULL)
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
                vct_core::update::check_update()?;
            } else {
                vct_core::update::update_interactive(force)?;
            }
        }

        Commands::Quota {
            provider,
            json,
            text,
            table,
        } => match provider {
            Some(provider) => run_quota(provider, text, table)?,
            None if json => run_quota_cards()?,
            None => return open_app(Tab::Quota, [false; 4], false),
        },

        Commands::Ledger {
            action: LedgerAction::Export,
        } => {
            // Run over ssh by another machine, so nothing here may create or
            // rewrite this one's settings.
            let config = vct_core::config::load_read_only();
            let scan_pool = build_scan_pool(config.performance.resolved_scan_threads())?;
            let stdout = io::stdout();
            let mut writer = BufWriter::with_capacity(STDOUT_BUFFER_BYTES, stdout.lock());
            vct_core::remote::export(config.providers, &scan_pool, &mut writer)?;
            writer.flush()?;
        }

        Commands::Config { action } => {
            run_config(action.unwrap_or(ConfigAction::Show))?;
        }
    }

    Ok(())
}

/// Exits with code 2 before touching `~/.vct` if the bare command has no terminal or `quota` gets `--text`/`--table` without a provider.
fn reject_unusable_invocation(cli: &Cli) {
    use clap::CommandFactory;
    use std::io::IsTerminal;

    match &cli.command {
        None if !io::stdout().is_terminal() => {
            eprintln!("{}", Cli::command().render_help());
            std::process::exit(2);
        }
        Some(Commands::Quota {
            provider: None,
            text,
            table,
            ..
        }) if *text || *table => Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "--text and --table print one provider's response: name a provider",
            )
            .exit(),
        _ => {}
    }
}

/// Opens the interactive app on `start`; `period` is `[daily, weekly, monthly, all]` and `merge_providers` overrides `usage.merge_models`.
fn open_app(start: Tab, period: [bool; 4], merge_providers: bool) -> Result<()> {
    let config = vct_core::config::load();
    vct_core::logging::apply(&config.logging);
    let [daily, weekly, monthly, all] = period;
    let time_range = resolve_time_range_with_default(
        daily,
        weekly,
        monthly,
        all,
        config.general.default_time_range,
    );
    prune_remotes(&config.remotes)?;
    run_app(AppOptions {
        start,
        time_range,
        merge: merge_providers || config.usage.merge_models,
        config,
        scan_pool: None,
    })
}

/// Drops snapshots for hosts removed from `[remotes]` and returns the cache directory.
fn prune_remotes(config: &vct_core::config::RemotesConfig) -> Result<std::path::PathBuf> {
    let cache_dir = vct_core::utils::resolve_paths()?.cache_dir;
    // A file that failed to parse reads as no hosts, which must not cost the
    // snapshots of hosts that are merely unreachable right now.
    if vct_core::config::file_is_valid() {
        vct_core::remote::prune(&cache_dir, config);
    }
    Ok(cache_dir)
}

/// Background handle for an in-flight one-shot `[remotes]` sync overlapping with local scanning.
struct PendingRemoteSync {
    handle: Option<std::thread::JoinHandle<Vec<(String, String)>>>,
}

impl PendingRemoteSync {
    /// Waits for the background sync to complete and prints any per-host fetch warnings to stderr.
    fn finish(mut self) {
        if let Some(handle) = self.handle.take() {
            let failures = handle.join().unwrap_or_else(|_| {
                vec![("<remote>".to_string(), "remote sync panicked".to_string())]
            });
            for (host, error) in failures {
                eprintln!(
                    "Warning: Could not fetch usage from remote {host}: {error}. Its last fetched data, if any, is shown."
                );
            }
        }
    }
}

impl Drop for PendingRemoteSync {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Prunes removed `[remotes]` snapshots and spawns a background sync for the remaining hosts when online and non-empty.
fn start_remotes_sync(config: &vct_core::config::RemotesConfig) -> Result<PendingRemoteSync> {
    let cache_dir = prune_remotes(config)?;
    if config.hosts.is_empty() || vct_core::utils::network_disabled() {
        return Ok(PendingRemoteSync { handle: None });
    }
    let remotes = config.clone();
    let handle = std::thread::Builder::new()
        .name("vct-remote-sync".to_string())
        .spawn(move || vct_core::remote::sync(&cache_dir, &remotes))?;
    Ok(PendingRemoteSync {
        handle: Some(handle),
    })
}

/// Returns whether `cli` opens the interactive app via [`open_app`] rather than running a one-shot command.
fn opens_interactive_app(cli: &Cli) -> bool {
    matches!(
        &cli.command,
        None | Some(
            Commands::Analysis {
                file: None,
                json: false,
                text: false,
                table: false,
                ..
            } | Commands::Usage {
                json: false,
                text: false,
                table: false,
                ..
            } | Commands::Quota {
                provider: None,
                json: false,
                text: false,
                table: false,
            }
        )
    )
}

/// Returns whether `cli` is a one-shot command eligible for background startup auto-update checks.
fn auto_update_eligible_command(cli: &Cli) -> bool {
    !opens_interactive_app(cli)
        && !matches!(
            &cli.command,
            Some(Commands::Update { .. } | Commands::Config { .. } | Commands::Ledger { .. })
        )
}

/// Spawns a background auto-update check for eligible one-shot commands (`update`, `config`, and `ledger` are excluded), returning `None` when skipped, not due today, or disabled.
fn spawn_cli_auto_update(cli: &Cli) -> Option<std::thread::JoinHandle<()>> {
    if !auto_update_eligible_command(cli) || !vct_core::update::auto_update_is_due() {
        return None;
    }

    let config = vct_core::config::load_read_only();
    if !config.general.auto_update {
        return None;
    }
    Some(std::thread::spawn(|| {
        vct_core::update::maybe_auto_update(true);
    }))
}

/// Handles the `config` subcommand by dispatching one [`ConfigAction`].
fn run_config(action: ConfigAction) -> Result<()> {
    match action {
        // Pure stdout: schema generation needs no config file, so it must not
        // resolve (and thereby create) ~/.vct — keep it usable on a read-only home.
        ConfigAction::Schema => print!("{}", vct_core::config::schema_json()),
        ConfigAction::Path => {
            println!("{}", vct_core::utils::get_config_path()?.display());
        }
        ConfigAction::Show => {
            let path = vct_core::utils::get_config_path()?;
            // Ensure the file exists (first-run creation) before reading it back.
            let _ = vct_core::config::load();
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            println!("{}", "Vibe Coding Tracker settings".bright_cyan().bold());
            println!("{}", path.display().dimmed());
            println!();
            print!("{}", contents);
        }
        // A failed or aborted editor is a non-zero exit, so scripts (and
        // users) can tell the edit did not complete cleanly.
        ConfigAction::Edit => vct_core::config::open_in_editor()?,
        ConfigAction::Migrate => {
            use vct_core::config::MigrationStatus;
            let path = vct_core::utils::get_config_path()?;
            match vct_core::config::migrate_config_file(&path)? {
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

/// Fetches normalized quota cards for all enabled providers with credentials and prints them as pretty JSON.
fn run_quota_cards() -> Result<()> {
    let config = vct_core::config::load_read_only();
    let cards = vct_core::quota::fetch_quota_cards(&config.usage.quota.panels, config.providers)?;
    write_pretty_json(&cards)
}

/// Fetches `provider`'s raw quota response and prints it as text, table, or pretty JSON, returning an error after printing if the status is non-2xx.
fn run_quota(provider: QuotaProvider, text: bool, table: bool) -> Result<()> {
    use vct_core::quota::{
        ANTIGRAVITY_LOGIN_HINT, CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT,
        CURSOR_LOGIN_HINT, GROK_LOGIN_HINT, antigravity, claude, copilot, cursor, grok, http, wham,
    };
    use vct_tui::display::quota::{display_quota_table, display_quota_text, print_quota_json};

    let render = |body: &str| {
        if text {
            display_quota_text(body);
        } else if table {
            display_quota_table(body);
        } else {
            print_quota_json(body);
        }
    };

    let client = http::build_client()?;
    let (status, body) = match provider {
        QuotaProvider::Claude => claude::fetch_claude_raw(&client),
        QuotaProvider::Codex => wham::fetch_codex_raw(&client),
        QuotaProvider::Copilot => copilot::fetch_copilot_raw(&client),
        QuotaProvider::Cursor => cursor::fetch_cursor_raw(&client),
        QuotaProvider::Grok => grok::fetch_grok_raw(&client),
        QuotaProvider::Antigravity => antigravity::fetch_antigravity_raw(&client),
    }?;

    render(&body);

    if !(200..300).contains(&status) {
        let (name, hint) = match provider {
            QuotaProvider::Claude => ("claude", CLAUDE_LOGIN_HINT),
            QuotaProvider::Codex => ("codex", CODEX_LOGIN_HINT),
            QuotaProvider::Copilot => ("copilot", COPILOT_LOGIN_HINT),
            QuotaProvider::Cursor => ("cursor", CURSOR_LOGIN_HINT),
            QuotaProvider::Grok => ("grok", GROK_LOGIN_HINT),
            QuotaProvider::Antigravity => ("antigravity", ANTIGRAVITY_LOGIN_HINT),
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
    let mut writer = BufWriter::with_capacity(STDOUT_BUFFER_BYTES, stdout.lock());
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}

/// Prints a stderr note when any sessions in `diagnostics` came only from the ledger rather than on-disk logs.
fn report_retained_sessions(diagnostics: &vct_core::ScanDiagnostics) {
    if diagnostics.retained > 0 {
        eprintln!(
            "Note: {} sessions not found on disk are included from the session ledger (~/.vct/sessions).",
            diagnostics.retained
        );
    }
}

/// Returns an error if every analysis source failed, or warns on stderr on partial failures.
fn report_analysis_collection(diagnostics: &vct_core::analysis::ScanDiagnostics) -> Result<()> {
    report_retained_sessions(diagnostics);
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

/// Returns an error if every usage source failed, or warns on stderr on partial failures.
fn report_usage_collection(diagnostics: &vct_core::usage::ScanDiagnostics) -> Result<()> {
    report_retained_sessions(diagnostics);
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

#[cfg(test)]
mod tests {
    use super::{auto_update_eligible_command, opens_interactive_app};
    use crate::cli::Cli;
    use clap::Parser;

    #[test]
    fn interactive_classification_matches_every_app_entry_point() {
        for args in [
            &["vct"][..],
            &["vct", "usage"],
            &["vct", "usage", "--daily", "--merge-providers"],
            &["vct", "analysis"],
            &["vct", "analysis", "--weekly"],
            &["vct", "quota"],
        ] {
            assert!(
                opens_interactive_app(&Cli::parse_from(args)),
                "{args:?} must be classified as interactive"
            );
        }

        for args in [
            &["vct", "usage", "--table"][..],
            &["vct", "usage", "--json"],
            &["vct", "usage", "--json", "--by-day"],
            &["vct", "analysis", "--text"],
            &["vct", "analysis", "session.jsonl"],
            &["vct", "quota", "--json"],
            &["vct", "quota", "claude"],
            &["vct", "ledger", "export"],
            &["vct", "version"],
            &["vct", "update"],
            &["vct", "config", "show"],
        ] {
            assert!(
                !opens_interactive_app(&Cli::parse_from(args)),
                "{args:?} must be classified as one-shot"
            );
        }
    }

    #[test]
    fn auto_update_excludes_interactive_update_config_and_ledger_commands() {
        for args in [
            &["vct", "usage", "--table"][..],
            &["vct", "usage", "--json"],
            &["vct", "usage", "--json", "--by-day"],
            &["vct", "analysis", "--text"],
            &["vct", "analysis", "session.jsonl"],
            &["vct", "quota", "--json"],
            &["vct", "quota", "claude"],
            &["vct", "version"],
        ] {
            assert!(
                auto_update_eligible_command(&Cli::parse_from(args)),
                "{args:?} must stay eligible for startup auto-update"
            );
        }

        for args in [
            &["vct"][..],
            &["vct", "usage"],
            &["vct", "analysis"],
            &["vct", "quota"],
            &["vct", "update"],
            &["vct", "config", "show"],
            &["vct", "ledger", "export"],
        ] {
            assert!(
                !auto_update_eligible_command(&Cli::parse_from(args)),
                "{args:?} must skip CLI startup auto-update"
            );
        }
    }
}
