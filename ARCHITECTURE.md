# Architecture Documentation

This document provides a comprehensive overview of the Vibe Coding Tracker's architecture, design patterns, and system organization.

## Table of Contents

- [System Overview](#system-overview)
- [Module Architecture](#module-architecture)
- [Data Flow](#data-flow)
- [Key Components](#key-components)
- [Design Patterns](#design-patterns)
- [External Dependencies](#external-dependencies)
- [Performance Considerations](#performance-considerations)

## System Overview

Vibe Coding Tracker is a Rust-based CLI tool designed to analyze AI coding assistant usage across multiple platforms (Claude Code, Codex, and Gemini). The system employs a modular architecture with clear separation of concerns:

```
┌─────────────────────────────────────────────────────────────┐
│                        CLI Layer                             │
│              (clap-based command routing)                    │
└────────────┬────────────────────────────────┬───────────────┘
             │                                │
             ▼                                ▼
┌────────────────────────┐      ┌────────────────────────────┐
│   Usage Analysis       │      │   Conversation Analysis    │
│   - calculator.rs      │      │   - analyzer.rs            │
│   - display.rs         │      │   - batch_analyzer.rs      │
└────────┬───────────────┘      └────────────┬───────────────┘
         │                                   │
         │         ┌─────────────────────────┤
         │         │                         │
         ▼         ▼                         ▼
┌─────────────────────┐          ┌──────────────────────────┐
│   Pricing System    │          │   Format Detection       │
│   - fetch_model_    │          │   - Claude/Codex/Gemini  │
│     pricing()       │          │   - detector.rs          │
│   - fuzzy matching  │          │   - claude_analyzer.rs   │
│   - caching         │          │   - codex_analyzer.rs    │
└─────────────────────┘          │   - gemini_analyzer.rs   │
                                 └──────────────────────────┘
         │                                   │
         └───────────┬───────────────────────┘
                     ▼
         ┌───────────────────────┐
         │   Data Models         │
         │   - DateUsageResult   │
         │   - CodeAnalysis      │
         │   - Claude/Codex/     │
         │     Gemini            │
         └───────────────────────┘
                     │
                     ▼
         ┌───────────────────────┐
         │   Output Layer        │
         │   - TUI (Ratatui)     │
         │   - Table (comfy)     │
         │   - JSON/Text         │
         └───────────────────────┘
```

## Module Architecture

### Core Modules

#### 1. Entry Points (`main.rs`, `lib.rs`, `cli.rs`)

**main.rs**

- Application entry point
- Command routing to usage/analysis pipelines
- Error handling and exit codes

**lib.rs**

- Library exports for external use
- Version information (`get_version()`)
- Public API surface

**cli.rs**

- Clap-based CLI definitions
- Commands: `usage`, `analysis`, `version`
- Argument parsing and validation

#### 2. Models Module (`src/models/`)

Defines core data structures with serde serialization:

**usage.rs**

- `DateUsageResult`: Date-indexed usage map (maps date → model → usage data)

**analysis.rs**

- `CodeAnalysis`: Comprehensive conversation metadata
  - User info (username, hostname, machineId)
  - Git metadata (remote URL, commit hash)
  - Token usage breakdown
  - Tool call statistics
  - File operation counts

**claude.rs**

- `ClaudeCodeLog`: Claude Code session log format

**codex.rs**

- `CodexMessage`: OpenAI/Codex message format
- `CompletionResponse`: Response with usage data
- `ReasoningTokens`: Reasoning output token tracking

**gemini.rs**

- `GeminiMessage`: Gemini message format
- `GeminiUsage`: Token usage tracking for Gemini
- `GeminiContent`: Content types for Gemini API

#### 3. Pricing System (`pricing.rs`)

**Responsibilities:**

- Fetch LiteLLM pricing data from GitHub
- Cache pricing with 24-hour TTL (daily cache files)
- Fuzzy model name matching (Jaro-Winkler algorithm)

**Matching Strategy (Priority Order):**

1. **Exact match**: Direct string equality
2. **Normalized match**: Strip version suffixes (e.g., `-20250514`)
3. **Substring match**: Check if model name contains pricing key
4. **Fuzzy match**: Jaro-Winkler similarity ≥ 0.7 threshold
5. **Fallback**: Return $0.00 if no match found

**Cache Location:**

```
~/.vibe-coding-tracker/model_pricing_YYYY-MM-DD.json
```

**Cost Calculation:**

```rust
cost = (input × input_cost_per_token) +
       (output × output_cost_per_token) +
       (cache_read × cache_read_cost_per_token) +
       (cache_creation × cache_creation_cost_per_token)
```

#### 4. Usage Analysis Module (`src/usage/`)

**calculator.rs**

- `get_usage_from_directories()`: Main aggregation function
  - Scans `~/.claude/projects/*.jsonl`
  - Scans `~/.codex/sessions/*.jsonl`
  - Scans `~/.gemini/tmp/<project_hash>/chats/*.json`
  - Aggregates tokens by date and model
  - Calculates costs via pricing system

**display.rs**

- `display_usage_interactive()`: Ratatui-based TUI with 1s refresh
- `display_usage_table()`: Static comfy-table output (UTF8_FULL preset)
- `display_usage_text()`: Plain text format (`Date > model: cost`)
- `display_usage_json()`: Full-precision JSON output

#### 5. Analysis Module (`src/analysis/`)

**analyzer.rs**

- `analyze_jsonl_file()`: Single file analysis entry point
- Routes to Claude/Codex analyzers based on detection

**batch_analyzer.rs**

- `analyze_all_sessions()`: Batch processing for all sessions
- Aggregates metrics by date and model:
  - Edit/Read/Write line counts
  - Tool call counts (Bash, Edit, Read, TodoWrite, Write)

**detector.rs**

- `detect_format()`: Determines Claude, Codex, or Gemini format
- Detection logic: Checks for `parentUuid` field (Claude-specific)

**claude_analyzer.rs**

- Parses Claude Code JSONL format
- Extracts:
  - Tool usage (tool_use blocks in content)
  - File operations (Edit, Read, Write line counts)
  - Git metadata
  - Conversation metrics

**codex_analyzer.rs**

- Parses Codex/OpenAI JSONL format
- Extracts:
  - Token usage from `completion_response.usage`
  - Reasoning tokens
  - Total token usage

**gemini_analyzer.rs**

- Parses Gemini JSON format
- Extracts:
  - Token usage from session messages
  - Message content and metadata
  - Session-level statistics

**display.rs**

- `display_analysis_interactive()`: Ratatui TUI for batch analysis
  - Columns: Date, Model, Edit Lines, Read Lines, Write Lines, Tool Counts
  - Auto-refresh, totals row, memory usage
- `display_analysis_table()`: Static table output

#### 6. Utilities Module (`src/utils/`)

**file.rs**

- `read_jsonl()`: Line-by-line JSONL parsing
- `save_json_pretty()`: Pretty-printed JSON output

**paths.rs**

- `resolve_paths()`: Resolves Claude, Codex, and Gemini session directories
- Handles `~` expansion via `home` crate

**git.rs**

- `get_git_remote()`: Extracts Git remote URL from repository
- Uses `git config --get remote.origin.url`

**time.rs**

- `format_timestamp()`: ISO 8601 date formatting
- Date aggregation utilities

#### 7. TUI Components (`src/usage/display.rs`, `src/analysis/display.rs`)

Terminal UI components built with Ratatui:

- Widgets: Tables, borders, styled text
- Layout: Constraints-based responsive design
- Event handling: Keyboard input (q, Esc, Ctrl+C)
- Refresh loop: 1-second intervals

## Data Flow

### Usage Command Flow

```
User runs: vct usage [--table|--text|--json]
                │
                ▼
         cli.rs parses args
                │
                ▼
    main.rs::Commands::Usage
                │
                ▼
usage/calculator.rs::get_usage_from_directories()
                │
                ├─> utils/paths.rs::resolve_paths()
                │   └─> Returns ~/.claude/projects, ~/.codex/sessions, ~/.gemini/tmp
                │
                ├─> walkdir scans *.jsonl files
                │
                ├─> For each file:
                │   ├─> utils/file.rs::read_jsonl()
                │   ├─> analysis/detector.rs::detect_format()
                │   └─> Aggregate tokens by (date, model)
                │
                ▼
        pricing.rs::fetch_model_pricing()
                │
                ├─> Check cache: ~/.vibe-coding-tracker/model_pricing_YYYY-MM-DD.json
                ├─> If expired: fetch from GitHub
                └─> Fuzzy match model names
                │
                ▼
        Calculate costs for each (date, model)
                │
                ▼
    usage/display.rs::display_usage_*()
                │
                ├─> Interactive: Ratatui TUI loop
                ├─> Table: comfy-table render
                ├─> Text: Date > model: cost
                └─> JSON: Full precision output
```

### Analysis Command Flow (Single File)

```
User runs: vct analysis --path <file.jsonl> [--output <out.json>]
                │
                ▼
         cli.rs parses args
                │
                ▼
    main.rs::Commands::Analysis
                │
                ▼
analysis/analyzer.rs::analyze_jsonl_file()
                │
                ├─> utils/file.rs::read_jsonl()
                │
                ├─> analysis/detector.rs::detect_format()
                │   └─> Identify Gemini (sessionId/projectHash/messages) or Claude (`parentUuid`)
                │
                ├─> Route to analyzer:
                │   ├─> Claude: claude_analyzer.rs
                │   │   ├─> Parse ClaudeMessage structs
                │   │   ├─> Extract tool_use blocks
                │   │   ├─> Count file operations (Edit/Read/Write lines)
                │   │   ├─> Extract Git info
                │   │   └─> Aggregate tool call counts
                │   │
                │   ├─> Codex: codex_analyzer.rs
                │       ├─> Parse CodexMessage structs
                │       ├─> Extract completion_response.usage
                │       └─> Handle reasoning tokens
                │
                │   └─> Gemini: gemini_analyzer.rs
                │       └─> Parse session JSON (messages, tokens)
                │
                ▼
        Build CodeAnalysis struct
                │
                ▼
        Output JSON (if --output specified)
```

### Analysis Command Flow (Batch)

```
User runs: vct analysis [--output <out.json>]
                │
                ▼
    main.rs::Commands::Analysis
                │
                ▼
analysis/batch_analyzer.rs::analyze_all_sessions()
                │
                ├─> utils/paths.rs::resolve_paths()
                │
                ├─> For each *.jsonl file:
                │   └─> analysis/analyzer.rs::analyze_jsonl_file()
                │
                ├─> Aggregate by (date, model):
                │   ├─> Sum edit/read/write lines
                │   └─> Sum tool call counts
                │
                ▼
    analysis/display.rs::display_analysis_*()
                │
                ├─> Interactive: Ratatui TUI (default)
                │   ├─> Show totals row
                │   ├─> Memory usage stats
                │   └─> 1s refresh loop
                │
                └─> JSON: Save aggregated array
```

## Key Components

### 1. Format Detection System

**Location:** `analysis/detector.rs`

**Logic:**

```rust
fn detect_format(records: &[Value]) -> FileFormat {
    if records.is_empty() {
        return FileFormat::Codex;
    }

    if records.len() == 1 {
        if let Some(obj) = records[0].as_object() {
            if obj.contains_key("sessionId")
                && obj.contains_key("projectHash")
                && obj.contains_key("messages")
            {
                return FileFormat::Gemini;
            }
        }
    }

    for record in records {
        if let Some(obj) = record.as_object() {
            if obj.contains_key("parentUuid") {
                return FileFormat::Claude;
            }
        }
    }

    FileFormat::Codex
}
```

**Rationale:** Gemini exports wrap a session in a single JSON object with `sessionId`/`projectHash`, Claude Code records contain `parentUuid`, and Codex defaults to the OpenAI event log structure.

### 2. Pricing Cache System

**Design Goals:**

- Minimize network requests
- Daily pricing updates
- Offline capability (stale cache acceptable)

**Implementation:**

```rust
// Cache file naming: model_pricing_YYYY-MM-DD.json
let cache_path = format!("{}/model_pricing_{}.json", cache_dir, today);

if cache_path.exists() {
    // Use cached data
} else {
    // Fetch from GitHub, save to cache
}
```

### 3. Interactive TUI Architecture

**Framework:** Ratatui (formerly tui-rs)

**Event Loop:**

```rust
loop {
    terminal.draw(|f| {
        // Render UI
    })?;

    if event::poll(Duration::from_secs(1))? {
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                _ => {}
            }
        }
    }

    // Refresh data every iteration
}
```

**Benefits:**

- Real-time updates
- Keyboard navigation
- Responsive layout
- Memory-efficient (no history buffering)

### 4. Fuzzy Model Matching

**Library:** `strsim` (Jaro-Winkler algorithm)

**Threshold:** 0.7 similarity score

**Example Matches:**

```
User model: "claude-sonnet-4-20250514"
Pricing DB: "claude-sonnet-4"
Match: Normalized (strip version suffix)

User model: "gpt-4-turbo-2024-04-09"
Pricing DB: "gpt-4-turbo"
Match: Normalized

User model: "anthropic.claude-3-sonnet"
Pricing DB: "claude-3-sonnet"
Match: Fuzzy (0.85 similarity)
```

## Design Patterns

### 1. Pipeline Architecture

Each analysis flow follows a clear pipeline:

```
Input → Detection → Parsing → Aggregation → Display → Output
```

### 2. Strategy Pattern (Display Modes)

Multiple display strategies for same data:

- `display_usage_interactive()`
- `display_usage_table()`
- `display_usage_text()`
- `display_usage_json()`

### 3. Repository Pattern (Data Access)

Centralized file access through `utils/file.rs`:

- `read_jsonl()`: Streaming JSONL reader
- `save_json_pretty()`: Formatted JSON writer

### 4. Factory Pattern (Format Detection)

`detector.rs` acts as factory, routing to appropriate analyzer:

```rust
match detect_format(line)? {
    FileFormat::Claude => claude_analyzer::analyze(),
    FileFormat::Codex => codex_analyzer::analyze(),
    FileFormat::Gemini => gemini_analyzer::analyze(),
}
```

### 5. Error Handling Strategy

**Libraries:**

- `anyhow`: Application-level errors (main.rs, high-level logic)
- `thiserror`: Library-level errors (custom error types)

**Approach:**

- Propagate errors with `?` operator
- Context-aware error messages
- Graceful degradation (e.g., $0.00 for unknown models)

## External Dependencies

### Critical Dependencies

**CLI & Serialization:**

- `clap` (4.x): Derive-based CLI parsing
- `serde` + `serde_json`: Serialization framework

**TUI:**

- `ratatui`: Terminal UI framework
- `crossterm`: Cross-platform terminal control
- `comfy-table`: Static table rendering

**HTTP & Data:**

- `reqwest` (rustls-tls): Async HTTP client
- `chrono`: Date/time manipulation

**File System:**

- `walkdir`: Recursive directory traversal
- `home`: Platform-agnostic home directory

**Algorithms:**

- `strsim`: Fuzzy string matching
- `regex`: Pattern matching

**System:**

- `sysinfo`: Memory and system stats
- `hostname`: Machine hostname retrieval

### Dependency Rationale

**Why Ratatui over alternatives?**

- Active maintenance
- Rich widget library
- Flexible layout system
- Efficient rendering

**Why rustls-tls for reqwest?**

- Smaller binary size than native-tls
- Pure Rust implementation
- Better cross-compilation

**Why comfy-table?**

- UTF-8 box drawing
- Flexible styling
- Column auto-sizing

## Performance Considerations

### 1. JSONL Streaming

**Approach:** Line-by-line parsing instead of loading entire file

**Benefits:**

- Constant memory usage
- Early error detection
- Handles large files (>100MB)

**Implementation:**

```rust
for line in BufReader::new(file).lines() {
    let line = line?;
    // Parse single line
}
```

### 2. Caching Strategy

**Pricing Cache:**

- Daily TTL reduces network requests
- Local file cache (no database overhead)

**No Session Cache:**

- Always read fresh data (session files change frequently)
- Acceptable trade-off for CLI tool

### 3. Aggregation Efficiency

**Data Structures:**

```rust
type DateUsageResult = HashMap<String, HashMap<String, serde_json::Value>>;
// Outer key: Date (YYYY-MM-DD)
// Inner key: Model name
// Value: Token usage data (varies by extension type)
```

**Complexity:** O(1) insertion, O(n) iteration for display

### 4. TUI Refresh Rate

**Interval:** 1 second

**Rationale:**

- Balance between responsiveness and CPU usage
- Session files updated infrequently (minutes/hours)
- Minimal overhead for re-aggregation

### 5. Binary Size Optimization

**Release Profile:**

```toml
[profile.release]
lto = "thin"      # Link-time optimization
codegen-units = 1 # Better optimization
strip = true      # Remove debug symbols
```

**Result:** ~3-5 MB binary

## File Format Specifications

### Claude Code JSONL Format

```json
{
  "parentUuid": "conv-123",
  "type": "apiResponse",
  "message": {
    "model": "claude-sonnet-4-20250514",
    "usage": {
      "input_tokens": 1000,
      "output_tokens": 500,
      "cache_read_input_tokens": 2000,
      "cache_creation_input_tokens": 500
    },
    "content": [
      {
        "type": "text",
        "text": "..."
      },
      {
        "type": "tool_use",
        "name": "Edit",
        "input": {
          "old_string": "...",
          "new_string": "..."
        }
      }
    ]
  }
}
```

### Codex JSONL Format

```json
{
  "completion_response": {
    "usage": {
      "prompt_tokens": 1000,
      "completion_tokens": 500,
      "total_tokens": 1500
    }
  },
  "reasoning_output_tokens": 100,
  "total_token_usage": 1600
}
```

## Directory Structure

```
~/.claude/
└── projects/
    ├── project-a.jsonl
    └── project-b.jsonl

~/.codex/
└── sessions/
    ├── session-1.jsonl
    └── session-2.jsonl

~/.gemini/
└── tmp/
    └── <project-hash>/
        └── chats/
            ├── session-1.json
            └── session-2.json

~/.vibe-coding-tracker/
└── model_pricing_2025-10-05.json  # Daily cache
```

## Extension Points

### Adding New AI Platforms

1. **Add model to `src/models/`**:

   ```rust
   // src/models/newplatform.rs
   #[derive(Deserialize)]
   pub struct NewPlatformMessage { ... }
   ```

2. **Update detector**:

   ```rust
   // src/analysis/detector.rs
   pub enum FileFormat {
       Claude,
       Codex,
       Gemini,
       NewPlatform,  // Add here
   }
   ```

3. **Implement analyzer**:

   ```rust
   // src/analysis/newplatform_analyzer.rs
   pub fn analyze_newplatform(lines: Vec<String>) -> Result<CodeAnalysis>
   ```

4. **Update router**:

   ```rust
   // src/analysis/analyzer.rs
   match format {
       FileFormat::NewPlatform => newplatform_analyzer::analyze(lines),
       ...
   }
   ```

### Adding New Display Formats

Implement trait-based abstraction:

```rust
trait DisplayFormatter {
    fn format(&self, data: &DateUsageResult) -> String;
}

struct TableFormatter;
struct JsonFormatter;
// Add custom formatters
```

### Adding New Metrics

1. Update `CodeAnalysis` struct in `models/analysis.rs`
2. Update analyzers to extract new metrics
3. Update display modules to render new columns

## Security Considerations

### File Access

- Only reads from known directories (`~/.claude`, `~/.codex`, `~/.gemini`)
- No arbitrary file write (output only to user-specified paths)
- No command execution in parsed data

### Network Requests

- HTTPS only (rustls)
- Single endpoint: GitHub raw content
- No authentication required
- No user data transmitted

### Data Privacy

- All processing local
- No telemetry or analytics
- Session data never leaves machine

## Testing Strategy

### Unit Tests

- Model deserialization
- Pricing calculation
- Fuzzy matching logic
- Date formatting

### Integration Tests

Located in `tests/`:

- `test_integration_usage.rs`: End-to-end usage command
- `test_integration_analysis.rs`: End-to-end analysis command

**Test Fixtures:**

- `examples/test_conversation.jsonl`: Claude format
- `examples/test_conversation_oai.jsonl`: Codex format

### Manual Testing

```bash
# Test usage command
cargo run -- usage --table

# Test analysis command
cargo run -- analysis --path examples/test_conversation.jsonl

# Test batch analysis
cargo run -- analysis

# Test error handling
cargo run -- usage --invalid-flag
```

## Future Architecture Considerations

### Potential Improvements

1. **Plugin System**: Load analyzers dynamically for new platforms
2. **Database Backend**: SQLite for historical tracking
3. **Web Dashboard**: Ratatui → WebAssembly UI
4. **Distributed Analysis**: Analyze across multiple machines
5. **Real-time Monitoring**: Watch session files for changes
6. **Export Formats**: CSV, Excel, PDF reports

### Scalability

**Current Limits:**

- File count: ~1000 sessions (tested)
- File size: Up to 1GB per file (streaming parser)
- Memory: \<50MB typical usage

**Bottlenecks:**

- Aggregation is O(n) where n = total lines across all files
- No indexing or incremental updates

**Solutions for Scale:**

- Incremental aggregation (track last-processed line)
- SQLite index on (date, model)
- Parallel file processing (Rayon)

---

**Document Version:** 1.0
**Last Updated:** 2025-10-05
**Maintainer:** Vibe Coding Tracker Team
