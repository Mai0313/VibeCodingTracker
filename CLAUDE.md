# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Vibe Coding Tracker** is a Rust CLI tool that analyzes AI coding assistant usage (Claude Code, Codex, and Gemini) by parsing JSONL session files, calculating token usage, computing costs via LiteLLM pricing data, and presenting insights through multiple output formats (interactive TUI, static tables, JSON, text).

**Binary names:**

- Full: `vibe_coding_tracker`
- Short alias: `vct`

## Build & Development Commands

```bash
# Build (debug mode)
cargo build

# Build release
cargo build --release
# or
make release

# Run tests
cargo test --all
# or
make test

# Format and lint
cargo fmt --all
cargo clippy --all-targets --all-features
# or
make fmt

# Run the CLI (debug)
./target/debug/vibe_coding_tracker <command>
# or release
./target/release/vct <command>

# Coverage (requires cargo-llvm-cov)
make coverage
```

## CLI Commands

```bash
# Interactive dashboard (default, updates every second)
vct usage

# Static table output
vct usage --table

# Plain text output (Date > model: cost format)
vct usage --text

# JSON output with full precision
vct usage --json

# Analyze specific conversation file
vct analysis --path <file.jsonl> [--output <output.json>]

# Batch analyze all sessions (interactive table by default)
vct analysis

# Batch analyze and save to JSON
vct analysis --output <output.json>

# Batch analyze all sessions grouped by provider (claude/codex/gemini)
vct analysis --all

# Batch analyze grouped by provider and save to JSON
vct analysis --all --output <output.json>

# Version info
vct version [--json|--text]

# Update to latest version from GitHub releases
vct update                # Interactive update with confirmation
vct update --force        # Force update without confirmation
vct update --check        # Only check for updates without installing
```

## Code Architecture

### Module Structure

```
src/
├── main.rs              # CLI entry point, command routing
├── lib.rs               # Library exports, version info
├── cli.rs               # Clap CLI definitions
├── pricing.rs           # LiteLLM pricing fetch, caching, fuzzy matching
├── update.rs            # Self-update functionality from GitHub releases
├── models/              # Data structures
│   ├── analysis.rs      # CodeAnalysis struct
│   ├── usage.rs         # DateUsageResult
│   ├── claude.rs        # Claude-specific types
│   ├── codex.rs         # Codex/OpenAI types
│   └── gemini.rs        # Gemini-specific types
├── analysis/            # JSONL analysis pipeline
│   ├── analyzer.rs      # Main entry: analyze_jsonl_file()
│   ├── batch_analyzer.rs # Batch analysis: analyze_all_sessions()
│   ├── display.rs       # Interactive TUI and table display for analysis
│   ├── detector.rs      # Detect Claude vs Codex vs Gemini format
│   ├── claude_analyzer.rs
│   ├── codex_analyzer.rs
│   └── gemini_analyzer.rs
├── usage/               # Usage aggregation & display
│   ├── calculator.rs    # get_usage_from_directories(), per-file aggregation
│   └── display.rs       # Interactive TUI, table, text, JSON formatters
└── utils/               # Helper utilities
    ├── file.rs          # read_jsonl, save_json_pretty
    ├── paths.rs         # resolve_paths (Claude/Codex/Gemini dirs)
    ├── git.rs           # Git remote detection
    └── time.rs          # Date formatting
```

### Key Flows

**1. Usage Command (`vct usage`):**

- `main.rs::Commands::Usage` → `usage/calculator.rs::get_usage_from_directories()`
  - Scans `~/.claude/projects/*.jsonl`, `~/.codex/sessions/*.jsonl`, and `~/.gemini/tmp/*.jsonl`
  - For each file, calls `analysis/analyzer.rs::analyze_jsonl_file()` for unified parsing
  - Extracts `conversationUsage` from `CodeAnalysis` result
  - Aggregates token usage by date and model into `DateUsageResult`
- `pricing.rs::fetch_model_pricing()` → fetches/caches LiteLLM pricing daily
- `usage/display.rs::display_usage_*()` → formats output (interactive/table/text/JSON)
  - Interactive mode uses Ratatui with 1-second refresh
  - Table mode uses comfy-table with UTF8_FULL preset
  - Text mode outputs: `Date > model: cost`
  - JSON includes full precision costs

