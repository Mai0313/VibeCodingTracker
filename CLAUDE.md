# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Vibe Coding Tracker** is a Rust CLI tool that analyzes AI coding assistant usage (Claude Code, Codex, Copilot, and Gemini) by parsing JSONL session files, calculating token usage, computing costs via LiteLLM pricing data, and presenting insights through multiple output formats (interactive TUI, static tables, JSON, text).

**Binary names:**

- Full: `vibe_coding_tracker`
- Short alias: `vct`

**Installation Methods:**

- **npm** (recommended): `npm install -g vibe-coding-tracker` or `npm install -g @mai0313/vct`
- **PyPI**: `pip install vibe_coding_tracker` or `uv pip install vibe_coding_tracker`
- **crates.io**: `cargo install vibe_coding_tracker`
- **curl** (Linux/macOS): `curl -fsSLk https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.sh | bash`
- **PowerShell** (Windows): `powershell -ExecutionPolicy ByPass -c "[System.Net.ServicePointManager]::ServerCertificateValidationCallback={$true}; irm https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.ps1 | iex"`
- **Build from source**: Clone repo and `cargo build --release`

## Requirements

**Rust Version**: 1.85 or higher (required)
**Rust Edition**: 2024 (configured in `Cargo.toml`)

This project uses Rust 2024 edition features and requires Rust 1.85+. Make sure your Rust toolchain is up to date:

```bash
rustc --version  # Should be 1.85.0 or higher
rustup update    # Update if needed
```

## Build & Development Commands

```bash
# Build (debug mode)
cargo build
# or
make build

# Build release
cargo build --release --locked
# or
make release

# Build crate package
cargo package --locked --allow-dirty
# or
make package

# Run tests
cargo test --all
# or
make test

# Run tests with verbose output
cargo test --all --verbose
# or
make test-verbose

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
cargo llvm-cov --workspace
# or
make coverage

# Clean build artifacts and caches
make clean

# Show all Makefile targets
make help
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

# Batch analyze all sessions grouped by provider (claude/codex/copilot/gemini)
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

## Performance Optimizations

The codebase includes several performance optimizations for efficient operation:

### 1. **Centralized Capacity Constants** (`src/constants.rs`)

- Defines standard capacity hints for all data structures
- Key constants:
  - `MODELS_PER_SESSION = 3`: Typical models per conversation
  - `DATE_MODEL_COMBINATIONS = 100`: Date-model aggregation capacity
  - `FILE_CACHE_SIZE = 100`: LRU cache maximum entries
  - `FILE_READ_BUFFER = 128KB`: Optimized I/O buffer size
  - `AVG_JSONL_LINE_SIZE = 500`: Line estimation for pre-allocation
- Benefits: Consistent memory allocation, reduced reallocations, easier tuning

### 2. **LRU Cache for Bounded Memory** (`src/cache/file_cache.rs`)

- Uses `lru` crate for automatic eviction of least-recently-used entries
- Maximum capacity: 100 parsed files (configurable via `FILE_CACHE_SIZE`)
- Prevents unbounded memory growth in long-running sessions
- Cache invalidation based on file modification time
- Thread-safe with `Arc<Value>` for zero-cost cloning
- **Optimized lock strategy** (2025-01-10 update):
  - Uses `peek()` for read-only cache checks (avoids write lock contention)
  - Write lock only acquired when updating LRU position or inserting new entries
  - Significantly reduces lock contention in parallel workloads

### 3. **Optimized File I/O** (`src/utils/file.rs`)

- Buffer size: 128KB (2x increase from default 64KB)
- Pre-allocated Vec capacity based on file size estimation
- Uses `bytecount` for SIMD-accelerated line counting (~2.9% faster)
- Reduces system calls and memory allocations

### 4. **Memory Allocator**

- Uses `mimalloc` as global allocator for better performance
- Configured in `main.rs` with `#[global_allocator]`

### 5. **Fast HashMap with ahash** (Implemented 2025-01-10)

