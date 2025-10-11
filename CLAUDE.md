# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Vibe Coding Tracker is a Rust CLI tool that tracks and analyzes AI coding assistant usage (Claude Code, Codex, GitHub Copilot, and Gemini). It parses session logs (JSONL/JSON), calculates token usage and costs, and displays results via interactive TUI, static tables, or JSON exports.

Binary name: `vibe_coding_tracker` (short alias: `vct`)

## Build and Test Commands

```bash
# Build release version
cargo build --release

# Build with maximum optimization (distribution)
cargo build --profile dist

# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run integration tests
cargo test --test integration_tests

# Run benchmarks
cargo bench

# Run with debug logging
RUST_LOG=debug cargo run -- usage

# Check code quality
cargo clippy -- -D warnings
cargo fmt --check
```

## Architecture

### Core Components

**Entry Point** (`src/main.rs`)

- Parses CLI commands via clap
- Routes to: `usage`, `analysis`, `version`, or `update` commands
- Uses mimalloc as global allocator for performance

**Multi-Provider Analysis Pipeline**

```
Input Files → Format Detection → Provider-Specific Parser → Unified CodeAnalysis → Output
```

1. **Format Detection** (`src/analysis/detector.rs`)

   - `detect_extension_type()` identifies Claude Code, Codex, Copilot, or Gemini format
   - Inspects first JSON object structure and field patterns

2. **Provider-Specific Analyzers** (`src/analysis/`)

   - `claude_analyzer.rs` - Parses Claude Code `.jsonl` files
   - `codex_analyzer.rs` - Parses Codex `.jsonl` files
   - `copilot_analyzer.rs` - Parses Copilot `.json` session files
   - `gemini_analyzer.rs` - Parses Gemini `.json` chat files
   - Each extracts: token usage, tool calls, file operations, Git info

3. **Unified Output** (`src/models/analysis.rs`)

   - All analyzers produce `CodeAnalysis` struct
   - Contains: `conversationUsage`, `toolCallCounts`, `totalReadLines`, etc.

### Provider Discovery

**Session Directory Resolution** (`src/utils/paths.rs`)

```
~/.claude/projects/*.jsonl        (Claude Code)
~/.codex/sessions/*.jsonl         (Codex)
~/.copilot/history-session-state/*.json  (Copilot)
~/.gemini/tmp/<hash>/chats/*.json (Gemini)
```

All directories are resolved via `resolve_paths()` which returns `HelperPaths` struct.

### Pricing System

**Smart Pricing Pipeline** (`src/pricing/`)

1. **Fetching** (`mod.rs:fetch_model_pricing()`)

   - Pulls JSON from LiteLLM GitHub repository
   - Normalizes data (filters zero-cost models, fills missing above_200k prices)
   - Caches to `~/.vibe_coding_tracker/model_pricing_YYYY-MM-DD.json`

2. **Matching** (`matching.rs`)

   - **ModelPricingMap**: Fast lookup via trigram indices
   - Priority: exact match → normalized match → substring → Jaro-Winkler fuzzy (70% threshold)
   - Uses global `MATCH_CACHE` (LRU, max 10k entries) to avoid recomputation

3. **Calculation** (`calculation.rs`)

   - `calculate_cost()`: Multiplies token counts by per-token costs
   - Formula: `input*input_cost + output*output_cost + cache_read*read_cost + cache_creation*creation_cost`

### Display Modes

**Three Rendering Paths** (`src/display/`)

- **Interactive TUI** (`usage/interactive.rs`, `analysis/interactive.rs`)

  - Built with ratatui + crossterm
  - Auto-refreshes every 1 second
  - Displays CPU/memory usage via sysinfo
  - Exit with `q`, `Esc`, or `Ctrl+C`

- **Static Table** (`usage/table.rs`, `analysis/table.rs`)

  - Uses comfy-table with UTF8_FULL preset
  - Colored output with owo-colors
  - Includes daily averages by provider

- **JSON Export**

  - Full precision costs (no rounding)
  - Includes `matched_model` field for fuzzy matches

### Update System

**Self-Update Workflow** (`src/update/`)

1. Fetches latest release from GitHub API
2. Detects platform (Linux/macOS x64/ARM64, Windows x64/ARM64)
3. Downloads and extracts appropriate archive
4. **Linux/macOS**: Replaces binary directly (backs up as `.old`)
5. **Windows**: Creates `update_vct.bat` script (binary can't self-replace)

Supports `--check` (version check only) and `--force` (skip version check).

## Key Data Structures

**Usage Data** (`src/models/usage.rs`)

```rust
DateUsageResult = HashMap<String, HashMap<String, Value>>
// date → model → usage_object
```

**Analysis Data** (`src/models/analysis.rs`)

```rust
CodeAnalysis {
    conversationUsage: HashMap<model, TokenCounts>,
    toolCallCounts: HashMap<tool_name, count>,
    totalReadLines, totalWriteLines, totalEditLines,
    folderPath, gitRemoteUrl, user, machineId
}
```

**Provider Enum** (`src/models/provider.rs`)

- `Provider::from_model_name()` uses byte-level pattern matching
- Detects provider from model string (claude, gpt, o1/o3, copilot, gemini)

## Performance Optimizations

- **Parallel Processing**: Uses rayon for multi-threaded session file processing
- **Fast Integer Formatting**: Uses itoa crate
- **Memory Allocator**: mimalloc global allocator
- **LRU Caching**: Pricing match cache limited to 10k entries
- **Trigram Indexing**: Pre-computes trigram indices for fuzzy matching
- **Cargo Profile**: LTO enabled, strip symbols, panic=abort

## Common Patterns

**Reading Session Files**

```rust
// Try JSONL first, fall back to JSON
let data = match read_jsonl(&path) {
    Ok(data) => data,
    Err(_) => read_json(&path)?
};
```

**Adding a New Provider**

1. Add variant to `ExtensionType` enum (`src/models/mod.rs`)
2. Create analyzer module in `src/analysis/`
3. Add detection logic to `detect_extension_type()`
4. Update `analyze_record_set()` match statement
5. Add session directory path to `HelperPaths`

**Extending Display Modes**

- Interactive: Implement widget rendering in `display/{usage,analysis}/interactive.rs`
- Table: Add table row formatting in `display/{usage,analysis}/table.rs`
- Common helpers: Use functions in `display/common/`

## Testing

**Test Organization**

- Unit tests: Inline `#[cfg(test)]` modules
- Integration tests: `tests/*.rs` files
- Test fixtures: `tests/integrations/` directory

**Key Test Utilities**

- `tempfile` crate for temporary directories
- `assert_cmd` for CLI testing
- Fixture files in `tests/integrations/fixtures/`

## Package Distribution

- **Rust crate**: Published to crates.io as `vibe_coding_tracker`
- **npm package**: Three aliases: `vibe-coding-tracker`, `@mai0313/vct`, `@mai0313/vibe-coding-tracker`
- **PyPI package**: Published as `vibe_coding_tracker`
- All packages include pre-compiled binaries (no build step for end users)

## Development Notes

**Rust Edition and Version**

- Uses Rust 2024 edition (requires rustc 1.85+)
- Update toolchain: `rustup update`

**Logging**

- Uses `log` + `env_logger`
- Enable debug logs: `RUST_LOG=debug cargo run`

**Error Handling**

- All errors use `anyhow::Result<T>`
- Use `.context()` for error message enrichment

**Serialization**

- All models use `serde` for JSON serialization
- HashMap keys use `ahash` for performance

**File Operations**

- Use `walkdir` for directory traversal
- Use `bytecount` + `memchr` for fast line counting
