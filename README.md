<center>

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

</center>

**Track your AI coding costs in real-time.** Vibe Coding Tracker is a powerful CLI tool that helps you monitor and analyze your Claude Code, Codex, Copilot, and Gemini usage, providing detailed cost breakdowns, token statistics, and code operation insights.

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> Note: CLI examples use the short alias `vct`. If you built from source, the compiled binary is named `vibe_coding_tracker`; create an alias or replace `vct` with the full name when running commands.

---

## 🎯 Why Vibe Coding Tracker?

### 💰 Know Your Costs

Stop wondering how much your AI coding sessions cost. Get **real-time cost tracking** with automatic pricing updates from [LiteLLM](https://github.com/BerriAI/litellm).

### 📊 Beautiful Visualizations

Choose your preferred view:

- **Interactive Dashboard**: Auto-refreshing terminal UI with live updates
- **Static Reports**: Professional tables for documentation
- **Script-Friendly**: Plain text and JSON for automation
- **Full Precision**: Export exact costs for accounting

### 🚀 Zero Configuration

Automatically detects and processes logs from Claude Code, Codex, Copilot, and Gemini. No setup required—just run and analyze.

### 🎨 Rich Insights

- Token usage by model and date
- Cost breakdown by cache types
- File operations tracking
- Command execution history
- Git repository information

---

## ✨ Key Features

| Feature                    | Description                                                          |
| -------------------------- | -------------------------------------------------------------------- |
| 🤖 **Auto-Detection**      | Intelligently identifies Claude Code, Codex, Copilot, or Gemini logs |
| 💵 **Smart Pricing**       | Fuzzy model matching + daily cache for speed                         |
| 🎨 **4 Display Modes**     | Interactive, Table, Text, and JSON outputs                           |
| 📈 **Comprehensive Stats** | Tokens, costs, file ops, and tool calls                              |
| ⚡ **High Performance**    | Built with Rust for speed and reliability                            |
| 🔄 **Live Updates**        | Real-time dashboard refreshes every second                           |
| 💾 **Efficient Caching**   | Smart daily cache reduces API calls                                  |

---

## 🚀 Quick Start

### Installation

Choose the installation method that works best for you:

#### Method 1: Build from Source (Recommended for Developers)

For users who want to customize the build or contribute to development:

```bash
# 1. Clone the repository
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker

# 2. Build release version
cargo build --release

# 3. Binary location
./target/release/vibe_coding_tracker

# 4. Optional: create a short alias
# Linux/macOS:
sudo ln -sf "$(pwd)/target/release/vibe_coding_tracker" /usr/local/bin/vct

# Or install to user directory:
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct
# Make sure ~/.local/bin is in your PATH
```

**Prerequisites**: [Rust toolchain](https://rustup.rs/) 1.85 or higher

> **Note**: This project uses **Rust 2024 edition** and requires Rust 1.85+. Update your toolchain with `rustup update` if needed.

#### Method 2: Install from crates.io

Install using Cargo from the official Rust package registry:

```bash
cargo install vibe_coding_tracker
```

#### Method 3: Install from npm

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

#### Method 4: Install from PyPI

**Prerequisites**: Python 3.8 or higher

```bash
pip install vibe_coding_tracker
# Or with uv
uv pip install vibe_coding_tracker
```

### First Run

```bash
# View your usage with the short alias (if available)
vct usage

# Or run the binary built by Cargo
./target/release/vibe_coding_tracker usage

# Analyze a specific conversation
./target/release/vibe_coding_tracker analysis --path ~/.claude/projects/session.jsonl
```

> 💡 **Tip**: Use `vct` as a short alias for `vibe_coding_tracker` to save typing—create it manually with `ln -sf ./target/release/vibe_coding_tracker ~/.local/bin/vct` (or any path you prefer).

---

## 📖 Command Guide

### 🔍 Quick Reference

```bash
vct <COMMAND> [OPTIONS]
# Replace with `vibe_coding_tracker` if you are using the full binary name

Commands:
analysis    Analyze JSONL conversation files (single file or all sessions)
usage       Display token usage statistics
version     Display version information
update      Update to the latest version from GitHub releases
help        Print this message or the help of the given subcommand(s)
```

---

## 💰 Usage Command

**Track your spending across all AI coding sessions.**

### Basic Usage

```bash
# Interactive dashboard (recommended)
vct usage

# Static table for reports
vct usage --table

# Plain text for scripts
vct usage --text

# JSON for data processing
vct usage --json
```

### What You Get

The tool scans these directories automatically:

- `~/.claude/projects/*.jsonl` (Claude Code)
- `~/.codex/sessions/*.jsonl` (Codex)
- `~/.copilot/history-session-state/*.json` (Copilot)
- `~/.gemini/tmp/<project_hash>/chats/*.json` (Gemini)

---

## 📊 Analysis Command

**Deep dive into conversation files - single file or batch analysis.**

### Basic Usage

```bash
# Single file: Analyze and display
vct analysis --path ~/.claude/projects/session.jsonl

# Single file: Save to file
vct analysis --path ~/.claude/projects/session.jsonl --output report.json

# Batch: Analyze all sessions with interactive table (default)
vct analysis

# Batch: Static table output with daily averages
vct analysis --table

# Batch: Save aggregated results to JSON
vct analysis --output batch_report.json

# Batch with provider grouping: Output complete records grouped by provider (JSON format)
vct analysis --all

# Save the grouped results to a file
vct analysis --all --output grouped_report.json
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

# Force update - always downloads latest version (even if already up-to-date)
vct update --force
```

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