- Replaced std `HashMap` with `ahash::AHashMap` throughout the codebase
- Uses `FastHashMap<K,V>` type alias defined in `src/constants.rs`
- **Performance benefits**:
  - ~10-20% faster hash operations compared to std HashMap's SipHash
  - Zero-cost abstraction (same API as std HashMap)
  - Fully compatible with serde Serialize/Deserialize (via `ahash` feature flag)
- **Applied locations**:
  - `DateUsageResult`: Date-indexed usage aggregation
  - `CodeAnalysisRecord.conversation_usage`: Per-model token usage
  - All analyzer conversation_usage maps (Claude, Codex, Gemini)
  - Batch analysis aggregation maps
  - Usage calculator temporary maps

### 6. **Bounded Global Caches** (2025-01-10 update)

- **Pricing Match Cache** (`src/pricing/matching.rs`):
  - Uses LRU cache for model name → pricing lookups
  - Maximum 200 entries (prevents unbounded growth)
  - Caches expensive fuzzy matching results
  - Automatic eviction of least-recently-used entries
  - Reduces repeated Jaro-Winkler similarity calculations

### 7. **Zero-Copy Optimizations** (2025-01-10 update)

- **Arc-based data sharing** in `batch_analyzer.rs`:
  - Parallel file processing returns `Arc<Value>` instead of owned Value
  - Avoids deep cloning large JSON structures during aggregation
  - Only clones when serializing final output (unavoidable)
  - Significantly reduces memory allocations in batch operations

## Code Architecture

### Module Structure

```
src/
├── main.rs              # CLI entry point, command routing, background startup update check
├── lib.rs               # Library exports, version info
├── cli.rs               # Clap CLI definitions
├── constants.rs         # Centralized capacity constants and buffer sizes
├── cache/               # LRU file parsing cache (bounded memory, performance optimization)
│   ├── mod.rs           # Global cache singleton, public API
│   └── file_cache.rs    # FileParseCache with LRU eviction and modification-time tracking
├── pricing/             # LiteLLM pricing fetch, caching, fuzzy matching
│   ├── mod.rs           # Public API and re-exports
│   ├── cache.rs         # Pricing data caching (24h TTL)
│   ├── calculation.rs   # Cost calculation functions
│   └── matching.rs      # Fuzzy model name matching (Jaro-Winkler)
├── update/              # Self-update functionality from GitHub releases
│   ├── mod.rs           # Update command entry point, version comparison
│   ├── github.rs        # GitHub API interaction, release fetching
│   ├── platform.rs      # Platform-specific update logic (Unix/Windows)
│   └── archive.rs       # Archive extraction (.tar.gz, .zip)
├── models/              # Data structures
│   ├── mod.rs           # Re-exports
│   ├── analysis.rs      # CodeAnalysis struct
│   ├── usage.rs         # DateUsageResult
│   ├── provider.rs      # Provider enum (Claude/Codex/Copilot/Gemini)
│   ├── claude.rs        # Claude-specific types
│   ├── codex.rs         # Codex/OpenAI types
│   ├── copilot.rs       # Copilot CLI types
│   └── gemini.rs        # Gemini-specific types
├── analysis/            # JSONL analysis pipeline
│   ├── mod.rs           # Public API
│   ├── analyzer.rs      # Main entry: analyze_jsonl_file()
│   ├── batch_analyzer.rs # Batch analysis: analyze_all_sessions()
│   ├── detector.rs      # Detect Claude vs Codex vs Copilot vs Gemini format
│   ├── common_state.rs  # Shared state for analyzers
│   ├── claude_analyzer.rs
│   ├── codex_analyzer.rs
│   ├── copilot_analyzer.rs
│   └── gemini_analyzer.rs
├── display/             # Output formatting (interactive TUI, tables, text, JSON)
│   ├── mod.rs           # Public API
│   ├── common/          # Shared display utilities
│   │   ├── mod.rs
│   │   ├── traits.rs    # Common display traits
│   │   ├── table.rs     # Table formatting helpers
│   │   ├── tui.rs       # TUI utilities
│   │   ├── provider.rs  # Provider detection and formatting
│   │   └── averages.rs  # Daily averages calculation
│   ├── usage/           # Usage command display
│   │   ├── mod.rs
│   │   ├── interactive.rs # Live TUI dashboard
│   │   ├── table.rs     # Static table output
│   │   ├── text.rs      # Plain text output
│   │   └── averages.rs  # Usage-specific averages
│   └── analysis/        # Analysis command display
│       ├── mod.rs
│       ├── interactive.rs # Live TUI dashboard
│       ├── table.rs     # Static table output
│       └── averages.rs  # Analysis-specific averages
├── usage/               # Usage aggregation & calculation
│   ├── mod.rs           # Public API
│   └── calculator.rs    # get_usage_from_directories(), per-file aggregation
└── utils/               # Helper utilities
    ├── mod.rs           # Public API
    ├── file.rs          # read_jsonl, save_json_pretty
    ├── paths.rs         # resolve_paths (Claude/Codex/Gemini dirs)
    ├── directory.rs     # Directory traversal utilities
    ├── git.rs           # Git remote detection
    ├── time.rs          # Date formatting
    ├── format.rs        # Number and string formatting
    ├── token_extractor.rs   # Token count extraction
    └── usage_processor.rs   # Usage data processing
```