**2. Analysis Command (`vct analysis`):**

**Single File Mode** (with `--path`):

- `main.rs::Commands::Analysis` → `analysis/analyzer.rs::analyze_jsonl_file()`
  - `detector.rs` determines Claude vs Codex vs Gemini format (checks `parentUuid` for Claude, `sessionId` for Gemini)
  - Routes to `claude_analyzer.rs`, `codex_analyzer.rs`, or `gemini_analyzer.rs`
  - Extracts: conversation usage, tool call counts, file operations, Git info
- Outputs detailed JSON with metadata (user, machineId, Git remote, etc.)

**Batch Mode** (without `--path`):

- `main.rs::Commands::Analysis` → `analysis/batch_analyzer.rs::analyze_all_sessions()`
  - Scans `~/.claude/projects/*.jsonl`, `~/.codex/sessions/*.jsonl`, and `~/.gemini/tmp/*.jsonl`
  - For each file, calls `analyze_jsonl_file()` (same as Usage command)
  - Extracts metrics from `CodeAnalysis` results and aggregates by date and model
  - Groups metrics: edit/read/write lines, tool call counts (Bash, Edit, Read, TodoWrite, Write)
- `analysis/display.rs::display_analysis_interactive()` → Interactive TUI (default)
  - Ratatui-based table with 1-second refresh
  - Columns: Date, Model, Edit Lines, Read Lines, Write Lines, Bash, Edit, Read, TodoWrite, Write
  - Shows totals row and memory usage
- With `--output`: Saves aggregated results as JSON array

**Batch Mode with Provider Grouping** (with `--all`):

- `main.rs::Commands::Analysis` → `analysis/batch_analyzer.rs::analyze_all_sessions_by_provider()`
  - Scans and processes each provider directory separately
  - Returns `ProviderGroupedAnalysis` struct with complete CodeAnalysis records for each provider
  - Output includes full records with all conversation usage, file operations, and tool call details
- Default behavior: Outputs JSON directly to stdout
  - Keys: "Claude-Code", "Codex", "Gemini"
  - Values: Arrays of complete CodeAnalysis objects with full records
- With `--output`: Saves the JSON to the specified file path

**3. Pricing System:**

- URL: `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json`
- Cache location: `~/.vibe-coding-tracker/model_pricing_YYYY-MM-DD.json`
- Cache TTL: 24 hours (by date)
- Matching strategy (priority order):
  1. Exact match
  2. Normalized match (strip version suffixes)
  3. Substring match
  4. Fuzzy match (Jaro-Winkler ≥ 0.7 threshold)
  5. Fallback to $0.00
- Cost calculation: `(input × input_cost) + (output × output_cost) + (cache_read × cache_cost) + (cache_creation × creation_cost)`

**4. Update Command (`vct update`):**

- `main.rs::Commands::Update` → `update.rs::update_interactive()` or `check_update()`
  - Fetches latest release from GitHub API: `https://api.github.com/repos/Mai0313/VibeCodingTracker/releases/latest`
  - Uses `semver` crate for semantic version comparison (not string comparison)
  - Compares current version (from `CARGO_PKG_VERSION`) with latest tag version
  - Downloads platform-specific compressed archive from release assets
  - Extracts the archive (`.tar.gz` for Unix, `.zip` for Windows)
  - **Linux/macOS**:
    - Extracts `.tar.gz` archive
    - Renames current binary to `.old` (backup)
    - Replaces with new binary directly
    - User can restart immediately
  - **Windows**:
    - Extracts `.zip` archive
    - Downloads new binary with `.new` extension
    - Creates batch script (`update_vct.bat`) to replace after exit
    - User must run batch script to complete update
- Platform detection uses `env::consts::OS` and `env::consts::ARCH`
- Asset naming convention: `vibe_coding_tracker-v{version}-{os}-{arch}[-gnu].{ext}`
  - Linux: `vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz`, `vibe_coding_tracker-v0.1.6-linux-arm64-gnu.tar.gz`
  - macOS: `vibe_coding_tracker-v0.1.6-macos-x64.tar.gz`, `vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz`
  - Windows: `vibe_coding_tracker-v0.1.6-windows-x64.zip`, `vibe_coding_tracker-v0.1.6-windows-arm64.zip`

