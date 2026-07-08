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
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/tree/main?tab=License-1-ov-file)
[![Star on GitHub](https://img.shields.io/github/stars/Mai0313/VibeCodingTracker?style=social&label=Star)](https://github.com/Mai0313/VibeCodingTracker)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

<img src="assets/social-preview.png" alt="Vibe Coding Tracker social preview" width="640">

</div>

**Track your AI coding costs in real-time.** Vibe Coding Tracker is a lightweight, high-performance CLI tool built in Rust that monitors and analyzes your Claude Code, Codex, Copilot, Gemini, OpenCode, and Cursor usage — with detailed cost breakdowns, token statistics, and code operation insights, all while keeping the memory footprint minimal.

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> Note: CLI examples use the short alias `vct`. If you installed via npm/pip/cargo, the binary might be named `vibe_coding_tracker` or `vct`. Create an alias or replace `vct` with the full name when running commands if needed.

---

## Why Vibe Coding Tracker?

### Know Your Costs

Stop wondering how much your AI coding sessions cost. Get **real-time cost tracking** with automatic pricing updates from [LiteLLM](https://github.com/BerriAI/litellm).

### Ultra-Lightweight

Built with Rust for minimal resource footprint. The interactive TUI dashboard typically sits at **under ~50 MB of resident memory** once the first refresh is done, even with hundreds of long-context sessions on disk — no Electron, no bloated runtimes. The usage path parses each session file in a lean usage-only mode and bypasses the cache, and we tune glibc's arena count at startup to keep long-running RSS honest.

### Beautiful Visualizations

Choose your preferred view:

- **Interactive Dashboard**: Auto-refreshing terminal UI with live updates, scrollable model list (arrow keys), and compact K/M/B number formatting
- **Static Reports**: Professional tables for documentation
- **Script-Friendly**: Plain text and JSON for automation
- **Full Precision**: Export exact costs for accounting

### Zero Configuration

Automatically detects and processes logs from Claude Code, Codex, Copilot, Gemini, OpenCode, and Cursor. No setup required — just run and analyze.

### Rich Insights

- Token usage by model and date
- Cost breakdown by cache types (read / create)
- File operations tracking (edit, read, write lines)
- Tool call history (Bash, Edit, Read, Write, TodoWrite)
- Per-provider totals

---

## Key Features

| Feature               | Description                                                                  |
| --------------------- | ---------------------------------------------------------------------------- |
| **Multi-Provider**    | Claude Code, Codex, Copilot, Gemini, OpenCode, and Cursor — all in one place |
| **Smart Pricing**     | Fuzzy model matching + daily cache from LiteLLM                              |
| **4 Display Modes**   | Interactive TUI, static table, plain text, and JSON                          |
| **Dual Analysis**     | Token/cost stats (`usage`) + code operation stats (`analysis`)               |
| **Live Quota Panels** | Live remaining quota for Claude, Codex, Copilot, and Cursor                  |
| **Ultra-Lightweight** | Under ~50 MB RSS in the TUI, streaming JSONL parse — built with Rust         |
| **Live Updates**      | Auto-refreshing dashboard (every 10s) with change highlighting               |

---

## Quick Start

### Installation

Choose the installation method that works best for you:

> **Developers**: If you want to build from source or contribute to development, please see [CONTRIBUTING.md](.github/CONTRIBUTING.md).

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

# Run without installing, straight from PyPI (uv)
uvx vibe_coding_tracker usage
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

## Command Guide

### Quick Reference

```
vct <COMMAND> [OPTIONS]
# Replace with `vibe_coding_tracker` if you are using the full binary name

Commands:
  analysis    Analyze JSONL conversation files (single file or all sessions)
  usage       Display token usage statistics
  version     Display version information
  update      Update to the latest version from GitHub releases
  fetch       Fetch a provider's raw quota/usage API response
  help        Print this message or the help of the given subcommand(s)
```

Time range flags (shared by `usage` and `analysis`, mutually exclusive, default `--all`):

| Flag          | Window                            |
| ------------- | --------------------------------- |
| `--daily`     | Sessions modified today           |
| `--weekly`    | Current ISO week (Monday → today) |
| `--monthly`   | Current calendar month            |
| `-a`, `--all` | Every session on disk (default)   |

---

## Usage Command

**Track your spending across all AI coding sessions.**

### Flags

| Flag                                           | Purpose                                                                          |
| ---------------------------------------------- | -------------------------------------------------------------------------------- |
| *(none)*                                       | Interactive TUI dashboard (default)                                              |
| `--table`                                      | Static table, no TUI                                                             |
| `--text`                                       | Plain text, script-friendly                                                      |
| `--json`                                       | JSON with enriched pricing metadata                                              |
| `--output <FILE>`                              | Save enriched JSON to a file                                                     |
| `--merge-providers`                            | Merge models sharing a base name across provider prefixes (ignored for `--json`) |
| `--daily` / `--weekly` / `--monthly` / `--all` | Time range filter (see table above)                                              |

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

# Save enriched JSON straight to a file
vct usage --output report.json

# Combine time range with output format
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily

# Merge same model reported under different provider prefixes
# (e.g. openai/gpt-5.5 + azure/gpt-5.5 + gpt-5.5 -> one row)
vct usage --table --merge-providers
```

> [!NOTE]
> Model rows are sorted by cost in ascending order, so the highest-spending model is listed last (right above the `TOTAL` row in `--table`). This applies to the interactive dashboard, `--table`, and `--text` output; `--json` preserves the same order. The interactive dashboard also hides models with zero usage in the selected range.

> [!TIP]
> The same model can show up as several rows when it is routed under different provider prefixes (`openai/gpt-5.5`, `azure/gpt-5.5`, plain `gpt-5.5`). `--merge-providers` collapses rows that share the base name after the first `/` (versions like `gpt-5.5` vs `gpt-5.4` stay separate) and sums their already-priced cost. In the interactive dashboard, press `m` to toggle it live; `--merge-providers` opens the dashboard already merged. `--json` is left as the raw per-model export.

### Preview: Interactive Dashboard (`vct usage`)

```
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Model                         Input   Output   Cache Read  Cache Write    Total  Cost (USD) │
│                                                                                             │
│ gemini-3.1-pro-preview         129K    10.3K        67.4K            0     207K       $0.40 │
│ claude-haiku-4-5-20251001     5.57K    19.8K        4.63M         620K    5.27M       $1.34 │
│ claude-opus-4-8               25.7K     179K        40.8M        2.57M    43.6M      $77.59 │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Provider                        Tokens        Cost                                          │
│                                                                                             │
│ Claude                           48.9M      $78.93                                          │
│ Gemini                            207K       $0.40                                          │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Total Cost: $79.33  |  Total Tokens: 49.3M  |  Models: 3  |  Memory: 42.8 MB                │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  PgUp/PgDn page  g/G top/end  m merge  r refresh  q quit  |  ★ github.com/Mai0313/VibeCodingTracker
```

### Preview: Table & JSON (`vct usage`)

`--table` prints the same numbers as a static report with a per-provider summary; `--json` emits one enriched row per model (each with `cost_usd`) for scripting.

```text
Token Usage Statistics

┌───────────────────────────┬─────────┬─────────┬─────────────┬─────────────┬──────────────┬────────────┐
│ Model                     ┆   Input ┆  Output ┆  Cache Read ┆ Cache Write ┆ Total Tokens ┆ Cost (USD) │
╞═══════════════════════════╪═════════╪═════════╪═════════════╪═════════════╪══════════════╪════════════╡
│ opencode/gemini-3.5-flash ┆  19,421 ┆     254 ┆           0 ┆           0 ┆       19,675 ┆      $0.03 │
│ gpt-5.5                   ┆ 242,227 ┆  16,229 ┆   2,406,912 ┆           0 ┆    2,665,368 ┆      $5.56 │
│ claude-opus-4-8           ┆ 401,937 ┆ 936,186 ┆ 138,099,926 ┆   6,057,836 ┆  145,495,885 ┆    $151.29 │
│ TOTAL                     ┆ 663,585 ┆ 952,669 ┆ 140,506,838 ┆   6,057,836 ┆  148,180,928 ┆    $156.88 │
└───────────────────────────┴─────────┴─────────┴─────────────┴─────────────┴──────────────┴────────────┘

Totals (by Provider)

┌───────────────┬─────────────┬─────────┐
│ Provider      ┆      Tokens ┆    Cost │
╞═══════════════╪═════════════╪═════════╡
│ Claude        ┆ 145,495,885 ┆ $151.29 │
│ Codex         ┆   2,665,368 ┆   $5.56 │
│ OpenCode      ┆      19,675 ┆   $0.03 │
│ All Providers ┆ 148,180,928 ┆ $156.88 │
└───────────────┴─────────────┴─────────┘
```

```json
// vct usage --json  (one model shown; rows are sorted by cost)
[
  {
    "model": "claude-opus-4-8",
    "cost_usd": 151.29,
    "usage": {
      "input_tokens": 401937,
      "output_tokens": 936186,
      "cache_read_input_tokens": 138099926,
      "cache_creation_input_tokens": 6057836,
      "reasoning_output_tokens": 0
    }
  }
]
```

### What It Scans

The tool automatically scans these directories:

- `~/.claude/projects/**/*.jsonl` (Claude Code — recursive, includes subagent logs)
- `~/.codex/sessions/**/*.jsonl` (Codex — recursive, includes daily subdirectories)
- `~/.copilot/session-state/<sessionId>/events.jsonl` (Copilot CLI)
- `~/.gemini/tmp/<project_hash>/chats/*.jsonl` (Gemini CLI)
- `~/.local/share/opencode/opencode.db` (OpenCode — SQLite database; honors `$XDG_DATA_HOME`)
- `~/.cursor/chats/*/*/store.db` (Cursor — SQLite chat stores, for `analysis`) and Cursor's dashboard usage API (for `usage` tokens + cost, via the local session token; approximated from local context data when offline)

### Live Quota Panels

`vct usage` shows **live remaining quota for Claude Code, Codex, GitHub Copilot, and Cursor right in the dashboard — with zero setup.** No status-line hook, no config file: vct reads each provider's own credentials, calls its usage API on a background thread, and keeps the panels current while you work.

```
┌ Claude ─────────────────┐┌ Codex ──────────────────┐┌ Copilot ────────────────┐┌ Cursor ─────────────────┐
│ Plan: max 20x           ││ Plan: plus              ││ Plan: individual        ││ Plan: free              │
│ 5h    ▰▱▱▱▱  13% ↻ 1h42m││ 5h    ▰▰▱▱▱  33% ↻ 12m  ││ prem  ▰▱▱▱▱   3% ↻ 24d  ││ total ▰▱▱▱▱   6% ↻ 16d  │
│ 7d    ▰▰▰▱▱  58% ↻ 1d23h││ 7d    ▰▰▱▱▱  36% ↻ 1h54m││ reqs  ▰▱▱▱▱ 45/1500     ││ auto  ▱▱▱▱▱   0% ↻ 16d  │
│ Fable ▰▰▰▰▱  79% ↻ 1d23h││ Credits: 0  +3 reset    ││ updated just now        ││ api   ▰▰▰▱▱  56% ↻ 16d  │
│ Balance: -   $0.00 used ││ updated just now        ││                         ││ updated just now        │
└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘
```

- **Claude** — plan tier, 5-hour, weekly, and per-model weekly usage from the official OAuth usage API (`GET /api/oauth/usage`), read from `~/.claude/.credentials.json`, plus your credit balance. Polled about once a minute to stay under the endpoint's rate limit; a red `LIMIT` flag appears in the title when a cap is hit. The per-model weekly row is best-effort and simply hides when that scope is not returned.
- **Codex** — plan tier, 5-hour and weekly usage, and credit balance from the ChatGPT backend (`wham/usage`) using `~/.codex/auth.json` (with approximate remaining messages / spend cap when applicable); falls back to the newest `rate_limits` in your Codex session logs when the API is unavailable (the title shows `Codex` vs `Codex (session)`).
- **Copilot** — plan tier plus your premium-request quota, shown as two gauges: percent used and the used / total request count (e.g. `45/1500`), from GitHub's Copilot API (`GET /copilot_internal/user`), read from `~/.copilot/config.json`. The request impersonates the Copilot CLI. The token is long-lived, so there is no refresh; a `401` / `403` shows a `run: copilot login` hint.
- **Cursor** — plan tier, total / auto / API percent **used**, and on-demand spend from cursor.com (`GET /api/usage-summary`), using the session token in `~/.config/cursor/auth.json`. Refresh is reactive: vct re-reads the file each poll and uses the token while it is valid, since the official Cursor client keeps it fresh.

**Automatic token refresh.** For Claude and Codex, when a token is near expiry or rejected, vct refreshes it and writes the new token back to the provider's own credential file (in that CLI's exact format), so a token is reused across checks rather than refreshed every time. If a refresh cannot proceed, the panel shows a `run: <provider> auth login` hint instead of breaking. Copilot (long-lived token) and Cursor (kept fresh by its own client) are read-only — vct never writes their credential files.

A panel appears only for a provider whose credentials are present. When four panels are shown the Provider Usage table folds out of the band, and at narrow widths the panels wrap to a 2×2 grid. Quota panels appear only in the interactive TUI; `--table`, `--text`, and `--json` are unchanged.

> **Platform note:** on macOS, Claude Code stores its OAuth credentials in the system Keychain rather than `~/.claude/.credentials.json`, so the Claude panel is not shown on macOS. Cursor's `~/.config/cursor` credential path is Linux-oriented.

---

## Analysis Command

**Deep dive into code operations — see exactly what your AI assistant did.**

### Flags

| Flag                                           | Purpose                                                     |
| ---------------------------------------------- | ----------------------------------------------------------- |
| *(none)*                                       | Interactive TUI dashboard over all sessions                 |
| `--path <FILE>`                                | Analyze a single JSONL/JSON conversation file (prints JSON) |
| `--table`                                      | Static table with per-provider totals                       |
| `--text`                                       | Plain text, script-friendly                                 |
| `--json`                                       | JSON array of aggregated rows printed to stdout             |
| `--output <FILE>`                              | Save results as pretty-printed JSON                         |
| `--daily` / `--weekly` / `--monthly` / `--all` | Time range filter (see table above)                         |

See [`examples/`](examples/) for sample inputs and matching JSON outputs for all four providers.

### Basic Usage

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with per-provider totals
vct analysis --table

# Plain text for scripts
vct analysis --text

# JSON of aggregated rows for data processing
vct analysis --json

# Analyze a single conversation file → stdout JSON
vct analysis --path ~/.claude/projects/session.jsonl

# Save results to JSON
vct analysis --output report.json

# Combine time range with output format
vct analysis --weekly
vct analysis --table --monthly
vct analysis --json --daily
vct analysis --output today.json --daily
```

### Preview: Interactive Dashboard (`vct analysis`)

```
┌─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Model                        Edit Lines   Read Lines  Write Lines   Bash   Edit   Read  TodoWrite  Write        │
│                                                                                                                 │
│ claude-haiku-4-5-20251001             0            0            0     43      0     59          0      0        │
│ claude-opus-4-8                   1.28K        13.3K        1.58K     82    146    209         18     62        │
│ gemini-3.1-pro-preview                0            0            0      0      0      0          0      0        │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Provider                     Edit Lines   Read Lines  Write Lines   Bash   Edit   Read  TodoWrite  Write   Days │
│                                                                                                                 │
│ Claude                            1.28K        13.3K        1.58K    125    146    268         18     62      3 │
│ Gemini                                0            0            0      0      0      0          0      0      1 │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Total Lines: 16.1K  |  Total Tools: 619  |  Models: 3  |  Memory: 41.2 MB                                       │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  PgUp/PgDn page  g/G top/end  r refresh  q quit  |  ★ github.com/Mai0313/VibeCodingTracker
```

### Preview: Table & JSON (`vct analysis`)

`--table` renders the per-model breakdown plus a per-provider summary (with an Active Days column); `--json` emits one aggregated row per model.

```text
Analysis Statistics

┌─────────────────┬────────────┬────────────┬─────────────┬──────┬──────┬──────┬───────────┬───────┐
│ Model           ┆ Edit Lines ┆ Read Lines ┆ Write Lines ┆ Bash ┆ Edit ┆ Read ┆ TodoWrite ┆ Write │
╞═════════════════╪════════════╪════════════╪═════════════╪══════╪══════╪══════╪═══════════╪═══════╡
│ gpt-5.5         ┆          0 ┆      3,087 ┆           0 ┆   25 ┆    0 ┆   10 ┆         0 ┆     0 │
│ claude-opus-4-8 ┆      1,493 ┆     15,564 ┆         970 ┆  123 ┆  134 ┆  144 ┆         0 ┆    12 │
│ TOTAL           ┆      1,493 ┆     18,651 ┆         970 ┆  148 ┆  134 ┆  154 ┆         0 ┆    12 │
└─────────────────┴────────────┴────────────┴─────────────┴──────┴──────┴──────┴───────────┴───────┘
```

```json
// vct analysis --json  (one model shown)
[
  {
    "model": "claude-opus-4-8",
    "editLines": 1493,
    "readLines": 15564,
    "writeLines": 970,
    "bashCount": 124,
    "editCount": 134,
    "readCount": 144,
    "todoWriteCount": 0,
    "writeCount": 12
  }
]
```

---

## Update Command

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
Current version: v1.3.0
Checking for latest release...
Latest version: v1.3.0 — you are up to date!
```

---

## Version Command

Report the embedded build metadata (binary version, Rust toolchain, Cargo version):

```bash
vct version          # Pretty table
vct version --text   # One-field-per-line, script-friendly
vct version --json   # Machine-readable JSON
```

```text
┌───────────────┬──────────┐
│ Version       ┆ 1.3.0    │
│ Rust Version  ┆ 1.96.0   │
│ Cargo Version ┆ 1.96.0   │
└───────────────┴──────────┘
```

The binary version is produced at build time by `build.rs` from `git describe`, so development builds include commit count + short SHA + `dirty` suffix when applicable.

---

## Fetch Command

**Print a provider's raw quota/usage API response — no parsing, no aggregation.**

Calls the same quota endpoint the `usage` dashboard uses (Claude / Codex / Copilot / Cursor) exactly once and prints the raw body, so you can inspect the exact API shape or sanity-check your credentials. It reads each provider's stored credentials and does **not** refresh tokens: if a token is expired, re-auth with that provider's own CLI (`claude` / `codex` / `copilot` / `cursor-agent`).

### Flags

| Flag      | Purpose                                       |
| --------- | --------------------------------------------- |
| *(none)*  | Pretty JSON (default)                         |
| `--json`  | Pretty JSON                                   |
| `--text`  | Flattened `key: value` lines, script-friendly |
| `--table` | Flattened Field / Value table                 |

### Basic Usage

```bash
# Raw JSON (default)
vct fetch claude
vct fetch codex
vct fetch copilot
vct fetch cursor

# Flattened plain text
vct fetch codex --text

# Flattened key/value table
vct fetch copilot --table
```

> [!NOTE]
> The response body is printed to stdout as-is. On an HTTP error the body is still printed and the process exits non-zero; a 401/403 additionally prints a `run: <cli> login` hint on stderr.

---

## Smart Pricing System

### How It Works

1. **Automatic Updates**: Fetches pricing from [LiteLLM](https://github.com/BerriAI/litellm) daily
2. **Smart Caching**: Stores pricing in `~/.vct/` for 24 hours
3. **Fuzzy Matching**: Finds best match even for custom model names
4. **Always Accurate**: Ensures you get the latest pricing

### Model Matching

**Priority Order**:

1. **Exact Match**: `claude-sonnet-4` → `claude-sonnet-4`
2. **Normalized**: `claude-sonnet-4-20250514` → `claude-sonnet-4`
3. **Substring**: `custom-gpt-4` → `gpt-4`
4. **Fuzzy (AI-powered)**: Uses Jaro-Winkler similarity (70% threshold)
5. **Fallback**: Shows $0.00 if no match found

### Cost Details

- **Beyond tokens**: Claude web-search tool calls (`server_tool_use.web_search_requests`) are billed per query on top of the token cost; every other model's per-query charge is $0.
- **OpenCode**: a novel model name is priced from its tokens only on an **exact** LiteLLM match; with no exact match, vct trusts the assistant message's own stored cost instead of guessing from a loosely-similar name.
- **Cache is raw**: the daily cache stores the filtered upstream LiteLLM JSON (not a derived shape), so tiered / batch pricing stays available without re-fetching, and a small in-process LRU keeps repeated lookups cheap during a TUI refresh.

---

## Docker Support

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .
```