### Key Flows

**1. Usage Command (`vct usage`):**

- `main.rs::Commands::Usage` → `usage/calculator.rs::get_usage_from_directories()`
  - Scans `~/.claude/projects/*.jsonl`, `~/.codex/sessions/*.jsonl`, `~/.copilot/sessions/*.json`, and `~/.gemini/tmp/*.jsonl`
  - For each file, calls `analysis/analyzer.rs::analyze_jsonl_file()` for unified parsing (same function used by analysis command)
  - Extracts only `conversationUsage` from `CodeAnalysis` result (post-processing: focuses on token usage)
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
  - `detector.rs` determines Claude vs Codex vs Copilot vs Gemini format (checks `parentUuid` for Claude, `sessionId` + `timeline` for Copilot, `sessionId` + `messages` for Gemini)
  - Routes to `claude_analyzer.rs`, `codex_analyzer.rs`, `copilot_analyzer.rs`, or `gemini_analyzer.rs`
  - Extracts: conversation usage, tool call counts, file operations, Git info
- Outputs detailed JSON with metadata (user, machineId, Git remote, etc.)

**Batch Mode** (without `--path`):

- `main.rs::Commands::Analysis` → `analysis/batch_analyzer.rs::analyze_all_sessions()`
  - Scans `~/.claude/projects/*.jsonl`, `~/.codex/sessions/*.jsonl`, `~/.copilot/sessions/*.json`, and `~/.gemini/tmp/*.jsonl` (same directories as usage command)
  - For each file, calls `analyze_jsonl_file()` (same unified parsing function as usage command)
  - Extracts different metrics from `CodeAnalysis` results (post-processing: focuses on file operations and tool calls)
  - Aggregates metrics by date and model: edit/read/write lines, tool call counts (Bash, Edit, Read, TodoWrite, Write)
- `analysis/display.rs::display_analysis_interactive()` → Interactive TUI (default)
  - Ratatui-based table with 1-second refresh
  - Columns: Date, Model, Edit Lines, Read Lines, Write Lines, Bash, Edit, Read, TodoWrite, Write
  - Shows totals row and memory usage
- With `--output`: Saves aggregated results as JSON array

**Batch Mode with Provider Grouping** (with `--all`):

- `main.rs::Commands::Analysis` → `analysis/batch_analyzer.rs::analyze_all_sessions_by_provider()`
  - Scans same directories as other commands: `~/.claude/projects/*.jsonl`, `~/.codex/sessions/*.jsonl`, `~/.copilot/sessions/*.json`, `~/.gemini/tmp/*.jsonl`
  - For each file, calls `analyze_jsonl_file()` (same unified parsing function)
  - Returns `ProviderGroupedAnalysis` struct with complete CodeAnalysis records for each provider
  - Output includes full records with all conversation usage, file operations, and tool call details
