# VibeCodingTracker - Developer Instructions

## Project Overview

VibeCodingTracker is a Rust-based CLI tool that parses and analyzes JSONL event logs from **Claude Code** and **Codex**, producing aggregated usage statistics and detailed code analysis reports. This is a complete Rust translation of the original Go implementation.

### Key Features

1. **Automatic Format Detection**: Detects whether logs are from Claude Code or Codex
2. **Comprehensive Analysis**: Extracts file operations, tool calls, and token usage
3. **Beautiful Output**: Multiple display formats (interactive table, static table, text, JSON)
4. **Intelligent Pricing**: Fetches model pricing from LiteLLM with daily caching and fuzzy model matching
5. **Performance**: Rust's zero-cost abstractions ensure fast processing

---

## CLI Commands

### 1. Analysis Command

**Purpose**: Analyzes JSONL conversation files from Claude Code or Codex and produces detailed CodeAnalysis reports.

#### Usage

```bash
# Analyze and output to stdout
vibe_coding_tracker analysis --path examples/test_conversation.jsonl

# Analyze and save to file
vibe_coding_tracker analysis --path examples/test_conversation.jsonl --output result.json

# Analyze Codex logs
vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl
```

#### Implementation Details

**Main Entry Point**: `src/main.rs:17-30`
- Calls `analyze_jsonl_file()` from `src/analysis/analyzer.rs`
- Optionally saves result to file using `utils::save_json_pretty()`

**Core Analysis Flow**: `src/analysis/analyzer.rs`

1. **File Reading** (`analyzer.rs:12-23`)
   - Reads JSONL file using `utils::read_jsonl()`
   - Returns empty JSON if file is empty

2. **Format Detection** (`analyzer.rs:19`)
   - Uses `analysis::detector::detect_extension_type()`
   - Implementation: `src/analysis/detector.rs:7-35`
   - Detection logic:
     - Checks for `parentUuid` field → Claude Code
     - Checks for `turn_context` event → Codex
     - Default: Codex

3. **Data Processing** (`analyzer.rs:26-36`)
   - **Claude Code**: `src/analysis/claude_analyzer.rs`
     - Parses file operations (Read, Write, Edit, Bash, TodoWrite)
     - Extracts tool calls from assistant messages
     - Aggregates token usage by model
     - Groups by conversation task (parentUuid)

   - **Codex**: `src/analysis/codex_analyzer.rs`
     - Parses shell commands
     - Extracts token usage from event_msg events
     - Groups by conversation ID

4. **Metadata Enrichment** (`analyzer.rs:38-42`)
   - Adds user name from environment (`USER` or `USERNAME`)
   - Adds machine ID (from `/etc/machine-id` or hostname)
   - Adds extension type (Claude-Code or Codex)
   - Adds insights version from `Cargo.toml`

**Output Structure** (`src/models/analysis.rs`)

```rust
CodeAnalysis {
    user: String,                    // Current user
    extension_name: String,          // "Claude-Code" or "Codex"
    insights_version: String,        // Package version
    machine_id: String,              // Machine identifier
    records: Vec<CodeAnalysisRecord> // Analysis records
}

CodeAnalysisRecord {
    total_unique_files: usize,       // Unique files touched
    total_write_lines: usize,        // Lines written
    total_read_lines: usize,         // Lines read
    total_edit_lines: usize,         // Lines edited
    total_write_characters: usize,   // Characters written
    total_read_characters: usize,    // Characters read
    total_edit_characters: usize,    // Characters edited
    write_file_details: Vec<...>,    // Write operation details
    read_file_details: Vec<...>,     // Read operation details
    edit_file_details: Vec<...>,     // Edit operation details
    run_command_details: Vec<...>,   // Command execution details
    tool_call_counts: {...},         // Tool call counters
    conversation_usage: HashMap,     // Token usage by model
    task_id: String,                 // Task identifier
    timestamp: i64,                  // Unix timestamp (milliseconds)
    folder_path: String,             // Working directory
    git_remote_url: String           // Git remote URL
}
```

