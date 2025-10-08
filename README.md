<center>

# Vibe Coding Tracker — AI Coding Assistant Usage Tracker

[![Crates.io](https://img.shields.io/crates/v/vibe_coding_tracker?logo=rust&style=flat-square&color=E05D44)](https://crates.io/crates/vibe_coding_tracker)
[![Crates.io Downloads](https://img.shields.io/crates/d/vibe_coding_tracker?logo=rust&style=flat-square)](https://crates.io/crates/vibe_coding_tracker)
[![npm version](https://img.shields.io/npm/v/vibe_coding_tracker?logo=npm&style=flat-square&color=CB3837)](https://www.npmjs.com/package/vibe_coding_tracker)
[![npm downloads](https://img.shields.io/npm/dt/vibe_coding_tracker?logo=npm&style=flat-square)](https://www.npmjs.com/package/vibe_coding_tracker)
[![PyPI version](https://img.shields.io/pypi/v/vibe_coding_tracker?logo=python&style=flat-square&color=3776AB)](https://pypi.org/project/vibe_coding_tracker/)
[![PyPI downloads](https://img.shields.io/pypi/dm/vibe_coding_tracker?logo=python&style=flat-square)](https://pypi.org/project/vibe_coding_tracker/)
[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white&style=flat-square)](https://www.rust-lang.org/)
[![tests](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/test.yml?label=tests&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/test.yml)
[![code-quality](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/code-quality-check.yml?label=code-quality&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</center>

**Track your AI coding costs in real-time.** Vibe Coding Tracker is a powerful CLI tool that helps you monitor and analyze your Claude Code, Codex, and Gemini usage, providing detailed cost breakdowns, token statistics, and code operation insights.

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

Automatically detects and processes logs from Claude Code, Codex, and Gemini. No setup required—just run and analyze.

### 🎨 Rich Insights

- Token usage by model and date
- Cost breakdown by cache types
- File operations tracking
- Command execution history
- Git repository information

---

## ✨ Key Features

| Feature                    | Description                                                 |
| -------------------------- | ----------------------------------------------------------- |
| 🤖 **Auto-Detection**      | Intelligently identifies Claude Code, Codex, or Gemini logs |
| 💵 **Smart Pricing**       | Fuzzy model matching + daily cache for speed                |
| 🎨 **4 Display Modes**     | Interactive, Table, Text, and JSON outputs                  |
| 📈 **Comprehensive Stats** | Tokens, costs, file ops, and tool calls                     |
| ⚡ **High Performance**    | Built with Rust for speed and reliability                   |
| 🔄 **Live Updates**        | Real-time dashboard refreshes every second                  |
| 💾 **Efficient Caching**   | Smart daily cache reduces API calls                         |

---

## 🚀 Quick Start

### Installation

Choose the installation method that works best for you:

#### Method 1: Install from npm (Recommended ✨)

**The easiest way to install** - includes pre-compiled binaries for your platform, no build step required!

Choose any of the following package names (all are identical):

```bash
# Main package
npm install -g vibe-coding-tracker

# Short alias with scope
npm install -g @mai0313/vct

# Full name with scope
npm install -g @mai0313/vibe-coding-tracker
```

**Prerequisites**: [Node.js](https://nodejs.org/) v22 or higher

**Supported Platforms**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### Method 2: Install from PyPI

**For Python users** - includes pre-compiled binaries for your platform, no build step required!

```bash
# Install with pip
pip install vibe_coding_tracker

# Install with uv (recommended for faster installation)
uv pip install vibe_coding_tracker
```

**Prerequisites**: Python 3.8 or higher

**Supported Platforms**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### Method 3: Install from crates.io

Install using Cargo from the official Rust package registry:

```bash
cargo install vibe_coding_tracker
```

**Prerequisites**: [Rust toolchain](https://rustup.rs/) 1.70 or higher

#### Method 4: Build from Source

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

**Prerequisites**: [Rust toolchain](https://rustup.rs/) 1.70 or higher

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
usage       Show token usage and costs (default: interactive)
analysis    Analyze conversation files and export data
version     Display version information
update      Update to the latest version from GitHub releases
help        Show help information
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
- `~/.gemini/tmp/<project_hash>/chats/*.json` (Gemini)

### 🎨 Interactive Mode (Default)

**Live dashboard that updates every second**

```
┌──────────────────────────────────────────────────────────────────┐
│                  📊 Token Usage Statistics                       │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬──────────────────────┬────────────┬────────────┬────────────┬──────────────┬────────────┬────────────┐
│ Date       │ Model                │ Input      │ Output     │ Cache Read │ Cache Create │ Total      │ Cost (USD) │
├────────────┼──────────────────────┼────────────┼────────────┼────────────┼──────────────┼────────────┼────────────┤
│ 2025-10-01 │ claude-sonnet-4-20…  │ 45,230     │ 12,450     │ 230,500    │ 50,000       │ 338,180    │ $2.15      │
│ 2025-10-02 │ claude-sonnet-4-20…  │ 32,100     │ 8,920      │ 180,000    │ 30,000       │ 251,020    │ $1.58      │
│ 2025-10-03 │ claude-sonnet-4-20…  │ 28,500     │ 7,200      │ 150,000    │ 25,000       │ 210,700    │ $1.32      │
│ 2025-10-03 │ gpt-4-turbo          │ 15,000     │ 5,000      │ 0          │ 0            │ 20,000     │ $0.25      │
│            │ TOTAL                │ 120,830    │ 33,570     │ 560,500    │ 105,000      │ 819,900    │ $5.30      │
└────────────┴──────────────────────┴────────────┴────────────┴────────────┴──────────────┴────────────┴────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 💰 Total Cost: $5.30  |  🔢 Total Tokens: 819,900  |  📅 Entries: 4  |  🧠 Memory: 12.5 MB                       │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                            📈 Daily Averages                                                      │
│                                                                                                                   │
│  Claude Code: 266,667 tokens/day  |  $1.68/day                                                                   │
│  Codex: 20,000 tokens/day  |  $0.25/day                                                                          │
│  Overall: 204,975 tokens/day  |  $1.33/day                                                                       │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

Press 'q', 'Esc', or 'Ctrl+C' to quit
```

**Features**:

- ✨ Auto-refreshes every second
- 🎯 Highlights today's entries
- 🔄 Shows recently updated rows
- 💾 Displays memory usage
- 📊 Summary statistics
- 📈 Daily averages by provider (Claude Code, Codex, Gemini)

**Controls**: Press `q`, `Esc`, or `Ctrl+C` to exit

### 📋 Static Table Mode

**Perfect for documentation and reports**

```bash
vct usage --table
```

```
📊 Token Usage Statistics

╔════════════╦══════════════════════╦════════════╦════════════╦════════════╦══════════════╦══════════════╦════════════╗
║ Date       ║ Model                ║ Input      ║ Output     ║ Cache Read ║ Cache Create ║ Total Tokens ║ Cost (USD) ║
╠════════════╬══════════════════════╬════════════╬════════════╬════════════╬══════════════╬══════════════╬════════════╣
║ 2025-10-01 ║ claude-sonnet-4-20…  ║ 45,230     ║ 12,450     ║ 230,500    ║ 50,000       ║ 338,180      ║ $2.15      ║
║ 2025-10-02 ║ claude-sonnet-4-20…  ║ 32,100     ║ 8,920      ║ 180,000    ║ 30,000       ║ 251,020      ║ $1.58      ║
║ 2025-10-03 ║ claude-sonnet-4-20…  ║ 28,500     ║ 7,200      ║ 150,000    ║ 25,000       ║ 210,700      ║ $1.32      ║
║ 2025-10-03 ║ gpt-4-turbo          ║ 15,000     ║ 5,000      ║ 0          ║ 0            ║ 20,000       ║ $0.25      ║
║            ║ TOTAL                ║ 120,830    ║ 33,570     ║ 560,500    ║ 105,000      ║ 819,900      ║ $5.30      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝

📈 Daily Averages (by Provider)

╔═════════════╦════════════════╦══════════════╦══════╗
║ Provider    ║ Avg Tokens/Day ║ Avg Cost/Day ║ Days ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Claude Code ║ 266,667        ║ $1.68        ║ 3    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Codex       ║ 20,000         ║ $0.25        ║ 1    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ OVERALL     ║ 204,975        ║ $1.33        ║ 4    ║
╚═════════════╩════════════════╩══════════════╩══════╝
```

### 📝 Text Mode

**Ideal for scripting and parsing**

```bash
vct usage --text
```

```
2025-10-01 > claude-sonnet-4-20250514: $2.154230
2025-10-02 > claude-sonnet-4-20250514: $1.583450
2025-10-03 > claude-sonnet-4-20250514: $1.321200
2025-10-03 > gpt-4-turbo: $0.250000
```

### 🗂️ JSON Mode

**Full precision for accounting and integration**

```bash
vct usage --json
```

```json
{
  "2025-10-01": [
    {
      "model": "claude-sonnet-4-20250514",
      "usage": {
        "input_tokens": 45230,
        "output_tokens": 12450,
        "cache_read_input_tokens": 230500,
        "cache_creation_input_tokens": 50000,
        "cache_creation": {
          "ephemeral_5m_input_tokens": 50000
        },
        "service_tier": "standard"
      },
      "cost_usd": 2.1542304567890125
    }
  ]
}
```

### 🔍 Output Comparison

| Feature         | Interactive | Table   | Text      | JSON               |
| --------------- | ----------- | ------- | --------- | ------------------ |
| **Best For**    | Monitoring  | Reports | Scripts   | Integration        |
| **Cost Format** | $2.15       | $2.15   | $2.154230 | 2.1542304567890123 |
| **Updates**     | Real-time   | Static  | Static    | Static             |
| **Colors**      | ✅          | ✅      | ❌        | ❌                 |
| **Parseable**   | ❌          | ❌      | ✅        | ✅                 |

### 💡 Use Cases

- **Budget Tracking**: Monitor your daily AI spending
- **Cost Optimization**: Identify expensive sessions
- **Team Reporting**: Generate usage reports for management
- **Billing**: Export precise costs for invoicing
- **Monitoring**: Real-time dashboard for active development

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

# Batch: Save aggregated results to JSON
vct analysis --output batch_report.json

# Batch with provider grouping: Output complete records grouped by provider (JSON format)
vct analysis --all

# Save the grouped results to a file
vct analysis --all --output grouped_report.json
```

### What You Get

**Single File Analysis**:

- **Token Usage**: Input, output, and cache statistics by model
- **File Operations**: Every read, write, and edit with full details
- **Command History**: All shell commands executed
- **Tool Usage**: Counts of each tool type used
- **Metadata**: User, machine ID, Git repo, timestamps

**Batch Analysis**:

- **Aggregated Metrics**: Grouped by date and model
- **Line Counts**: Edit, read, and write operations
- **Tool Statistics**: Bash, Edit, Read, TodoWrite, Write counts
- **Interactive Display**: Real-time TUI table (default)
- **JSON Export**: Structured data for further processing

### Sample Output - Single File

```json
{
  "extensionName": "Claude-Code",
  "insightsVersion": "0.1.0",
  "user": "wei",
  "machineId": "5b0dfa41ada84d5180a514698f67bd80",
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
      "toolCallCounts": {
        "Read": 15,
        "Write": 4,
        "Edit": 2,
        "Bash": 5,
        "TodoWrite": 3
      },
      "totalUniqueFiles": 8,
      "totalWriteLines": 80,
      "totalReadLines": 120,
      "folderPath": "/home/wei/repo/project",
      "gitRemoteUrl": "https://github.com/user/project.git"
    }
  ]
}
```

### Sample Output - Batch Analysis

**Interactive Table** (default when running `vct analysis`):

```
┌──────────────────────────────────────────────────────────────────┐
│                  🔍 Analysis Statistics                           │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬────────────────────┬────────────┬────────────┬────────────┬──────┬──────┬──────┬───────────┬───────┐
│ Date       │ Model              │ Edit Lines │ Read Lines │ Write Lines│ Bash │ Edit │ Read │ TodoWrite │ Write │
├────────────┼────────────────────┼────────────┼────────────┼────────────┼──────┼──────┼──────┼───────────┼───────┤
│ 2025-10-02 │ claude-sonnet-4-5…│ 901        │ 11,525     │ 53         │ 13   │ 26   │ 27   │ 10        │ 1     │
│ 2025-10-03 │ claude-sonnet-4-5…│ 574        │ 10,057     │ 1,415      │ 53   │ 87   │ 78   │ 30        │ 8     │
│ 2025-10-03 │ gpt-5-codex        │ 0          │ 1,323      │ 0          │ 75   │ 0    │ 20   │ 0         │ 0     │
│            │ TOTAL              │ 1,475      │ 22,905     │ 1,468      │ 141  │ 113  │ 125  │ 40        │ 9     │
└────────────┴────────────────────┴────────────┴────────────┴────────────┴──────┴──────┴──────┴───────────┴───────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 📝 Total Lines: 25,848  |  🔧 Total Tools: 428  |  📅 Entries: 3  |  🧠 Memory: 8.2 MB                         │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

Press 'q', 'Esc', or 'Ctrl+C' to quit
```

**JSON Export** (with `--output`):

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

### 💡 Use Cases

**Single File Analysis**:

- **Usage Auditing**: Track what the AI did in each session
- **Cost Attribution**: Calculate costs per project or feature
- **Compliance**: Export detailed activity logs
- **Analysis**: Understand coding patterns and tool usage

**Batch Analysis**:

- **Productivity Tracking**: Monitor coding activity over time
- **Tool Usage Patterns**: Identify most-used tools across sessions
- **Model Comparison**: Compare efficiency between different AI models
- **Historical Analysis**: Track trends in code operations by date

---

## 🔧 Version Command

**Check your installation.**

```bash
# Formatted output
vct version

# JSON format
vct version --json

# Plain text
vct version --text
```

### Output

```
🚀 Vibe Coding Tracker

╔════════════════╦═════════╗
║ Version        ║ 0.1.0   ║
╠════════════════╬═════════╣
║ Rust Version   ║ 1.89.0  ║
╠════════════════╬═════════╣
║ Cargo Version  ║ 1.89.0  ║
╚════════════════╩═════════╝
```

---

## 🔄 Update Command

**Keep your installation up-to-date automatically.**

The update command checks GitHub releases and downloads the latest version for your platform.

### Basic Usage

```bash
# Interactive update with confirmation
vct update

# Check for updates without installing
vct update --check

# Force update without confirmation prompt
vct update --force
```

### How It Works

1. **Check Latest Version**: Fetches the latest release from GitHub API
2. **Compare Versions**: Compares current version with the latest available
3. **Download Binary**: Downloads the appropriate binary for your platform (Linux/macOS/Windows)
4. **Smart Replacement**:
   - **Linux/macOS**: Automatically replaces the binary (backs up old version to `.old`)
   - **Windows**: Downloads as `.new` and creates a batch script for safe replacement

### Platform Support

The update command automatically detects your platform and downloads the correct archive:

- **Linux**: `vibe_coding_tracker-v{version}-linux-x64-gnu.tar.gz`, `vibe_coding_tracker-v{version}-linux-arm64-gnu.tar.gz`
- **macOS**: `vibe_coding_tracker-v{version}-macos-x64.tar.gz`, `vibe_coding_tracker-v{version}-macos-arm64.tar.gz`
- **Windows**: `vibe_coding_tracker-v{version}-windows-x64.zip`, `vibe_coding_tracker-v{version}-windows-arm64.zip`

### Windows Update Process

On Windows, the binary cannot be replaced while running. The update command:

1. Downloads the new version as `vct.new`
2. Creates an update script (`update_vct.bat`)
3. Displays instructions to complete the update

Run the batch script after closing the application to finish the update.

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

### Cost Calculation

```
Total Cost = (Input Tokens × Input Cost) +
             (Output Tokens × Output Cost) +
             (Cache Read × Cache Read Cost) +
             (Cache Creation × Cache Creation Cost)
```

---

## 🐳 Docker Support

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# Run with your sessions
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```

---

## 🔍 Troubleshooting

### Pricing Data Not Loading

```bash
# Check cache
ls -la ~/.vibe_coding_tracker/

# Force refresh
rm -rf ~/.vibe_coding_tracker/
vct usage

# Debug mode
RUST_LOG=debug vct usage
```

### No Usage Data Shown

```bash
# Verify session directories
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/
ls -la ~/.gemini/tmp/

# Count session files
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
find ~/.gemini/tmp -name "*.json" | wc -l
```

### Analysis Command Fails

```bash
# Validate JSONL format
jq empty < your-file.jsonl

# Check file permissions
ls -la your-file.jsonl

# Run with debug output
RUST_LOG=debug vct analysis --path your-file.jsonl
```

### Interactive Mode Issues

```bash
# Reset terminal if broken
reset

# Check terminal type
echo $TERM  # Should be xterm-256color or compatible

# Use static table as fallback
vct usage --table
```

---

## ⚡ Performance

Built with Rust for **speed** and **reliability**:

| Operation           | Time   |
| ------------------- | ------ |
| Parse 10MB JSONL    | ~320ms |
| Analyze 1000 events | ~45ms  |
| Load cached pricing | ~2ms   |
| Interactive refresh | ~30ms  |

**Binary Size**: ~3-5 MB (stripped)

---

## 📚 Learn More

- **Developer Docs**: See [.github/copilot-instructions.md](.github/copilot-instructions.md)
- **Report Issues**: [GitHub Issues](https://github.com/Mai0313/VibeCodingTracker/issues)
- **Source Code**: [GitHub Repository](https://github.com/Mai0313/VibeCodingTracker)

---

## 🤝 Contributing

Contributions welcome! Here's how:

1. Fork the repository
2. Create your feature branch
3. Make your changes
4. Submit a pull request

For development setup and guidelines, see [.github/copilot-instructions.md](.github/copilot-instructions.md).

---

## 📄 License

MIT License - see [LICENSE](LICENSE) for details.

---

## 🙏 Credits

- [LiteLLM](https://github.com/BerriAI/litellm) for model pricing data
- Claude Code, Codex, and Gemini teams for creating amazing AI coding assistants
- The Rust community for excellent tooling

---

<center>

**Save money. Track usage. Code smarter.**

[⭐ Star this project](https://github.com/Mai0313/VibeCodingTracker) if you find it useful!

Made with 🦀 Rust

</center>