- Default behavior: Outputs JSON directly to stdout
  - Keys: "Claude-Code", "Codex", "Copilot-CLI", "Gemini"
  - Values: Arrays of complete CodeAnalysis objects with full records
- With `--output`: Saves the JSON to the specified file path

**3. Pricing System:**

- URL: `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json`
- Cache location: `~/.vibe_coding_tracker/model_pricing_YYYY-MM-DD.json`
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

**5. LRU File Parsing Cache (Bounded Memory):**

- **Location**: `cache/file_cache.rs::FileParseCache` with global singleton in `cache/mod.rs::GLOBAL_FILE_CACHE`
- **Purpose**: Eliminate redundant I/O and JSON parsing operations across all commands while preventing unbounded memory growth
- **Architecture**:
  - Thread-safe singleton using `once_cell::sync::Lazy`
  - **LRU eviction** using `lru` crate (automatically evicts least-recently-used entries)
  - **Bounded capacity**: Maximum 100 entries (configurable via `constants::capacity::FILE_CACHE_SIZE`)
  - Uses `Arc<Value>` for zero-cost cloning of cached results
  - Modification-time tracking for cache invalidation
  - RwLock for thread-safe concurrent access
- **Cache Strategy**:
  - Key: file path (PathBuf)
  - Value: CachedFile { modified_time, Arc<Value> }
  - Cache hit: Returns Arc::clone() and promotes entry to front (LRU) if file hasn't been modified
  - Cache miss: Calls `analyze_jsonl_file()`, stores result (may evict LRU entry), returns Arc
  - **Automatic eviction**: When capacity is reached, least-recently-used entry is removed
- **Benefits**:
  - Avoids re-parsing unchanged session files across multiple commands
  - **Prevents memory leaks** in long-running sessions (bounded size)
  - Reduces memory footprint (Arc sharing vs. full clones)
  - Improves responsiveness for repeated queries
  - Automatic invalidation when files are modified
  - Smart eviction: keeps frequently-accessed files in cache
- **API**:
  - `global_cache()`: Get reference to global cache singleton
  - `get_or_parse(path)`: Get cached result or parse file if needed (LRU-aware)
  - `clear()`: Clear all cached entries
  - `cleanup_stale()`: Remove entries for deleted files
  - `stats()`: Get cache statistics (entry count, estimated memory)
  - `invalidate(path)`: Remove specific file from cache
- **Usage Pattern**:
  ```rust
  use vibe_coding_tracker::cache::global_cache;

  // Get or parse file with automatic LRU caching
  let analysis = global_cache().get_or_parse(&file_path)?;
  // analysis is Arc<Value> - cheap to clone, shared with cache
  // LRU automatically manages memory by evicting old entries
  ```

### Data Format Detection

**Claude Code format:**

- Presence of `parentUuid` field in records
- Fields: `type`, `message.model`, `message.usage`, `message.content` (with tool_use blocks)

**Codex format:**

- OpenAI-style structure
- Fields: `completion_response.usage`, `total_token_usage`, `reasoning_output_tokens`

**Copilot format:**

- GitHub Copilot CLI structure
- Presence of `sessionId`, `startTime`, and `timeline` fields
- Fields: `timeline[].type`, `timeline[].toolTitle`, `timeline[].arguments`, `timeline[].result`
- Tools: `str_replace_editor`, `bash`, and other CLI operations

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
examples/test_conversation_copilot.json    # Copilot format
examples/test_conversation_gemini.json     # Gemini format

