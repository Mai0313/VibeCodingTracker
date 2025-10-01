use anyhow::Result;
use codex_usage::cli::{Cli, Commands};
use codex_usage::usage::{display_usage_table, get_usage_from_directories};
use codex_usage::{analyze_jsonl_file, get_version_info, PKG_DESCRIPTION, PKG_NAME};

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

        Commands::Version => {
            let version_info = get_version_info();
            println!("{} v{}", PKG_NAME, version_info.version);
            println!("{}", PKG_DESCRIPTION);
        }
    }

    Ok(())
}