**Key Implementation Details**:
- File paths are normalized and deduplicated
- Line counts are calculated from content
- Character counts include all content
- Git remote URL extracted using `utils::git::get_git_remote_url()`
- Timestamps are parsed as ISO 8601 strings and converted to Unix milliseconds

---

### 2. Usage Command

**Purpose**: Displays token usage statistics from Claude Code and Codex sessions, organized by date and model.

#### Usage

```bash
# Display interactive table (default, refreshes every 1 second)
vibe_coding_tracker usage

# Display static table
vibe_coding_tracker usage --table

# Display plain text
vibe_coding_tracker usage --text

# Display JSON with full precision costs
vibe_coding_tracker usage --json
```

#### Implementation Details

**Main Entry Point**: `src/main.rs:32-145`

**Session Directory Discovery** (`src/utils/paths.rs:16-34`)
```rust
// Claude Code sessions
~/.claude/projects/*.jsonl

// Codex sessions
~/.codex/sessions/*.jsonl
```

**Usage Calculation Flow** (`src/usage/calculator.rs`)

1. **Directory Processing** (`calculator.rs:29-45`)
   - Walks through both Claude and Codex session directories
   - Processes all `.jsonl` files
   - Groups usage by file modification date (YYYY-MM-DD format)

2. **File Analysis** (`calculator.rs:9-27`)
   - Auto-detects format (Claude-Code vs Codex)
   - Extracts token usage and tool call counts

3. **Claude Usage Extraction** (`calculator.rs:63-117`)
   - Parses `assistant` messages
   - Extracts from `message.usage` object:
     - `input_tokens`
     - `output_tokens`
     - `cache_read_input_tokens`
     - `cache_creation_input_tokens`
     - `cache_creation` (nested object)
     - `service_tier`
   - Counts tool uses from `content` array with `type: "tool_use"`

4. **Codex Usage Extraction** (`calculator.rs:119-175`)
   - Extracts model from `turn_context` events
   - Parses `event_msg` with `type: "token_count"`
   - Extracts from `info.total_token_usage`:
     - `input_tokens`
     - `output_tokens`
     - `reasoning_output_tokens` (added to output_tokens)
     - `cached_input_tokens` (cache read)
     - `total_tokens`
   - Counts shell commands from `response_item` with `type: "function_call"` and `name: "shell"`

5. **Usage Aggregation** (`calculator.rs:298-399`)
   - Merges usage data from multiple files
   - Sums token counts by model and date
   - Preserves model-specific metadata

**Model Pricing System** (`src/pricing.rs`)

1. **Pricing Source**
   - URL: `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json`
   - Contains pricing for all major LLM models

2. **Caching Mechanism** (`pricing.rs:45-141`)
   - Cache directory: `~/.vibe-coding-tracker/`
   - Cache filename pattern: `model_pricing_YYYY-MM-DD.json`
   - Cache lifetime: **Daily** (one cache file per day)
   - Old cache cleanup: Automatically removes cache files from previous days
   - Cache operations:
     - `load_from_cache()`: Loads today's cache if exists
     - `save_to_cache()`: Saves pricing with today's date
     - `cleanup_old_cache()`: Removes old cache files

3. **Model Matching Strategy** (`pricing.rs:195-260`)

   **Priority order**:
   1. **Exact match**: Direct lookup in pricing map
   2. **Normalized match**: Removes date suffixes (e.g., `-20240229`), version patterns (e.g., `-v1.0`), and provider prefixes (e.g., `bedrock/`)
   3. **Substring match**: Checks if model name contains pricing key or vice versa
   4. **Fuzzy match**: Uses Jaro-Winkler similarity algorithm
      - Threshold: 0.7
      - Case-insensitive comparison
      - Returns best match above threshold
   5. **Default fallback**: Returns zero costs if no match found

4. **Cost Calculation** (`pricing.rs:178-193`)
   ```rust
   total_cost =
       (input_tokens × input_cost_per_token) +
       (output_tokens × output_cost_per_token) +
       (cache_read_tokens × cache_read_input_token_cost) +
       (cache_creation_tokens × cache_creation_input_token_cost)
   ```