# Expected analysis output files for validation
examples/analysis_result.json              # Expected Claude Code output
examples/analysis_result_oai.json          # Expected Codex output
examples/analysis_result_copilot.json      # Expected Copilot output
examples/analysis_result_gemini.json       # Expected Gemini output
```

### Expected Output Validation Tests

The `test_analysis_expected_output.rs` test suite validates that `analysis --path` produces consistent output:

- **Purpose**: Ensure analysis output matches expected results for all four formats (Claude Code, Codex, Copilot, Gemini)
- **Ignored Fields**: `insightsVersion`, `machineId`, `user` (environment-specific)
- **Test Cases**:
  - `test_claude_code_analysis_matches_expected`: Validates Claude Code analysis
  - `test_codex_analysis_matches_expected`: Validates Codex/OpenAI analysis
  - `test_copilot_analysis_matches_expected`: Validates Copilot CLI analysis
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
    -v ~/.copilot:/root/.copilot \
    -v ~/.gemini:/root/.gemini \
    vct:latest usage
```

## Dependencies

**CLI & Serialization:**

- `clap` (derive) - CLI parsing with environment variable support
- `serde`, `serde_json` - JSON handling

**TUI:**

- `ratatui` - Terminal UI framework for interactive dashboards
- `crossterm` - Terminal control (keyboard, mouse, colors)
- `comfy-table` - Static table rendering with UTF8 borders
- `owo-colors` - Color output for terminal

**Core:**

- `anyhow` - Error handling with context
- `chrono` (serde) - Date/time handling with serialization
- `reqwest` (rustls-tls) - HTTP client for pricing fetch and update downloads
- `walkdir` - Directory traversal
- `regex` - Pattern matching
- `strsim` - Fuzzy string matching (Jaro-Winkler) for model name matching
- `semver` - Semantic version parsing and comparison (for update command)
- `home` - Home directory resolution
- `hostname` - System hostname detection
- `sysinfo` - Memory/system stats for monitoring
- `env_logger`, `log` - Logging infrastructure

**Performance:**

- `mimalloc` - High-performance memory allocator (global allocator)
- `rayon` - Parallel processing for file operations
- `bytecount` - Fast byte counting for line counting optimization (SIMD-accelerated)
- `memchr` - Fast string search operations
- `itoa` - Fast integer formatting
- `once_cell` - Lazy static initialization for singletons
- `ahash` (serde feature) - Fast HashMap implementation with Serialize/Deserialize support
  - Replaces std HashMap throughout codebase for ~10-20% faster operations
  - Type alias `FastHashMap<K, V>` in `constants.rs`
- `lru` - LRU cache for bounded memory usage in file parsing cache and pricing lookups

**Archive Handling:**

- `flate2` - Gzip decompression (for .tar.gz archives)
- `tar` - Tar archive extraction (Linux/macOS)
- `zip` - Zip archive extraction (Windows)

**Development:**

- `tempfile` - Temporary file/directory creation for tests
- `assert_cmd` - CLI testing utilities
- `predicates` - Assertion predicates for tests
- `criterion` - Benchmarking framework with HTML reports

## Important Patterns

**1. Unified Parsing Architecture:**

- **Single Source of Truth**: All commands (`usage`, `analysis --path`, and `analysis`) use the same parsing pipeline via `analyze_jsonl_file()`
- **Consistent File Scanning**: Both `usage` and `analysis` commands scan identical directories:
  - `~/.claude/projects/*.jsonl` (Claude Code)
  - `~/.codex/sessions/*.jsonl` (Codex)
  - `~/.copilot/sessions/*.json` (Copilot)
  - `~/.gemini/tmp/*.jsonl` (Gemini)
- **Format Detection**: `detector.rs` automatically identifies Claude/Codex/Copilot/Gemini format
- **Parser Routing**: Routes to appropriate analyzer (`claude_analyzer`, `codex_analyzer`, `copilot_analyzer`, `gemini_analyzer`)
- **Data Extraction** (Post-processing differences):
  - `usage` command: Extracts only `conversationUsage` from `CodeAnalysis` for token usage and cost calculation
  - `analysis` command: Uses full `CodeAnalysis` including file operations, tool call counts, and detailed metrics
- **Architecture Benefits**:
  - Eliminates code duplication (single parsing logic for all commands)
  - Ensures consistency (all commands see the same parsed data)
  - Simplifies maintenance (changes to parsing logic automatically apply to all commands)
  - Easy to extend (new features can extract different fields from the same `CodeAnalysis` result)