### Data Format Detection

**Claude Code format:**

- Presence of `parentUuid` field in records
- Fields: `type`, `message.model`, `message.usage`, `message.content` (with tool_use blocks)

**Codex format:**

- OpenAI-style structure
- Fields: `completion_response.usage`, `total_token_usage`, `reasoning_output_tokens`

**Gemini format:**

- Single session object structure
- Presence of `sessionId`, `projectHash`, and `messages` fields
- Fields: `messages[].tokens` (input, output, cached, thoughts, tool, total)

## Testing

```bash
# Run all tests
cargo test --all

# Run specific test file
cargo test --test test_integration_usage

# Run expected output validation tests
cargo test --test test_analysis_expected_output

# Run with verbose output
cargo test --all --verbose

# Example conversation files for testing
examples/test_conversation.jsonl          # Claude Code format
examples/test_conversation_oai.jsonl       # Codex format
examples/test_conversation_gemini.json     # Gemini format

# Expected analysis output files for validation
examples/analysis_result.json              # Expected Claude Code output
examples/analysis_result_oai.json          # Expected Codex output
examples/analysis_result_gemini.json       # Expected Gemini output
```

### Expected Output Validation Tests

The `test_analysis_expected_output.rs` test suite validates that `analysis --path` produces consistent output:

- **Purpose**: Ensure analysis output matches expected results for all three formats (Claude Code, Codex, Gemini)
- **Ignored Fields**: `insightsVersion`, `machineId`, `user` (environment-specific)
- **Test Cases**:
  - `test_claude_code_analysis_matches_expected`: Validates Claude Code analysis
  - `test_codex_analysis_matches_expected`: Validates Codex/OpenAI analysis
  - `test_gemini_analysis_matches_expected`: Validates Gemini analysis
- **Helper Function**: `compare_json_ignore_fields()` recursively compares JSON while ignoring specific fields

Run these tests to verify that changes to the analysis logic haven't altered the output format:

```bash
cargo test --test test_analysis_expected_output -- --nocapture
```

## Docker

```bash
# Build production image
docker build -f docker/Dockerfile --target prod -t vct:latest .

# Run with session directories mounted
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.gemini:/root/.gemini \
    vct:latest usage
```

## Dependencies

**CLI & Serialization:**

- `clap` (derive) - CLI parsing
- `serde`, `serde_json` - JSON handling

**TUI:**

- `ratatui` - Terminal UI framework
- `crossterm` - Terminal control
- `comfy-table` - Static table rendering
- `owo-colors` - Color output

**Core:**

- `anyhow`, `thiserror` - Error handling
- `chrono` - Date/time
- `reqwest` (rustls-tls) - HTTP client for pricing fetch and update downloads
- `walkdir` - Directory traversal
- `regex` - Pattern matching
- `strsim` - Fuzzy string matching (Jaro-Winkler)
- `semver` - Semantic version parsing and comparison (for update command)
- `flate2` - Gzip decompression (for .tar.gz archives)
- `tar` - Tar archive extraction
- `zip` - Zip archive extraction
- `home` - Home directory resolution
- `hostname` - System hostname
- `sysinfo` - Memory/system stats

## Important Patterns

**1. Unified Parsing Architecture:**

- **Single Source of Truth**: All commands (`usage`, `analysis --path`, and `analysis`) use the same parsing pipeline via `analyze_jsonl_file()`
- **Format Detection**: `detector.rs` automatically identifies Claude/Codex/Gemini format
- **Parser Routing**: Routes to appropriate analyzer (`claude_analyzer`, `codex_analyzer`, `gemini_analyzer`)
- **Data Extraction**:
  - `usage` command extracts only `conversationUsage` from `CodeAnalysis`
  - `analysis` command uses full `CodeAnalysis` including file operations and tool calls
- **Benefits**: Eliminates code duplication, ensures consistency, and simplifies maintenance