**Display Modes** (`src/usage/display.rs`)

1. **Interactive Mode (Default)** (`display.rs:25-289`)
   - Uses `ratatui` and `crossterm` for terminal UI
   - **Features**:
     - Auto-refreshes every 1 second
     - Highlights today's entries with dark gray background
     - Highlights recently updated rows (within 1 second) with green background
     - Shows memory usage of current process
     - Displays summary statistics (total cost, total tokens, entry count)
   - **Navigation**:
     - Press `q`, `Esc`, or `Ctrl+C` to quit
   - **Formatting**:
     - Costs rounded to 2 decimal places: `${:.2}`
     - Numbers formatted with thousand separators: `1,234,567`
   - **System Monitoring**:
     - Uses `sysinfo` crate to track memory usage
     - Updates process info on every refresh

2. **Static Table Mode** (`--table`) (`display.rs:291-432`)
   - Uses `comfy-table` for formatted output
   - **Formatting**:
     - Costs rounded to 2 decimal places: `${:.2}`
     - Numbers formatted with thousand separators
     - Color-coded output (cyan dates, green models, red totals)
   - **Features**:
     - Includes totals row at bottom
     - Shows fuzzy-matched model names in parentheses
     - UTF8 borders (UTF8_FULL preset)

3. **Text Mode** (`--text`) (`display.rs:530-561`)
   - Plain text output, one line per model per date
   - Format: `YYYY-MM-DD > model: $0.123456`
   - **Formatting**:
     - Costs shown with 6 decimal places: `${:.6}`
     - No thousand separators
     - Minimal formatting for scripting/parsing

4. **JSON Mode** (`--json`) (`main.rs:33-134`)
   - Full precision JSON output
   - **Cost Calculation**: Unlike table modes, costs are **NOT rounded** - they are stored as full precision `f64` values
   - **Structure**:
     ```json
     {
       "YYYY-MM-DD": [
         {
           "model": "model-name",
           "usage": { /* token usage data */ },
           "cost_usd": 0.123456789,  // Full precision, not rounded
           "matched_model": "matched-name"  // Only present if fuzzy matched
         }
       ]
     }
     ```
   - **Use cases**: Data export, cost tracking, integration with other tools

**Key Differences Between Display Modes**:

| Feature | Interactive | Table | Text | JSON |
|---------|-------------|-------|------|------|
| Cost Rounding | 2 decimals | 2 decimals | 6 decimals | **Full precision** |
| Number Format | Thousands sep | Thousands sep | Raw | Raw |
| Auto-refresh | Yes (1s) | No | No | No |
| Highlighting | Yes | No | No | No |
| Fuzzy Match Display | In model name | In model name | In model name | Separate field |
| Memory Usage | Yes | No | No | No |
| Colors | Yes | Yes | No | No |

---

### 3. Version Command

**Purpose**: Displays version information about the VibeCodingTracker binary and build environment.

#### Usage

```bash
# Display formatted table (default)
vibe_coding_tracker version

# Display as JSON
vibe_coding_tracker version --json

# Display as plain text
vibe_coding_tracker version --text
```

#### Implementation Details

**Main Entry Point**: `src/main.rs:147-199`

**Version Information** (`src/lib.rs:14-26`)

1. **Package Version**
   - Source: `Cargo.toml` version field
   - Accessed via: `env!("CARGO_PKG_VERSION")`
   - Embedded at compile time

2. **Rust Version** (`lib.rs:28-45`)
   - Runtime detection: Executes `rustc --version`
   - Parses output to extract version number
   - Example: "1.28.2" from "rustc 1.28.2 (xxxxx)"
   - Fallback: "unknown" if command fails

3. **Cargo Version** (`lib.rs:47-64`)
   - Runtime detection: Executes `cargo --version`
   - Parses output to extract version number
   - Example: "1.89.0" from "cargo 1.89.0 (xxxxx)"
   - Fallback: "unknown" if command fails