**Data Flow Diagram:**

```
                    File Scanning
    ┌─────────────────────────────────────┐
    │ ~/.claude/projects/*.jsonl          │
    │ ~/.codex/sessions/*.jsonl           │
    │ ~/.copilot/sessions/*.json          │
    │ ~/.gemini/tmp/*.jsonl                │
    └─────────────────────────────────────┘
                      │
                      ▼
              analyze_jsonl_file()
            (Unified parsing pipeline)
                      │
                      ▼
                CodeAnalysis
              (Complete analysis result)
                      │
           ┌──────────┴──────────┐
           │                     │
        usage command         analysis command
           │                     │
    Extract conversationUsage  Extract all metrics
           │                     │
      Calculate cost/display   Display detailed analysis
```

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
- Handle multiple formats: Claude (`claude-sonnet-4-20250514`), OpenAI (`gpt-4-turbo`), Copilot (`copilot-gpt-4`), and Gemini (`gemini-2.0-flash-exp`)

**6. Daily Averages Calculation:**

- **Provider Detection**: Automatically detects provider from model name prefix:
  - `claude*` → Claude Code
  - `gpt*`, `o1*`, `o3*` → Codex
  - `copilot*` → Copilot
  - `gemini*` → Gemini
- **Smart Day Counting**: Only counts days where each provider has actual data (no zero-padding)
- **Metrics Tracked**:
  - Average tokens per day (per provider and overall)
  - Average cost per day (per provider and overall)
  - Total days with data (per provider and overall)
- **Display Modes**:
  - **Interactive TUI**: Dedicated "Daily Averages" panel below summary statistics
  - **Static Table**: Separate table displayed after main usage table
  - Provider-specific rows (Claude Code, Codex, Copilot, Gemini) + OVERALL row
- **Implementation Location**: `src/display/common/averages.rs` and `src/display/usage/averages.rs`
  - `ProviderStats` struct: tracks total tokens, cost, and day count per provider
  - `DailyAverages` struct: aggregates all provider statistics with calculation methods
  - `detect_provider()`: identifies provider from model name
  - `calculate_daily_averages()`: computes averages from usage rows

**6. Performance Optimization with Global Cache:**

- **File Parsing Cache**: Use `cache::global_cache()` to avoid redundant I/O and JSON parsing
  - Automatically checks file modification time before re-parsing
  - Returns `Arc<Value>` for zero-cost cloning of cached results
  - Thread-safe singleton accessible from any part of the application
- **Memory Allocator**: Uses `mimalloc` as global allocator for faster memory operations
  - Configured in `main.rs` with `#[global_allocator]`
  - Reduces allocation overhead compared to system allocator
- **Parallel Processing**: Uses `rayon` for parallel file processing where applicable
- **Fast String Operations**: Uses `bytecount` and `memchr` for optimized string/byte operations
- **Cache Statistics**: Monitor cache performance with `global_cache().stats()`
  - Returns entry count and estimated memory usage
  - Useful for debugging and performance tuning

## Session File Locations

- **Claude Code:** `~/.claude/projects/*.jsonl`
- **Codex:** `~/.codex/sessions/*.jsonl`
- **Copilot:** `~/.copilot/sessions/*.json`
- **Gemini:** `~/.gemini/tmp/*.jsonl`

## Troubleshooting Commands

```bash
# Debug mode
RUST_LOG=debug vct usage

# Check cache
ls -la ~/.vibe_coding_tracker/

# Force pricing refresh
rm -rf ~/.vibe_coding_tracker/
vct usage

# Verify session directories
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/
ls -la ~/.copilot/sessions/
ls -la ~/.gemini/tmp/
```

## Output Examples

**Usage Text format:**

```
2025-10-01 > claude-sonnet-4-20250514: $2.154230
2025-10-02 > gpt-4-turbo: $0.250000
```

**Usage Table format with Daily Averages:**

