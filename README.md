<div align="center" markdown="1">

# Vibe Coding Tracker — AI Coding Assistant Usage Tracker

[![Crates.io](https://img.shields.io/crates/v/vibe_coding_tracker?logo=rust&style=flat-square&color=E05D44)](https://crates.io/crates/vibe_coding_tracker)
[![Crates.io Downloads](https://img.shields.io/crates/d/vibe_coding_tracker?logo=rust&style=flat-square)](https://crates.io/crates/vibe_coding_tracker)
[![npm version](https://img.shields.io/npm/v/vibe-coding-tracker?logo=npm&style=flat-square&color=CB3837)](https://www.npmjs.com/package/vibe-coding-tracker)
[![npm downloads](https://img.shields.io/npm/dt/vibe-coding-tracker?logo=npm&style=flat-square)](https://www.npmjs.com/package/vibe-coding-tracker)
[![PyPI version](https://img.shields.io/pypi/v/vibe_coding_tracker?logo=python&style=flat-square&color=3776AB)](https://pypi.org/project/vibe_coding_tracker/)
[![PyPI downloads](https://img.shields.io/pypi/dm/vibe_coding_tracker?logo=python&style=flat-square)](https://pypi.org/project/vibe-coding-tracker)
[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white&style=flat-square)](https://www.rust-lang.org/)
[![tests](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/test.yml?label=tests&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/test.yml)
[![code-quality](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/code-quality-check.yml?label=code-quality&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/tree/master?tab=License-1-ov-file)
[![Star on GitHub](https://img.shields.io/github/stars/Mai0313/VibeCodingTracker?style=social&label=Star)](https://github.com/Mai0313/VibeCodingTracker)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</div>

**Track your AI coding costs in real-time.** Vibe Coding Tracker is a lightweight, high-performance CLI tool built in Rust that monitors and analyzes your Claude Code, Codex, Copilot, and Gemini usage — with detailed cost breakdowns, token statistics, and code operation insights, all while keeping CPU and memory usage minimal.

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> Note: CLI examples use the short alias `vct`. If you installed via npm/pip/cargo, the binary might be named `vibe_coding_tracker` or `vct`. Create an alias or replace `vct` with the full name when running commands if needed.

---

## 🎯 Why Vibe Coding Tracker?

### 💰 Know Your Costs

Stop wondering how much your AI coding sessions cost. Get **real-time cost tracking** with automatic pricing updates from [LiteLLM](https://github.com/BerriAI/litellm).

### 🪶 Ultra-Lightweight

Built with Rust for minimal resource footprint. The interactive TUI dashboard runs at **~3-5% CPU** and **~140 MB memory** even while processing millions of tokens across multiple providers — no Electron, no bloated runtimes.

### 📊 Beautiful Visualizations

Choose your preferred view:

- **Interactive Dashboard**: Auto-refreshing terminal UI with live updates
- **Static Reports**: Professional tables for documentation
- **Script-Friendly**: Plain text and JSON for automation
- **Full Precision**: Export exact costs for accounting

### 🚀 Zero Configuration

Automatically detects and processes logs from Claude Code, Codex, Copilot, and Gemini. No setup required — just run and analyze.

### 🎨 Rich Insights

- Token usage by model and date
- Cost breakdown by cache types (read / create)
- File operations tracking (edit, read, write lines)
- Tool call history (Bash, Edit, Read, Write, TodoWrite)
- Per-provider daily averages

---

## ✨ Key Features

| Feature                  | Description                                                    |
| ------------------------ | -------------------------------------------------------------- |
| 🤖 **Multi-Provider**    | Claude Code, Codex, Copilot, and Gemini — all in one place     |
| 💵 **Smart Pricing**     | Fuzzy model matching + daily cache from LiteLLM                |
| 🎨 **4 Display Modes**   | Interactive TUI, static table, plain text, and JSON            |
| 📈 **Dual Analysis**     | Token/cost stats (`usage`) + code operation stats (`analysis`) |
| 🪶 **Ultra-Lightweight** | ~3-5% CPU, ~140 MB RAM — built with Rust                       |
| 🔄 **Live Updates**      | Real-time dashboard refreshes every second                     |
| 💾 **Efficient Caching** | Smart daily cache reduces API calls                            |

---

## 🚀 Quick Start

### Installation

Choose the installation method that works best for you:

> 👨‍💻 **Developers**: If you want to build from source or contribute to development, please see [CONTRIBUTING.md](CONTRIBUTING.md).

#### Method 1: Install from npm

**Prerequisites**: [Node.js](https://nodejs.org/) v22 or higher

Choose one of the following package names (they are identical):

```bash
# Main package
npm install -g vibe-coding-tracker

# Short alias with scope
npm install -g @mai0313/vct

# Full name with scope
npm install -g @mai0313/vibe-coding-tracker
```

#### Method 2: Install from PyPI

**Prerequisites**: Python 3.8 or higher

```bash
pip install vibe_coding_tracker
# Or with uv
uv pip install vibe_coding_tracker
```

#### Method 3: Install from crates.io

Install using Cargo from the official Rust package registry:

```bash
cargo install vibe_coding_tracker
```

### First Run

```bash
# View your usage with the interactive dashboard
vct usage

# Or run the binary built by Cargo/pip
vibe_coding_tracker usage

# Analyze code operations across all sessions
vct analysis
```

---

## 📖 Command Guide

### 🔍 Quick Reference

```
vct <COMMAND> [OPTIONS]
# Replace with `vibe_coding_tracker` if you are using the full binary name

Commands:
  analysis    Analyze JSONL conversation files (single file or all sessions)
  usage       Display token usage statistics
  version     Display version information
  update      Update to the latest version from GitHub releases
  help        Print this message or the help of the given subcommand(s)
```

Time range flags (shared by `usage` and `analysis`, mutually exclusive, default `--all`):

| Flag        | Window                            |
| ----------- | --------------------------------- |
| `--daily`   | Sessions modified today           |
| `--weekly`  | Current ISO week (Monday → today) |
| `--monthly` | Current calendar month            |
| `--all`     | Every session on disk (default)   |

---

## 💰 Usage Command

**Track your spending across all AI coding sessions.**

### Flags

| Flag                                           | Purpose                             |
| ---------------------------------------------- | ----------------------------------- |
| *(none)*                                       | Interactive TUI dashboard (default) |
| `--table`                                      | Static table, no TUI                |
| `--text`                                       | Plain text, script-friendly         |
| `--json`                                       | JSON with enriched pricing metadata |
| `--daily` / `--weekly` / `--monthly` / `--all` | Time range filter (see table above) |

### Basic Usage

```bash
# Interactive dashboard (recommended)
vct usage

# Static table for reports
vct usage --table

# Plain text for scripts
vct usage --text

# JSON for data processing (includes cost_usd and matched_model fields)
vct usage --json

# Combine time range with output format
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily
```

### Preview: Interactive Dashboard (`vct usage`)

```
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    📊 Token Usage Statistics                                │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Model                              Input     Output    Cache Read  Cache Create  Total Cost │
│                                                                                             │
│ claude-haiku-4-5-20251001          5,567     19,769    4,627,938   619,816       $1.34      │
│ claude-opus-4-6                    25,651    179,066   40,830,154  2,572,258     $77.59     │
│ gemini-3.1-pro-preview             129,115   10,339    67,385      0             $0.40      │
│ TOTAL                              160,333   209,174   45,525,477  3,192,074     $79.33     │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Provider              Tokens / Day     Cost / Day     Active Days                           │
│                                                                                             │
│ 🤖 Claude Code        16,293,406       $26.31         3                                    │
│ ✨ Gemini             206,839          $0.40          1                                     │
│ ⭐ All Providers      16,362,353       $26.44         3                                    │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│   💰 Total: $79.33 | 🔢 Tokens: 49,087,058 | 📊 Models: 3 | ⚡ CPU: 4.6% | 🧠 Mem: 148 MB │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
                          Press 'q', 'Esc', 'Ctrl+C' to quit | Press 'r' to refresh
```

### What It Scans

The tool automatically scans these directories:

- `~/.claude/projects/*.jsonl` (Claude Code)
- `~/.codex/sessions/*.jsonl` (Codex)
- `~/.copilot/history-session-state/*.json` (Copilot)
- `~/.gemini/tmp/<project_hash>/chats/*.json` (Gemini)

---

## 📊 Analysis Command

**Deep dive into code operations — see exactly what your AI assistant did.**

### Flags

| Flag                                           | Purpose                                                            |
| ---------------------------------------------- | ------------------------------------------------------------------ |
| *(none)*                                       | Interactive TUI dashboard over all sessions                        |
| `--path <FILE>`                                | Analyze a single JSONL/JSON conversation file (prints JSON)        |
| `--table`                                      | Static table with per-provider daily averages                      |
| `--output <FILE>`                              | Save results as pretty-printed JSON                                |
| `--by-provider`                                | Group rows by provider (claude / codex / copilot / gemini) as JSON |
| `--daily` / `--weekly` / `--monthly` / `--all` | Time range filter (see table above)                                |

See [`examples/`](examples/) for sample inputs and matching JSON outputs for all four providers.

### Basic Usage

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with daily averages
vct analysis --table

# Analyze a single conversation file → stdout JSON
vct analysis --path ~/.claude/projects/session.jsonl

# Save results to JSON
vct analysis --output report.json

# Group results by provider
vct analysis --by-provider

# Save grouped results
vct analysis --by-provider --output grouped_report.json

# Combine time range with output format
vct analysis --weekly
vct analysis --table --monthly
vct analysis --by-provider --daily --output today.json
```

### Preview: Interactive Dashboard (`vct analysis`)

```
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    🔍 Analysis Statistics                                   │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Model                        Edit Lines  Read Lines  Write Lines  Bash  Edit  Read  Write  │
│                                                                                             │
│ claude-haiku-4-5-20251001    0           0           0            43    0     59    0       │
│ claude-opus-4-6              1,280       13,264      1,575        82    146   209   62      │
│ gemini-3.1-pro-preview       0           0           0            0     0     0     0       │
│ TOTAL                        1,280       13,264      1,575        125   146   268   62      │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Provider          EditL/Day ReadL/Day WriteL/Day Bash/Day Edit/Day Read/Day Write/Day Days  │
│                                                                                             │
│ 🤖 Claude Code    426.7     4421.3    525.0      41.7     48.7     89.3     20.7      3    │
│ ✨ Gemini         0         0         0          0.0      0.0      0.0      0.0       1    │
│ ⭐ All Providers  426.7     4421.3    525.0      41.7     48.7     89.3     20.7      3    │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│  📝 Lines: 16,119 | 🔧 Tools: 601 | 📊 Models: 3 | ⚡ CPU: 3.6% | 🧠 Mem: 140 MB         │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
                          Press 'q', 'Esc', 'Ctrl+C' to quit | Press 'r' to refresh
```

---

## 🔄 Update Command

**Keep your installation up-to-date automatically.**

The update command works for **all installation methods** (npm/pip/cargo/manual) by directly downloading and replacing the binary from GitHub releases.

### Basic Usage

```bash
# Check for updates
vct update --check

# Interactive update with confirmation
vct update

# Force update — always downloads latest version
vct update --force
```

### Preview (`vct update --check`)

```
📋 Current version: v0.5.10
🔍 Checking for latest release...
✅ Latest version: v0.5.10 — you are up to date!
```

---

## 🏷️ Version Command

Report the embedded build metadata (binary version, Rust toolchain, Cargo version):

```bash
vct version          # Pretty table
vct version --text   # One-field-per-line, script-friendly
vct version --json   # Machine-readable JSON
```

The binary version is produced at build time by `build.rs` from `git describe`, so development builds include commit count + short SHA + `dirty` suffix when applicable.

---

## 💡 Smart Pricing System

### How It Works

1. **Automatic Updates**: Fetches pricing from [LiteLLM](https://github.com/BerriAI/litellm) daily
2. **Smart Caching**: Stores pricing in `~/.vibe_coding_tracker/` for 24 hours
3. **Fuzzy Matching**: Finds best match even for custom model names
4. **Always Accurate**: Ensures you get the latest pricing

### Model Matching

**Priority Order**:

1. ✅ **Exact Match**: `claude-sonnet-4` → `claude-sonnet-4`
2. 🔄 **Normalized**: `claude-sonnet-4-20250514` → `claude-sonnet-4`
3. 🔍 **Substring**: `custom-gpt-4` → `gpt-4`
4. 🎯 **Fuzzy (AI-powered)**: Uses Jaro-Winkler similarity (70% threshold)
5. 💵 **Fallback**: Shows $0.00 if no match found

---

## 🐳 Docker Support

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# Run with your sessions
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.copilot:/root/.copilot \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```