**Display Formats**

1. **Default (Table)** (`main.rs:164-198`)
   - Formatted table using `comfy-table`
   - Colored output (green labels, white values)
   - Title: "🚀 Vibe Coding Tracker" (bright cyan, bold)
   - UTF8 borders

2. **JSON Format** (`main.rs:150-157`)
   ```json
   {
     "Version": "0.1.0",
     "Rust Version": "1.28.2",
     "Cargo Version": "1.89.0"
   }
   ```

3. **Text Format** (`main.rs:159-163`)
   ```
   Version: 0.1.0
   Rust Version: 1.28.2
   Cargo Version: 1.89.0
   ```

---

## Project Architecture

### Module Structure

```
src/
├── lib.rs                      # Library entry point, version info
├── main.rs                     # CLI entry point, command routing
├── cli.rs                      # Command-line argument parsing (clap)
├── pricing.rs                  # Model pricing fetch, cache, fuzzy matching
├── models/
│   ├── mod.rs                 # Module exports
│   ├── analysis.rs            # CodeAnalysis data structures
│   ├── usage.rs               # Usage statistics structures
│   ├── claude.rs              # Claude Code log models
│   └── codex.rs               # Codex log models
├── analysis/
│   ├── mod.rs                 # Module exports
│   ├── analyzer.rs            # Main analysis orchestrator
│   ├── detector.rs            # Format detection logic
│   ├── claude_analyzer.rs     # Claude Code log parser
│   └── codex_analyzer.rs      # Codex log parser
├── usage/
│   ├── mod.rs                 # Module exports
│   ├── calculator.rs          # Usage calculation and aggregation
│   └── display.rs             # Display formatting (interactive, table, text)
└── utils/
    ├── mod.rs                 # Module exports
    ├── paths.rs               # Path resolution, user/machine ID
    ├── time.rs                # ISO timestamp parsing
    ├── file.rs                # JSONL reading, JSON saving
    └── git.rs                 # Git remote URL extraction
```

### Key Dependencies

#### CLI and Formatting
- **clap** (4.5): Command-line argument parsing with derive macros
- **comfy-table** (7.1): Static table formatting
- **ratatui** (0.28): Terminal UI framework for interactive mode
- **crossterm** (0.28): Terminal manipulation (raw mode, events)
- **owo-colors** (4.0): Terminal color styling

#### Data Processing
- **serde** (1.0): Serialization framework
- **serde_json** (1.0): JSON parsing and generation
- **chrono** (0.4): Date/time handling
- **regex** (1.10): Pattern matching in logs

#### File System and Network
- **walkdir** (2.5): Recursive directory traversal
- **home** (0.5): Home directory resolution
- **reqwest** (0.12): HTTP client for pricing fetch
- **hostname** (0.4): Hostname detection

#### Error Handling
- **anyhow** (1.0): Flexible error handling
- **thiserror** (1.0): Custom error types

#### Utilities
- **strsim** (0.11): String similarity (Jaro-Winkler for fuzzy matching)
- **sysinfo** (0.32): System information and process monitoring
- **log** (0.4): Logging framework
- **env_logger** (0.11): Environment-based logging configuration

### Data Flow

```
User Input
    ↓
CLI Parsing (cli.rs)
    ↓
Command Router (main.rs)
    ↓
    ├─→ Analysis Command
    │       ↓
    │   Read JSONL (utils/file.rs)
    │       ↓
    │   Detect Format (analysis/detector.rs)
    │       ↓
    │   Parse & Analyze (analysis/*_analyzer.rs)
    │       ↓
    │   Enrich Metadata (utils/paths.rs, utils/git.rs)
    │       ↓
    │   Output JSON
    │
    ├─→ Usage Command
    │       ↓
    │   Scan Directories (utils/paths.rs)
    │       ↓
    │   Calculate Usage (usage/calculator.rs)
    │       ↓
    │   Fetch Pricing (pricing.rs → HTTP/Cache)
    │       ↓
    │   Calculate Costs (pricing.rs)
    │       ↓
    │   Display (usage/display.rs)
    │       ↓
    │   Output (Interactive/Table/Text/JSON)
    │
    └─→ Version Command
            ↓
        Get Version Info (lib.rs)
            ↓
        Format Output
            ↓
        Display (Table/JSON/Text)
```