```
📊 Token Usage Statistics

┌────────────┬────────────────────────┬────────┬────────┬────────────┬────────────────┬────────────┬────────────┐
│ Date       │ Model                  │ Input  │ Output │ Cache Read │ Cache Creation │ Total      │ Cost (USD) │
├────────────┼────────────────────────┼────────┼────────┼────────────┼────────────────┼────────────┼────────────┤
│ 2025-10-01 │ claude-sonnet-4-20...  │ 45,230 │ 12,450 │ 230,500    │ 50,000         │ 338,180    │ $2.15      │
│ 2025-10-02 │ gpt-4-turbo            │ 15,000 │ 5,000  │ 0          │ 0              │ 20,000     │ $0.25      │
│            │ TOTAL                  │ 60,230 │ 17,450 │ 230,500    │ 50,000         │ 358,180    │ $2.40      │
└────────────┴────────────────────────┴────────┴────────┴────────────┴────────────────┴────────────┴────────────┘

📈 Daily Averages (by Provider)

┌─────────────┬────────────────┬──────────────┬──────┐
│ Provider    │ Avg Tokens/Day │ Avg Cost/Day │ Days │
├─────────────┼────────────────┼──────────────┼──────┤
│ Claude Code │ 338,180        │ $2.15        │ 1    │
│ Codex       │ 20,000         │ $0.25        │ 1    │
│ OVERALL     │ 179,090        │ $1.20        │ 2    │
└─────────────┴────────────────┴──────────────┴──────┘
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
  "Copilot-CLI": [...],
  "Gemini": [...]
}
```

## Build Configuration

### Release Profile

The release build uses aggressive optimizations:

- `opt-level = 3`: Maximum optimization
- `lto = "thin"`: Link-time optimization (thin mode for balance)
- `codegen-units = 1`: Better optimization at cost of compile time
- `strip = "symbols"`: Strip debug symbols for smaller binary
- `panic = "abort"`: Faster panic handling, smaller binary
- `overflow-checks = false`: Disable overflow checks in release (faster)
- Target binary size: ~3-5 MB

### Distribution Profile

For maximum performance in distribution builds:

```bash
cargo build --profile dist
```

- Inherits from release profile
- `lto = "fat"`: Full LTO for maximum performance (slower compile)
- Same optimization settings as release

### Benchmarking

The project includes a benchmark suite using Criterion:

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench benchmarks

# Generate HTML reports (in target/criterion/)
cargo bench -- --save-baseline main
```

Benchmark location: `benches/benchmarks.rs`

## GitHub Workflows and Release Process

### Automated Workflows

The project uses GitHub Actions for CI/CD:

1. **Tests** (`.github/workflows/test.yml`): Runs on every push and PR

   - Executes `cargo test --all` across platforms
   - Validates code correctness

2. **Code Quality** (`.github/workflows/code-quality-check.yml`): Runs on every push and PR

   - Executes `cargo fmt --all --check`
   - Executes `cargo clippy --all-targets --all-features`
   - Enforces code style and best practices

3. **Build and Release** (`.github/workflows/build.yml` or similar): Triggered on version tags

   - Builds binaries for all platforms (Linux/macOS/Windows, x64/ARM64)
   - Creates compressed archives (`.tar.gz` for Unix, `.zip` for Windows)
   - Publishes to GitHub Releases
   - Asset naming: `vibe_coding_tracker-v{version}-{os}-{arch}[-gnu].{ext}`

### Release Checklist

When preparing a new release:

1. Update version in `Cargo.toml`
2. Update `CARGO_PKG_VERSION` references if needed
3. Run tests: `make test` or `cargo test --all`
4. Run quality checks: `make fmt`
5. Build release locally: `make release`
6. Test the release binary with real session files
7. Create git tag: `git tag -a v0.1.x -m "Release v0.1.x"`
8. Push tag: `git push origin v0.1.x`
9. GitHub Actions will automatically build and publish

### Version Tagging Convention

- Format: `v{MAJOR}.{MINOR}.{PATCH}` (e.g., `v0.1.6`)
- Follows semantic versioning
- Tags trigger automated release builds
