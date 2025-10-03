<center>

# CodexUsage — AI Coding Assistant Usage Tracker

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/codex_usage/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/codex_usage/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/codex_usage/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/codex_usage/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/codex_usage/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/codex_usage/pulls)

</center>

**Track your AI coding costs in real-time.** CodexUsage is a powerful CLI tool that helps you monitor and analyze your Claude Code and Codex usage, providing detailed cost breakdowns, token statistics, and code operation insights.

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

---

## 🎯 Why CodexUsage?

### 💰 Know Your Costs
Stop wondering how much your AI coding sessions cost. Get **real-time cost tracking** with automatic pricing updates from [LiteLLM](https://github.com/BerriAI/litellm).

### 📊 Beautiful Visualizations
Choose your preferred view:
- **Interactive Dashboard**: Auto-refreshing terminal UI with live updates
- **Static Reports**: Professional tables for documentation
- **Script-Friendly**: Plain text and JSON for automation
- **Full Precision**: Export exact costs for accounting

### 🚀 Zero Configuration
Automatically detects and processes logs from both Claude Code and Codex. No setup required—just run and analyze.

### 🎨 Rich Insights
- Token usage by model and date
- Cost breakdown by cache types
- File operations tracking
- Command execution history
- Git repository information

---

## ✨ Key Features

| Feature | Description |
|---------|-------------|
| 🤖 **Auto-Detection** | Intelligently identifies Claude Code or Codex logs |
| 💵 **Smart Pricing** | Fuzzy model matching + daily cache for speed |
| 🎨 **4 Display Modes** | Interactive, Table, Text, and JSON outputs |
| 📈 **Comprehensive Stats** | Tokens, costs, file ops, and tool calls |
| ⚡ **High Performance** | Built with Rust for speed and reliability |
| 🔄 **Live Updates** | Real-time dashboard refreshes every second |
| 💾 **Efficient Caching** | Smart daily cache reduces API calls |

---

## 🚀 Quick Start

### Installation

**Prerequisites**: [Rust toolchain](https://rustup.rs/) (1.70+)

```bash
# Clone and build
git clone https://github.com/Mai0313/codex_usage.git
cd CodexUsage
cargo build --release

# Binary location: ./target/release/codex_usage
```

### First Run

```bash
# View your usage with interactive dashboard
./target/release/codex_usage usage

# Or analyze a specific conversation
./target/release/codex_usage analysis --path ~/.claude/projects/session.jsonl
```

---

## 📖 Command Guide

### 🔍 Quick Reference

```bash
codex_usage <COMMAND> [OPTIONS]

Commands:
  usage       Show token usage and costs (default: interactive)
  analysis    Analyze conversation files and export data
  version     Display version information
  help        Show help information
```

---

## 💰 Usage Command

**Track your spending across all AI coding sessions.**

### Basic Usage

```bash
# Interactive dashboard (recommended)
codex_usage usage

# Static table for reports
codex_usage usage --table

# Plain text for scripts
codex_usage usage --text

# JSON for data processing
codex_usage usage --json
```

### What You Get

The tool scans these directories automatically:
- `~/.claude/projects/*.jsonl` (Claude Code)
- `~/.codex/sessions/*.jsonl` (Codex)

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

Press 'q', 'Esc', or 'Ctrl+C' to quit
```

**Features**:
- ✨ Auto-refreshes every second
- 🎯 Highlights today's entries
- 🔄 Shows recently updated rows
- 💾 Displays memory usage
- 📊 Summary statistics

**Controls**: Press `q`, `Esc`, or `Ctrl+C` to exit

### 📋 Static Table Mode

**Perfect for documentation and reports**

```bash
codex_usage usage --table
```

```
📊 Token Usage Statistics

╔════════════╦══════════════════════╦════════════╦════════════╦════════════╦══════════════╦══════════════╦════════════╗
║ Date       ║ Model                ║ Input      ║ Output     ║ Cache Read ║ Cache Create ║ Total Tokens ║ Cost (USD) ║
╠════════════╬══════════════════════╬════════════╬════════════╬════════════╬══════════════╬══════════════╬════════════╣
║ 2025-10-01 ║ claude-sonnet-4-20…  ║ 45,230     ║ 12,450     ║ 230,500    ║ 50,000       ║ 338,180      ║ $2.15      ║
║ 2025-10-02 ║ claude-sonnet-4-20…  ║ 32,100     ║ 8,920      ║ 180,000    ║ 30,000       ║ 251,020      ║ $1.58      ║
║ 2025-10-03 ║ claude-sonnet-4-20…  ║ 28,500     ║ 7,200      ║ 150,000    ║ 25,000       ║ 210,700      ║ $1.32      ║
║            ║ TOTAL                ║ 105,830    ║ 28,570     ║ 560,500    ║ 105,000      ║ 799,900      ║ $5.05      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝
```

### 📝 Text Mode

**Ideal for scripting and parsing**

```bash
codex_usage usage --text
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
codex_usage usage --json
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
      "cost_usd": 2.1542304567890123
    }
  ]
}
```

### 🔍 Output Comparison

| Feature | Interactive | Table | Text | JSON |
|---------|-------------|-------|------|------|
| **Best For** | Monitoring | Reports | Scripts | Integration |
| **Cost Format** | $2.15 | $2.15 | $2.154230 | 2.1542304567890123 |
| **Updates** | Real-time | Static | Static | Static |
| **Colors** | ✅ | ✅ | ❌ | ❌ |
| **Parseable** | ❌ | ❌ | ✅ | ✅ |

### 💡 Use Cases

- **Budget Tracking**: Monitor your daily AI spending
- **Cost Optimization**: Identify expensive sessions
- **Team Reporting**: Generate usage reports for management
- **Billing**: Export precise costs for invoicing
- **Monitoring**: Real-time dashboard for active development

---

## 📊 Analysis Command

**Deep dive into specific conversation files.**

### Basic Usage

```bash
# Analyze and display
codex_usage analysis --path ~/.claude/projects/session.jsonl