---

## Development Guidelines

### Building and Testing

```bash
# Format and lint
make fmt            # rustfmt + clippy
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings

# Testing
make test           # Run all tests with verbose output
cargo test --verbose

# Building
make build          # Debug build
make release        # Release build with optimizations
cargo build --release

# Packaging
make package        # Create .crate package
cargo package --locked --allow-dirty
```

### Code Quality Standards

1. **Formatting**: Use `rustfmt` with default settings
2. **Linting**: All clippy warnings treated as errors (`-D warnings`)
3. **Error Handling**: Use `anyhow::Result` for functions, `thiserror` for custom errors
4. **Testing**: Unit tests for utilities, integration tests for workflows
5. **Documentation**: Rustdoc comments for all public APIs

### Performance Optimizations

**Release Profile** (`Cargo.toml:59-62`)
```toml
[profile.release]
lto = "thin"           # Link-Time Optimization
codegen-units = 1      # Single codegen unit for better optimization
strip = "symbols"      # Remove debug symbols
```

### Testing

**Test Coverage** (50+ tests)
- `tests/test_utils_time.rs`: Time parsing (6 tests)
- `tests/test_utils_paths.rs`: Path resolution (4 tests)
- `tests/test_utils_file.rs`: File I/O (10 tests)
- `tests/test_utils_git.rs`: Git operations (4 tests)
- `tests/test_models.rs`: Data models (12 tests)
- `tests/test_analysis_detector.rs`: Format detection (6 tests)
- `tests/test_integration_analysis.rs`: Analysis workflow (7 tests)
- `tests/test_integration_usage.rs`: Usage calculation (6 tests)

---

## Common Development Tasks

### Adding a New Command

1. **Update CLI** (`src/cli.rs`)
   ```rust
   #[derive(Subcommand, Debug)]
   pub enum Commands {
       // ... existing commands ...
       NewCommand {
           #[arg(long)]
           option: bool,
       },
   }
   ```

2. **Add Handler** (`src/main.rs`)
   ```rust
   match cli.command {
       // ... existing matches ...
       Commands::NewCommand { option } => {
           // Implementation
       }
   }
   ```

3. **Create Module** (`src/new_command.rs`)
4. **Export Module** (`src/lib.rs`)
5. **Add Tests** (`tests/test_new_command.rs`)

### Modifying Display Format

All display logic is in `src/usage/display.rs`:
- **Interactive**: Modify `display_usage_interactive()` and ratatui widgets
- **Table**: Modify `display_usage_table()` and comfy-table setup
- **Text**: Modify `display_usage_text()` format string

### Adding New Pricing Source

Modify `src/pricing.rs`:
1. Update `LITELLM_PRICING_URL` constant
2. Adjust `ModelPricing` struct if fields change
3. Update parsing in `fetch_model_pricing()`
4. Add tests for new pricing structure

### Debugging Tips

1. **Enable Logging**
   ```bash
   RUST_LOG=debug cargo run -- usage
   ```

2. **Check Cache**
   ```bash
   ls -la ~/.vibe-coding-tracker/
   cat ~/.vibe-coding-tracker/model_pricing_$(date +%Y-%m-%d).json
   ```

3. **Test with Examples**
   ```bash
   cargo run -- analysis --path examples/test_conversation.jsonl --output test.json
   cargo run -- analysis --path examples/test_conversation_oai.jsonl
   ```

4. **Validate JSONL Files**
   ```bash
   # Each line should be valid JSON
   cat examples/test_conversation.jsonl | while read line; do echo "$line" | jq .; done
   ```

---

## Go to Rust Translation Reference

For developers familiar with the original Go implementation:

