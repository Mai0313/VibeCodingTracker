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

**Track your AI coding costs in real-time.** Vibe Coding Tracker is a lightweight, high-performance CLI tool built in Rust that monitors and analyzes your Claude Code, Codex, Copilot, Gemini, OpenCode, Cursor, Hermes, and Grok usage — with detailed cost breakdowns, token statistics, and code operation insights, all while keeping the memory footprint minimal.

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

Automatically detects and processes logs from Claude Code, Codex, Copilot, Gemini, OpenCode, Cursor, Hermes, and Grok. No setup required — just run and analyze. A `~/.vct/config.toml` is created with sensible defaults on first run if you ever want to tweak behavior (see [Configuration](#configuration)).

### Rich Insights

- Token usage by model and date
- Cost breakdown by cache types (read / create)
- File operations tracking (edit, read, write lines)
- Tool call history (Bash, Edit, Read, Write, TodoWrite)
- Per-provider totals

---

## Key Features

| Feature               | Description                                                             |
| --------------------- | ----------------------------------------------------------------------- |
| **Multi-Provider**    | Claude Code, Codex, Copilot, Gemini, OpenCode, Cursor, Hermes, and Grok |
| **Smart Pricing**     | Fuzzy model matching + daily cache from LiteLLM                         |
| **4 Display Modes**   | Interactive TUI, static table, plain text, and JSON                     |
| **Dual Analysis**     | Token/cost stats (`usage`) + code operation stats (`analysis`)          |
| **Live Quota Panels** | Live remaining quota for Claude, Codex, Copilot, and Cursor             |
| **Ultra-Lightweight** | Under ~50 MB RSS in the TUI, streaming session parse — built with Rust  |
| **Live Updates**      | Auto-refreshing dashboard (every 10s) with change highlighting          |

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
  analysis    Analyze local session data (single file or all sessions)
  usage       Display token usage statistics
  version     Display version information
  update      Update to the latest version from GitHub releases
  fetch       Fetch a provider's raw quota/usage API response
  config      Show or edit the persistent settings file (~/.vct/config.toml)
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

# Save enriched JSON with shell redirection
vct usage --json > report.json

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
> The same model can show up as several rows when it is routed under different provider prefixes (`openai/gpt-5.5`, `azure/gpt-5.5`, plain `gpt-5.5`). `--merge-providers` collapses rows that share the base name after the first `/` (versions like `gpt-5.5` vs `gpt-5.4` stay separate) and sums their already-priced cost. In the interactive dashboard, press `m` to toggle it live (the choice is saved to `~/.vct/config.toml`, so the next launch remembers it); `--merge-providers` opens the dashboard already merged. `--json` is left as the raw per-model export.

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
  ↑/↓ scroll  m merge  r refresh  q quit  |  Star on GitHub
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
- `~/.cursor/chats/*/*/store.db` (Cursor — SQLite chat stores, used for `analysis` and a local `usage` estimate consistent with the other providers)
- `~/.hermes/state.db` (Hermes — SQLite database, honors `$HERMES_HOME`; `usage` only)
- `$GROK_HOME/sessions/*/*/signals.json` (Grok CLI — defaults to `~/.grok`; sibling `updates.jsonl` supplies `analysis` data)

Grok `usage` is one point-in-time local context estimate: vct records `signals.json`'s `contextTokensUsed` as cache-read tokens and estimates cost at the model's cache-read price. It is not cumulative billed usage. `analysis` reconstructs completed Read / Write / Edit / Bash / TodoWrite operations from the sibling `updates.jsonl`. Grok does not support quota panels or `vct fetch`.

### Live Quota Panels

`vct usage` shows **live remaining quota for Claude Code, Codex, GitHub Copilot, and Cursor right in the dashboard — with zero setup.** No status-line hook, no credentials to enter: vct reads each provider's own credentials, calls its usage API on a background thread, and keeps the panels current while you work. (Prefer a quieter dashboard? Trim `panels` under `[usage.quota]` in [`config.toml`](#configuration), or set it to `[]` to hide the band.)

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

### Arguments and Flags

| Argument / Flag                                | Purpose                                                                                  |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------- |
| *(none)*                                       | Interactive TUI dashboard over all sessions                                              |
| `<FILE>`                                       | Analyze one JSONL/JSON session file and print its complete `CodeAnalysis` JSON           |
| `--table`                                      | Static summary table with per-provider totals                                            |
| `--text`                                       | Plain-text summary, script-friendly                                                      |
| `--json`                                       | Complete parser results as JSON: one object for `<FILE>`, otherwise an array of objects  |
| `--daily` / `--weekly` / `--monthly` / `--all` | Time range filter for all-session analysis (see table above; not accepted with `<FILE>`) |

See [`examples/`](examples/) for sample inputs and matching JSON outputs for the four JSONL providers, plus the Grok session fixture under [`examples/grok_session/`](examples/grok_session/).

### Basic Usage

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with per-provider totals
vct analysis --table

# Plain text for scripts
vct analysis --text

# Complete parser results for every session
vct analysis --json

# Analyze a single conversation file → stdout JSON
vct analysis ~/.claude/projects/session.jsonl

# Summarize only that conversation
vct analysis ~/.claude/projects/session.jsonl --table

# Save complete JSON with shell redirection
vct analysis --json > report.json
vct analysis ~/.claude/projects/session.jsonl > session-analysis.json

# Combine time range with output format
vct analysis --weekly
vct analysis --table --monthly
vct analysis --json --daily
vct analysis --json --daily > today.json
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
  ↑/↓ scroll  r refresh  q quit  |  Star on GitHub
```

### Preview: Table & JSON (`vct analysis`)

`--table` renders the per-model breakdown plus a per-provider summary (with an Active Days column). `--text` and `--table` are compact projections of the same normalized parser records. `--json` keeps the complete records, including per-operation details and token usage. With no `<FILE>`, the outer array contains one `CodeAnalysis` object per session; with `<FILE>`, stdout is that single object and matches the corresponding shape under [`examples/`](examples/).

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

```jsonc
// vct analysis --json  (one abbreviated session shown)
[
  {
    "user": "alice",
    "extensionName": "Claude-Code",
    "insightsVersion": "...",
    "machineId": "...",
    "records": [
      {
        "totalUniqueFiles": 3,
        "totalReadLines": 120,
        "readFileDetails": [
          {
            "filePath": "/repo/src/main.rs",
            "lineCount": 120,
            "characterCount": 4102,
            "timestamp": 1783872000000
          }
        ],
        "toolCallCounts": { "Bash": 1, "Edit": 0, "Read": 1, "TodoWrite": 0, "Write": 0 },
        "conversationUsage": { "claude-opus-4-8": { "input_tokens": 42, "output_tokens": 18 } }
      }
    ]
  }
]
```

> [!WARNING]
> Complete analysis JSON can be large and may contain source text, edit bodies, shell commands, absolute paths, repository URLs, user names, machine identifiers, and token metadata. Review it before sharing.

Batch analysis reads live provider stores. If an assistant appends to a session during the scan, later runs can legitimately contain newer data. Unchanged inputs serialize in a deterministic order.

Noninteractive analysis returns an error when every discovered source fails or uses an unrecognized schema. If only some sources fail, successful output is preserved and a warning is written to stderr.

`analysis FILE` follows the same rule for malformed or unsupported records inside one file: parsed JSON/text/table output is preserved on stdout and a generic skipped-record warning is written to stderr.

Codex code-mode sessions expose a completed JavaScript `exec` cell but no structured trace for its nested tools. VCT counts that cell as one Bash call and preserves its source in complete JSON, but does not guess nested Read/Edit/Write operations.

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

## Configuration

vct keeps its user settings in `~/.vct/config.toml`. The file is **created with defaults on first run**, so you never have to write it by hand — edit it only when you want to change a default. It is generated from vct's typed settings and carries a `#:schema` directive on the first line, so a schema-aware TOML editor (taplo / VS Code "Even Better TOML") gives you autocomplete and validation. Print the schema yourself with `vct config schema`. A file written by an older vct is upgraded to the current layout in place the next time vct reads it (or on demand with `vct config migrate`), so an upgrade never leaves you on a stale format.

```toml
#:schema https://raw.githubusercontent.com/Mai0313/VibeCodingTracker/main/vct.schema.json

[general]
# Default time range when no --daily/--weekly/--monthly/--all flag is given.
# One of: "daily" | "weekly" | "monthly" | "all".
default_time_range = "all"

[usage]
# Start the usage dashboard with models merged across provider prefixes.
# Toggled live with `m`; the last state is saved back here.
merge_models = false
# Seconds between automatic redraws of the usage TUI (minimum 1).
refresh_interval = 10

[usage.quota]
# Which live quota panels to show. Remove a name to hide that panel; use an
# empty list ([]) to hide the whole band.
panels = ["claude", "codex", "copilot", "cursor"]
# Seconds between live quota-panel polls, shared by every provider (minimum 1).
refresh_interval = 60

[analysis]
# Seconds between automatic redraws of the analysis TUI (minimum 1).
refresh_interval = 10

[providers]
# Include each provider's sessions in usage / analysis. Set a provider to false
# to skip it entirely (no directory scan, no API).
claude = true
codex = true
copilot = true
gemini = true
opencode = true
cursor = true
hermes = true
grok = true

[logging]
# Minimum level written to ~/.vct/logs/vct-YYYY-MM-DD.log.
# One of: "off" | "error" | "warn" | "info" | "debug" | "trace".
level = "warn"
# Days of daily log files to keep; older files are pruned on startup. 0 keeps every file.
retention_days = 7
```

| Setting                        | Effect                                                                                                                       |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- |
| `general.default_time_range`   | Period used when you pass no `--daily/--weekly/--monthly/--all`. An explicit flag always wins.                               |
| `usage.merge_models`           | Seeds the dashboard merged; the `m` toggle saves your last choice back here. `--merge-providers` forces on.                  |
| `usage.refresh_interval`       | Redraw cadence of the `usage` dashboard (seconds).                                                                           |
| `usage.quota.panels`           | Which quota panels to show (`claude` / `codex` / `copilot` / `cursor`); drop a name to hide it, `[]` to hide the whole band. |
| `usage.quota.refresh_interval` | Poll cadence for every live quota panel (seconds); higher is safer against a provider's rate limits.                         |
| `analysis.refresh_interval`    | Redraw cadence of the `analysis` dashboard (seconds).                                                                        |
| `providers.*`                  | Skip a provider entirely (no scan, no API) when `false` — handy if you don't use one.                                        |
| `logging.level`                | Minimum severity written to the log file (`off`..`trace`); never printed to the terminal.                                    |
| `logging.retention_days`       | Days of daily log files to keep; older `vct-*.log` are pruned on startup (`0` keeps all).                                    |

> [!NOTE]
> Cursor `usage` is a **local estimate** from the chat stores, so it behaves like Claude Code / Codex / Copilot / Gemini (all computed from local session files) and needs no network. It undercounts Cursor's real spend, because much of it is billed under Cursor-internal model names the local data cannot price — treat Cursor cost as approximate.

> [!NOTE]
> vct writes diagnostics to `~/.vct/logs/vct-YYYY-MM-DD.log` (plain text, file only — never shown in the dashboard). It stays quiet when healthy (default level `warn`) and the file is created lazily, so a healthy run leaves nothing behind. When a quota fetch fails or a session is skipped, that is where the reason is recorded — bump `logging.level` to `debug` for the full detail.

### Managing the file

```bash
# Print the config file path
vct config path

# Print the current settings
vct config show

# Open the file in $VISUAL / $EDITOR (falls back to vi / notepad)
vct config edit

# Print the JSON schema (regenerate with: vct config schema > vct.schema.json)
vct config schema

# Upgrade a legacy-format file to the current layout in place
vct config migrate
```

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
- **Hermes**: priced the same way as OpenCode — an **exact** LiteLLM match prices from tokens, otherwise vct uses Hermes's own stored cost.
- **Grok**: `contextTokensUsed` is priced as cache-read tokens only; this is a point-in-time local context estimate, not cumulative billed usage.
- **Cache is raw**: the daily cache stores the filtered upstream LiteLLM JSON (not a derived shape), so tiered / batch pricing stays available without re-fetching, and a small in-process LRU keeps repeated lookups cheap during a TUI refresh.

---

## Docker Support

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .
```