# Save to file
codex_usage analysis --path ~/.claude/projects/session.jsonl --output report.json
```

### What You Get

Detailed JSON report including:
- **Token Usage**: Input, output, and cache statistics by model
- **File Operations**: Every read, write, and edit with full details
- **Command History**: All shell commands executed
- **Tool Usage**: Counts of each tool type used
- **Metadata**: User, machine ID, Git repo, timestamps

### Sample Output

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

### 💡 Use Cases

- **Usage Auditing**: Track what the AI did in each session
- **Cost Attribution**: Calculate costs per project or feature
- **Compliance**: Export detailed activity logs
- **Analysis**: Understand coding patterns and tool usage

---

## 🔧 Version Command

**Check your installation.**

```bash
# Formatted output
codex_usage version

# JSON format
codex_usage version --json

# Plain text
codex_usage version --text
```

### Output

```
🚀 Codex Usage Analyzer

╔════════════════╦═════════╗
║ Version        ║ 0.1.0   ║
╠════════════════╬═════════╣
║ Rust Version   ║ 1.89.0  ║
╠════════════════╬═════════╣
║ Cargo Version  ║ 1.89.0  ║
╚════════════════╩═════════╝
```

---

## 💡 Smart Pricing System

### How It Works

1. **Automatic Updates**: Fetches pricing from [LiteLLM](https://github.com/BerriAI/litellm) daily
2. **Smart Caching**: Stores pricing in `~/.codex-usage/` for 24 hours
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
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .

# Run with your sessions
docker run --rm \
  -v ~/.claude:/root/.claude \
  -v ~/.codex:/root/.codex \
  codex_usage:latest usage
```

---

## 🔍 Troubleshooting

### Pricing Data Not Loading

```bash
# Check cache
ls -la ~/.codex-usage/

# Force refresh
rm -rf ~/.codex-usage/
codex_usage usage

# Debug mode
RUST_LOG=debug codex_usage usage
```

### No Usage Data Shown

```bash
# Verify session directories
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/

# Count JSONL files
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
```

### Analysis Command Fails

```bash
# Validate JSONL format
jq empty < your-file.jsonl

# Check file permissions
ls -la your-file.jsonl

# Run with debug output
RUST_LOG=debug codex_usage analysis --path your-file.jsonl
```

### Interactive Mode Issues

```bash
# Reset terminal if broken
reset

# Check terminal type
echo $TERM  # Should be xterm-256color or compatible

# Use static table as fallback
codex_usage usage --table
```

---

## ⚡ Performance

Built with Rust for **speed** and **reliability**:

| Operation | Time |
|-----------|------|
| Parse 10MB JSONL | ~320ms |
| Analyze 1000 events | ~45ms |
| Load cached pricing | ~2ms |
| Interactive refresh | ~30ms |

**Binary Size**: ~3-5 MB (stripped)

---

## 📚 Learn More

- **Developer Docs**: See [.github/copilot-instructions.md](.github/copilot-instructions.md)
- **Report Issues**: [GitHub Issues](https://github.com/Mai0313/codex_usage/issues)
- **Source Code**: [GitHub Repository](https://github.com/Mai0313/codex_usage)

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
- Claude Code and Codex teams for creating amazing AI coding assistants
- The Rust community for excellent tooling

---

<center>

**Save money. Track usage. Code smarter.**

[⭐ Star this project](https://github.com/Mai0313/codex_usage) if you find it useful!

Made with 🦀 Rust

</center>