| Go Function | Rust Implementation | Module |
|-------------|---------------------|--------|
| `analyzeConversations` | `analyze_claude_conversations` | `analysis::claude_analyzer` |
| `analyzeCodexConversations` | `analyze_codex_conversations` | `analysis::codex_analyzer` |
| `CalculateUsageFromJSONL` | `calculate_usage_from_jsonl` | `usage::calculator` |
| `GetUsageFromDirectories` | `get_usage_from_directories` | `usage::calculator` |
| `ReadJSONL` | `read_jsonl` | `utils::file` |
| `parseISOTimestamp` | `parse_iso_timestamp` | `utils::time` |
| `getGitRemoteOriginURL` | `get_git_remote_url` | `utils::git` |
| `detectExtensionType` | `detect_extension_type` | `analysis::detector` |
| `processClaudeUsageData` | `process_claude_usage_data` | `usage::calculator` |
| `processVibeCodingTrackerData` | `process_vibe_coding_tracker_data` | `usage::calculator` |
| `displayStaticTable` | `display_usage_table` | `usage::display` |

---

## Important Notes

### Session Directory Locations

- **Claude Code**: `~/.claude/projects/*.jsonl`
- **Codex**: `~/.codex/sessions/*.jsonl`

### Cache Management

- **Location**: `~/.vibe-coding-tracker/`
- **Pattern**: `model_pricing_YYYY-MM-DD.json`
- **Lifetime**: Daily (automatically cleaned)
- **Size**: ~500KB per file

### Cost Precision

**Critical Difference**: Cost rounding differs by output format:
- **Interactive/Table modes**: Costs rounded to **2 decimal places** for display (`${:.2}`)
- **Text mode**: Costs shown with **6 decimal places** (`${:.6}`)
- **JSON mode**: Costs stored with **full f64 precision** (no rounding)

When integrating with other systems or doing precise accounting, **always use JSON mode** to get full precision costs.

### Model Matching

The fuzzy matching algorithm is **case-insensitive** and uses **Jaro-Winkler similarity** with a threshold of **0.7**. This means:
- `claude-3-sonnet-20240229` matches `claude-3-sonnet`
- `gpt-4-turbo-preview` matches `gpt-4-turbo`
- Model names in parentheses indicate fuzzy matches: `custom-model (gpt-4)`

### Error Handling

- **Missing pricing**: Returns zero cost, not an error
- **Empty JSONL**: Returns empty result, not an error
- **Invalid JSON lines**: Skipped silently (logged with debug level)
- **Network failures**: Falls back to cache if available

---

## Troubleshooting

### Pricing Not Loading

```bash
# Check cache
ls -la ~/.vibe-coding-tracker/

# Clear cache and refetch
rm -rf ~/.vibe-coding-tracker/
cargo run -- usage

# Enable debug logging
RUST_LOG=debug cargo run -- usage 2>&1 | grep -i pricing
```

### Usage Not Showing

```bash
# Check session directories exist
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/

# Verify JSONL files
find ~/.claude/projects -name "*.jsonl" -exec wc -l {} \;
find ~/.codex/sessions -name "*.jsonl" -exec wc -l {} \;
```

### Analysis Fails

```bash
# Validate JSONL syntax
jq empty < examples/test_conversation.jsonl

# Check file permissions
ls -la examples/test_conversation.jsonl

# Test format detection
RUST_LOG=debug cargo run -- analysis --path examples/test_conversation.jsonl
```

### Interactive Mode Issues

```bash
# If terminal is broken after crash
reset

# Verify terminal supports required features
echo $TERM  # Should be xterm-256color or similar

# Test static table instead
cargo run -- usage --table
```

---

## CI/CD Integration

See `.github/workflows/` for:
- **test.yml**: Comprehensive testing with coverage
- **code-quality-check.yml**: Formatting and linting
- **build_release.yml**: Cross-platform binary releases
- **build_package.yml**: Cargo package creation
- **build_image.yml**: Docker image building

---

## License

MIT - see `LICENSE` file.
