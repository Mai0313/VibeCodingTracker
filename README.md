<center>

# CodexUsage — Codex & Claude Code Telemetry Parser

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/CodexUsage/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/CodexUsage/pulls)

</center>

CodexUsage parses JSONL event logs from Codex and Claude Code, producing aggregated CodeAnalysis JSON plus optional debug artifacts.

Other Languages: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## What It Does

This project is a complete Rust translation of the original Go implementation (`parser.go`), parsing and analyzing JSONL log files from Claude Code and Codex.

- Parse JSONL logs (one event per line) from Codex and Claude Code
- Detect source automatically and normalize paths across sessions
- Aggregate read/write/edit/command usage, tool call counts, and conversation token usage
- Output a single CodeAnalysis record (JSON) and optional debug files

Scope focuses on data extraction, summarization, and file handling. Transport of results (e.g., SendAnalysisData) is out of scope.

## Features

1. **Automatic Detection**: Automatically identifies Claude Code or Codex log format
2. **Comprehensive Statistics**: Includes file operations, tool calls, token usage, and more
3. **Beautiful Output**: Usage statistics with formatted table display and thousand separators
4. **Robust Error Handling**: Leverages Rust's type system for reliable error management
5. **Performance Optimized**: Release builds include LTO and symbol stripping

## Quick Start

Prerequisites: Rust toolchain (rustup), Docker optional

```bash
# Build the project
make fmt            # rustfmt + clippy
make test           # cargo test (verbose)
make build          # cargo build
make build-release  # cargo build --release
make package        # build .crate package
```

## CLI Usage

### Analysis Command

Analyze a JSONL conversation file and get detailed statistics:

```bash
# Analyze and output to stdout
codex_usage analysis --path examples/test_conversation.jsonl

# Analyze and save to a file
codex_usage analysis --path examples/test_conversation.jsonl --output result.json

# Analyze Codex logs
codex_usage analysis --path examples/test_conversation_oai.jsonl
```

### Usage Command

Display token usage statistics from your Claude Code and Codex sessions:

```bash
# Display usage in table format
codex_usage usage

# Display usage in JSON format
codex_usage usage --json
```

### Version Command

Display version information:

```bash
codex_usage version
```

## Project Structure

```
codex_usage/
├── src/
│   ├── lib.rs              # Library code
│   ├── main.rs             # CLI entry point
│   ├── cli.rs              # CLI argument parsing
│   ├── models/             # Data models
│   │   ├── mod.rs
│   │   ├── analysis.rs     # Analysis data structures
│   │   ├── usage.rs        # Usage data structures
│   │   ├── claude.rs       # Claude Code log models
│   │   └── codex.rs        # Codex log models
│   ├── analysis/           # Analysis functionality
│   │   ├── mod.rs
│   │   ├── analyzer.rs     # Main analyzer
│   │   ├── claude_analyzer.rs  # Claude Code analyzer
│   │   ├── codex_analyzer.rs   # Codex analyzer
│   │   └── detector.rs     # Extension type detection
│   ├── usage/              # Usage statistics
│   │   ├── mod.rs
│   │   ├── calculator.rs   # Usage calculation
│   │   └── display.rs      # Usage display formatting
│   └── utils/              # Utility functions
│       ├── mod.rs
│       ├── paths.rs        # Path handling
│       ├── time.rs         # Time parsing
│       ├── file.rs         # File I/O
│       └── git.rs          # Git operations
├── examples/               # Example JSONL files
├── tests/                  # Integration tests
└── parser.go              # Original Go implementation (reference)
```

## Key Dependencies

- **CLI**: clap (v4.5) - Command-line argument parsing
- **Serialization**: serde, serde_json - JSON handling
- **Error Handling**: anyhow, thiserror - Robust error management
- **Time**: chrono - Timestamp parsing
- **File System**: walkdir, home - Directory traversal and path resolution
- **Regex**: regex - Pattern matching in log parsing
- **Logging**: log, env_logger - Debug output

## Go to Rust Mapping

| Go Function | Rust Implementation | Description |
|-------------|---------------------|-------------|
| `analyzeConversations` | `analysis::claude_analyzer::analyze_claude_conversations` | Claude Code analysis |
| `analyzeCodexConversations` | `analysis::codex_analyzer::analyze_codex_conversations` | Codex analysis |
| `CalculateUsageFromJSONL` | `usage::calculator::calculate_usage_from_jsonl` | Single file usage calculation |
| `GetUsageFromDirectories` | `usage::calculator::get_usage_from_directories` | Directory usage statistics |
| `ReadJSONL` | `utils::file::read_jsonl` | JSONL file reading |
| `parseISOTimestamp` | `utils::time::parse_iso_timestamp` | Timestamp parsing |
| `getGitRemoteOriginURL` | `utils::git::get_git_remote_url` | Git remote URL extraction |

## Docker

```bash
docker build -f docker/Dockerfile --target prod -t ghcr.io/<owner>/<repo>:latest .
docker run --rm ghcr.io/<owner>/<repo>:latest
```

Binary image tag (current placeholder):
```bash
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .
docker run --rm codex_usage:latest
```

## Naming Placeholders

- Crate/binary: `codex_usage`
- Repository links: `https://github.com/<owner>/codex-usage` placeholders
- CI workflows may assume a specific binary name; we will align them after the final project name is chosen

## License

MIT — see `LICENSE`.