**2. Cost Rounding:**

- Interactive/table mode: round to 2 decimals (`$2.15`)
- JSON/text mode: full precision (`2.1542304567890123`)

**3. Date Aggregation:**

- Group usage by date (YYYY-MM-DD format)
- Display totals row in tables

**4. Interactive TUI:**

- Auto-refresh every 1 second
- Highlight today's entries
- Show memory usage and summary stats
- Exit keys: `q`, `Esc`, `Ctrl+C`

**5. Model Name Handling:**

- Always use fuzzy matching when looking up pricing
- Store matched model name for transparency
- Handle multiple formats: Claude (`claude-sonnet-4-20250514`), OpenAI (`gpt-4-turbo`), and Gemini (`gemini-2.0-flash-exp`)

## Session File Locations

- **Claude Code:** `~/.claude/projects/*.jsonl`
- **Codex:** `~/.codex/sessions/*.jsonl`
- **Gemini:** `~/.gemini/tmp/*.jsonl`

## Troubleshooting Commands

```bash
# Debug mode
RUST_LOG=debug vct usage

# Check cache
ls -la ~/.vibe-coding-tracker/

# Force pricing refresh
rm -rf ~/.vibe-coding-tracker/
vct usage

# Verify session directories
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/
ls -la ~/.gemini/tmp/
```

## Output Examples

**Usage Text format:**

```
2025-10-01 > claude-sonnet-4-20250514: $2.154230
2025-10-02 > gpt-4-turbo: $0.250000
```

**Usage JSON format:**

```json
{
  "2025-10-01": [
    {
      "model": "claude-sonnet-4-20250514",
      "usage": {
        "input_tokens": 45230,
        "output_tokens": 12450,
        "cache_read_input_tokens": 230500,
        "cache_creation_input_tokens": 50000
      },
      "cost_usd": 2.1542304567890125,
      "matched_model": "claude-sonnet-4"
    }
  ]
}
```

**Batch Analysis JSON format:**

```json
[
  {
    "date": "2025-10-02",
    "model": "claude-sonnet-4-5-20250929",
    "editLines": 901,
    "readLines": 11525,
    "writeLines": 53,
    "bashCount": 13,
    "editCount": 26,
    "readCount": 27,
    "todoWriteCount": 10,
    "writeCount": 1
  },
  {
    "date": "2025-10-03",
    "model": "claude-sonnet-4-5-20250929",
    "editLines": 574,
    "readLines": 10057,
    "writeLines": 1415,
    "bashCount": 53,
    "editCount": 87,
    "readCount": 78,
    "todoWriteCount": 30,
    "writeCount": 8
  }
]
```

**Batch Analysis with --all (Provider Grouped) JSON format:**

```json
{
  "Claude-Code": [
    {
      "extensionName": "Claude-Code",
      "insightsVersion": "0.1.9",
      "machineId": "18f309cbbb654be69eff5ff79d2f3fa6",
      "records": [
        {
          "conversationUsage": {
            "claude-sonnet-4-20250514": {
              "input_tokens": 252,
              "output_tokens": 3921,
              "cache_read_input_tokens": 1298818,
              "cache_creation_input_tokens": 124169
            }
          },
          "editFileDetails": [...],
          "readFileDetails": [...],
          "writeFileDetails": [...],
          "runCommandDetails": [...],
          "toolCallCounts": {
            "Bash": 1,
            "Edit": 3,
            "Read": 2,
            "TodoWrite": 14,
            "Write": 3
          },
          "totalEditLines": 2,
          "totalReadLines": 42,
          "totalWriteLines": 441,
          "taskId": "b162b1ae-97bc-475f-9b5f-ffbf55ca5b3f",
          "timestamp": 1756386827562,
          "folderPath": "/home/wei/repo/claude-code",
          "gitRemoteUrl": "https://github.com/Mai0313/claude-code"
        }
      ],
      "user": "wei"
    }
  ],
  "Codex": [...],
  "Gemini": [...]
}
```

## Release Profile

The release build uses aggressive optimizations:

- LTO: thin
- Codegen units: 1
- Stripped symbols
- Target binary size: ~3-5 MB
