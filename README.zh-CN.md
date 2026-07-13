<div align="center" markdown="1">

# Vibe Coding Tracker — AI 编程助手使用量追踪器

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

**实时追踪你的 AI 编程开销。** Vibe Coding Tracker 是一款基于 Rust 构建的轻量级高性能 CLI 工具，用于监控和分析你在 Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 和 Grok 上的使用情况——提供详细的费用明细、token 统计和代码操作洞察，同时保持极低的内存占用。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：CLI 示例中使用简写别名 `vct`。如果你是通过 npm/pip/cargo 安装的，二进制文件可能命名为 `vibe_coding_tracker` 或 `vct`。如有需要，请创建别名或在运行命令时将 `vct` 替换为完整名称。

---

## 为什么选择 Vibe Coding Tracker？

### 掌握你的开销

不用再猜测 AI 编程会话到底花了多少钱。通过 [LiteLLM](https://github.com/BerriAI/litellm) 自动更新定价，获取**实时费用追踪**。

### 超轻量级

使用 Rust 构建，资源占用极低。交互式 TUI 面板稳定后常驻内存通常控制在 **约 50 MB 以内**，即使磁盘上有数百个长 context session 文件也不例外——无需 Electron，无需臃肿的运行时。usage 路径以精简模式流式解析每个 session 文件并绕过 cache，启动时还会调整 glibc 的 arena 数量，让长时间运行的 RSS 保持诚实。

### 精美的可视化

选择你喜欢的查看方式：

- **交互式面板**：自动刷新的终端 UI，支持实时更新、可滚动的模型列表（方向键）、进程级 CPU/内存实时读数，以及 K/M/B 精简数字格式
- **静态报表**：专业的表格，适合文档记录
- **脚本友好**：纯文本和 JSON 格式，方便自动化
- **完整精度**：导出精确费用，满足财务核算需求

### 零配置

自动检测并处理来自 Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 和 Grok 的日志。无需任何设置——直接运行即可分析。首次运行时会自动生成一个带有合理默认值的 `~/.vct/config.toml`，方便你日后想调整行为时使用（参见 [配置](#%E9%85%8D%E7%BD%AE)）。

### 丰富的洞察

- 按模型和日期统计 token 使用量
- 按 cache 类型（读取/创建）细分费用
- 文件操作追踪（编辑、读取、写入行数）
- 工具调用历史（Bash、Edit、Read、Write、TodoWrite）
- 按供应商统计总计

---

## 核心特性

| 特性             | 说明                                                                  |
| ---------------- | --------------------------------------------------------------------- |
| **多供应商支持** | Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 和 Grok |
| **智能定价**     | 模糊模型匹配 + 从 LiteLLM 每日缓存更新                                |
| **4 种显示模式** | 交互式 TUI、静态表格、纯文本和 JSON                                   |
| **双维度分析**   | token/费用统计（`usage`）+ 代码操作统计（`analysis`）                 |
| **实时额度面板** | Claude、Codex、Copilot 和 Cursor 的实时剩余额度                       |
| **超轻量级**     | TUI 常驻内存 50 MB 以内、流式 session 解析——基于 Rust 构建            |
| **实时更新**     | 面板自动刷新（每 10 秒），并高亮变化                                  |

---

## 快速开始

### 安装

选择最适合你的安装方式：

> **开发者**：如果你想从源码构建或参与项目开发，请参阅 [CONTRIBUTING.md](.github/CONTRIBUTING.md)。

#### 方式一：通过 npm 安装

**前置条件**：[Node.js](https://nodejs.org/) v22 或更高版本

以下包名任选其一（内容完全相同）：

```bash
# Main package
npm install -g vibe-coding-tracker

# Short alias with scope
npm install -g @mai0313/vct

# Full name with scope
npm install -g @mai0313/vibe-coding-tracker
```

#### 方式二：通过 PyPI 安装

**前置条件**：Python 3.8 或更高版本

```bash
pip install vibe_coding_tracker
# Or with uv
uv pip install vibe_coding_tracker

# Run without installing, straight from PyPI (uv)
uvx vibe_coding_tracker usage
```

#### 方式三：通过 crates.io 安装

使用 Cargo 从 Rust 官方包注册中心安装：

```bash
cargo install vibe_coding_tracker
```

### 首次运行

```bash
# View your usage with the interactive dashboard
vct usage

# Or run the binary built by Cargo/pip
vibe_coding_tracker usage

# Analyze code operations across all sessions
vct analysis
```

---

## 命令指南

### 快速参考

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

时间范围 flag（`usage` 与 `analysis` 共用，互斥，默认 `--all`）：

| Flag          | 范围                         |
| ------------- | ---------------------------- |
| `--daily`     | 今天更新过的 session         |
| `--weekly`    | 本 ISO 周（周一 → 今天）     |
| `--monthly`   | 本自然月                     |
| `-a`, `--all` | 磁盘上所有 session（默认值） |

---

## Usage 命令

**追踪你在所有 AI 编程会话中的开销。**

### Flag 一览

| Flag                                           | 用途                                                                          |
| ---------------------------------------------- | ----------------------------------------------------------------------------- |
| *(不带参数)*                                   | 互动式 TUI 面板（默认）                                                       |
| `--table`                                      | 静态表格，不启动 TUI                                                          |
| `--text`                                       | 纯文本，适合脚本处理                                                          |
| `--json`                                       | JSON 输出，附带定价信息                                                       |
| `--merge-providers`                            | 合并共享同一 base 名称、仅 provider 前缀不同的 model（`--json` 会忽略此选项） |
| `--daily` / `--weekly` / `--monthly` / `--all` | 时间范围筛选（见上方表格）                                                    |

### 基本用法

```bash
# Interactive dashboard (recommended)
vct usage

# Static table for reports
vct usage --table

# Plain text for scripts
vct usage --text

# JSON 输出，包含 cost_usd 与 matched_model 字段
vct usage --json

# 通过 shell redirection 保存富化 JSON
vct usage --json > report.json

# 时间范围与输出格式可自由组合
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily

# 合并同一 model 在不同 provider 前缀下的多行
# (例如 openai/gpt-5.5 + azure/gpt-5.5 + gpt-5.5 -> 一行)
vct usage --table --merge-providers
```

> [!NOTE]
> Model 行会按 cost 升序排序，因此花费最高的 model 会排在最后（在 `--table` 中紧邻 `TOTAL` 行上方）。该排序适用于交互式面板、`--table` 与 `--text` 三种输出；`--json` 也会保持相同顺序。交互式面板还会隐藏在所选范围内用量为 0 的模型。

> [!TIP]
> 同一个 model 在不同 provider 前缀下路由时会显示成多行（`openai/gpt-5.5`、`azure/gpt-5.5`、纯 `gpt-5.5`）。`--merge-providers` 会把第一个 `/` 之后 base 名称相同的行合并（`gpt-5.5` 与 `gpt-5.4` 这类版本不同的仍分开），并把它们已定价的 cost 相加。在交互式面板中按 `m` 可实时切换（该选择会保存到 `~/.vct/config.toml`，因此下次启动会记住它）；`--merge-providers` 则让面板一打开就是合并状态。`--json` 保持为逐一 model 的原始导出。

### 预览：交互式面板（`vct usage`）

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
│ Total Cost: $79.33  |  Total Tokens: 49.3M  |  Models: 3  |  Memory: 42.8 MB  |  CPU: 17.9% │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  m merge  r refresh  q quit  |  Star on GitHub
```

### 预览：表格与 JSON（`vct usage`）

`--table` 会以静态报表的形式打印相同的数字，并附带按 provider 的汇总；`--json` 则为每个 model 输出一行富化数据（各自带有 `cost_usd`），方便脚本处理。

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

### 扫描范围

该工具会自动扫描以下目录：

- `~/.claude/projects/**/*.jsonl`（Claude Code，递归包含 subagent 日志）
- `~/.codex/sessions/**/*.jsonl`（Codex，递归包含每日子目录）
- `~/.copilot/session-state/<sessionId>/events.jsonl`（Copilot CLI）
- `~/.gemini/tmp/<project_hash>/chats/*.jsonl`（Gemini CLI）
- `~/.local/share/opencode/opencode.db`（OpenCode，SQLite 数据库；遵循 `$XDG_DATA_HOME`）
- `~/.cursor/chats/*/*/store.db`（Cursor，SQLite 会话库，用于 `analysis`，并给出一个与其他 provider 一致的本地 `usage` 估算）
- `~/.hermes/state.db`（Hermes，SQLite 数据库，遵循 `$HERMES_HOME`；仅 `usage`）
- `$GROK_HOME/sessions/*/*/signals.json`（Grok CLI，默认使用 `~/.grok`；同层的 `updates.jsonl` 提供 `analysis` 数据）

Grok 的 `usage` 是单一时点的本地 context 估算：vct 会把 `signals.json` 的 `contextTokensUsed` 记为 cache-read token，并按该 model 的 cache-read 费率估算费用。这不是累计的 billed usage。`analysis` 会从同层的 `updates.jsonl` 还原已完成的 Read / Write / Edit / Bash / TodoWrite 操作。Grok 不支持 quota panel 或 `vct fetch`。

### 实时额度面板

`vct usage` 会**在仪表盘中直接显示 Claude Code、Codex、GitHub Copilot 与 Cursor 的实时剩余额度——完全零配置。** 不需要 status-line hook，也无需手动输入任何凭证：vct 会读取各 provider 自己的 OAuth 凭证，在后台线程调用其用量 API，并在你工作时保持面板持续更新。（想要更清爽的面板？在 [`config.toml`](#%E9%85%8D%E7%BD%AE) 中精简 `[usage.quota]` 下的 `panels`，或设为 `[]` 隐藏整栏。）

```
┌ Claude ─────────────────┐┌ Codex ──────────────────┐┌ Copilot ────────────────┐┌ Cursor ─────────────────┐
│ Plan: max 20x           ││ Plan: plus              ││ Plan: individual        ││ Plan: free              │
│ 5h    ▰▱▱▱▱  13% ↻ 1h42m││ 5h    ▰▰▱▱▱  33% ↻ 12m  ││ prem  ▰▱▱▱▱   3% ↻ 24d  ││ total ▰▱▱▱▱   6% ↻ 16d  │
│ 7d    ▰▰▰▱▱  58% ↻ 1d23h││ 7d    ▰▰▱▱▱  36% ↻ 1h54m││ reqs  ▰▱▱▱▱ 45/1500     ││ auto  ▱▱▱▱▱   0% ↻ 16d  │
│ Fable ▰▰▰▰▱  79% ↻ 1d23h││ Credits: 0  +3 reset    ││ updated just now        ││ api   ▰▰▰▱▱  56% ↻ 16d  │
│ Balance: -   $0.00 used ││ updated just now        ││                         ││ updated just now        │
└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘
```

- **Claude** — 方案类型、5 小时、每周以及单模型每周用量，来自官方 OAuth 用量 API（`GET /api/oauth/usage`），从 `~/.claude/.credentials.json` 读取，并显示额度余额。约每分钟轮询一次以避开该端点的速率限制；触及上限时标题会出现红色 `LIMIT` 标记。单模型每周那一行属于尽力而为，未返回该范围时就自动隐藏。
- **Codex** — 套餐类型、5 小时和每周用量以及额度余额，使用 `~/.codex/auth.json` 从 ChatGPT 后端（`wham/usage`）获取（在适用时显示大致剩余讯息数 / 消费上限）；API 不可用时回退到 Codex 会话日志中最新的 `rate_limits`（标题显示 `Codex` 或 `Codex (session)`）。
- **Copilot** — 方案类型，以及你的 premium 请求额度，以两个进度条呈现：已用百分比，以及已用 / 总量的请求数（例如 `45/1500`），来自 GitHub 的 Copilot API（`GET /copilot_internal/user`），从 `~/.copilot/config.json` 读取。该请求会模拟 Copilot CLI。token 为长期有效，因此不需要刷新；遇到 `401` / `403` 时会显示 `run: copilot login` 提示。
- **Cursor** — 方案类型、total / auto / API 已用百分比，以及按需消费，来自 cursor.com（`GET /api/usage-summary`），使用 `~/.config/cursor/auth.json` 中的 session token。刷新是被动式的：vct 每次轮询都会重新读取该文件，并在 token 有效期内使用它，因为官方 Cursor 客户端会让它保持最新。

**自动刷新 token。** 对 Claude 和 Codex，当 token 接近过期或被拒绝时，vct 会刷新它并把新 token 写回该 provider 自己的凭证文件（采用该 CLI 的原始格式），因此 token 会在多次检查之间复用，而不是每次都重新刷新。如果刷新失败，面板会显示 `run: <provider> auth login` 提示，而不会直接中断。Copilot（长期有效的 token）和 Cursor（由其自身客户端保持最新）为只读——vct 从不写入它们的凭证文件。

只有在某个 provider 的凭证存在时，才会显示对应的面板。当四个面板都显示时，Provider Usage 表格会从这一栏中折叠隐藏；在较窄的宽度下，面板会折行成 2×2 网格。额度面板仅在交互式 TUI 中显示；`--table`、`--text`、`--json` 不受影响。

> **平台说明：** 在 macOS 上，Claude Code 会把 OAuth 凭证保存在系统 Keychain 中，而不是 `~/.claude/.credentials.json`，因此在 macOS 上不会显示 Claude 面板。Cursor 的 `~/.config/cursor` 凭证路径偏向 Linux。

---

## Analysis 命令

**深入了解代码操作——查看你的 AI 助手到底做了什么。**

### 参数与 Flag

| 参数 / Flag                                    | 用途                                                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------- |
| *(不带参数)*                                   | 互动式 TUI 面板, 覆盖所有 session                                            |
| `<FILE>`                                       | 分析单一 JSONL/JSON session 文件, 并将完整 `CodeAnalysis` JSON 输出到 stdout |
| `--table`                                      | 静态摘要表格, 附带 provider 汇总                                             |
| `--text`                                       | 纯文本摘要, 方便脚本处理                                                     |
| `--json`                                       | 完整 parser 结果. 搭配 `<FILE>` 时为单一 object, 否则为 object 数组          |
| `--daily` / `--weekly` / `--monthly` / `--all` | 所有 session 的时间范围筛选. 不可与 `<FILE>` 同时使用, 其他说明见上方表格    |

参见 [`examples/`](examples/) 目录，其中包含四种 JSONL provider 的示例输入与对应 JSON 输出，以及 [`examples/grok_session/`](examples/grok_session/) 下的 Grok session fixture。

### 基本用法

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with per-provider totals
vct analysis --table

# 纯文本输出，方便脚本处理
vct analysis --text

# 输出所有 session 的完整 parser 结果
vct analysis --json

# 分析单一对话文件并输出 JSON
vct analysis ~/.claude/projects/session.jsonl

# 只汇总这个对话文件
vct analysis ~/.claude/projects/session.jsonl --table

# 通过 shell redirection 保存完整 JSON
vct analysis --json > report.json
vct analysis ~/.claude/projects/session.jsonl > session-analysis.json

# 时间范围与输出格式可自由组合
vct analysis --weekly
vct analysis --table --monthly
vct analysis --json --daily
vct analysis --json --daily > today.json
```

### 预览：交互式面板（`vct analysis`）

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
│ Total Lines: 16.1K  |  Total Tools: 619  |  Models: 3  |  Memory: 41.2 MB  |  CPU: 17.9%                        │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  r refresh  q quit  |  Star on GitHub
```

### 预览：表格与 JSON（`vct analysis`）

`--table` 会显示各 model 的明细, 并附上各 provider 的汇总, 包含 Active Days 列. `--text` 与 `--table` 都是相同 normalized parser records 的精简 projection. `--json` 会保留完整 records, 包括每次操作的 details 与 token usage. 未提供 `<FILE>` 时, 外层数组中的每个元素都是一个 session 的 `CodeAnalysis` object. 提供 `<FILE>` 时, stdout 只会输出该 object, shape 与 [`examples/`](examples/) 中对应的结果相同.

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
> 完整 analysis JSON 可能很大, 也可能包含 source text, edit body, shell command, absolute path, repository URL, user name, machine identifier 与 token metadata. 分享前请先检查内容.

Batch analysis 会读取 Provider 的实时数据. 如果 assistant 在扫描期间继续写入 session, 后续执行可能合理地包含更新数据. 未变动的 input 会产生固定顺序的输出.

如果找到的 source 全部读取失败或使用无法识别的 schema, 非交互式 analysis 会返回 error. 如果只有部分 source 失败, 成功结果会保留, warning 会写入 stderr.

`analysis FILE` 对单一文件内格式错误或不受支持的 record 采用相同行为: 在 stdout 保留已解析的 JSON/text/table 输出, 并将一般性的 skipped-record warning 写入 stderr.

Codex code mode session 会提供已完成的 JavaScript `exec` cell, 但没有 nested tool 的结构化 trace. VCT 会将该 cell 计为一次 Bash call, 并在完整 JSON 中保留 source, 但不会猜测 nested Read/Edit/Write operation.

---

## Update 命令

**自动保持安装版本为最新。**

update 命令适用于**所有安装方式**（npm/pip/cargo/手动安装），它会直接从 GitHub releases 下载并替换二进制文件。

### 基本用法

```bash
# Check for updates
vct update --check

# Interactive update with confirmation
vct update

# Force update — always downloads latest version
vct update --force
```

### 预览（`vct update --check`）

```
Current version: v1.3.0
Checking for latest release...
Latest version: v1.3.0 — you are up to date!
```

---

## Version 命令

查看内置的构建信息（binary version、Rust toolchain、Cargo version）：

```bash
vct version          # 彩色表格
vct version --text   # 每行一个字段，适合脚本
vct version --json   # 机读 JSON
```

```text
┌───────────────┬──────────┐
│ Version       ┆ 1.3.0    │
│ Rust Version  ┆ 1.96.0   │
│ Cargo Version ┆ 1.96.0   │
└───────────────┴──────────┘
```

Binary version 由 `build.rs` 在编译期通过 `git describe` 写入，开发版本会附带 commit 计数、short SHA 与 `dirty` 后缀。

---

## Fetch 命令

**打印某个供应商的原始 quota/usage API 响应 — 不解析、不聚合。**

对 `usage` 面板使用的同一个 quota 端点（Claude / Codex / Copilot / Cursor）发一次请求，直接打印原始 body，方便你查看 API 的实际结构或检查凭证是否正常。它读取各供应商已保存的凭证，并且**不会**刷新 token：token 过期时，请用对应供应商自己的 CLI 重新登录（`claude` / `codex` / `copilot` / `cursor-agent`）。

### 参数

| 参数      | 用途                          |
| --------- | ----------------------------- |
| *(无)*    | 彩色 JSON（默认）             |
| `--json`  | 彩色 JSON                     |
| `--text`  | 摊平成 `key: value`，适合脚本 |
| `--table` | 摊平成 Field / Value 表格     |

### 基本用法

```bash
# 原始 JSON（默认）
vct fetch claude
vct fetch codex
vct fetch copilot
vct fetch cursor

# 摊平成纯文本
vct fetch codex --text

# 摊平成 key/value 表格
vct fetch copilot --table
```

> [!NOTE]
> 响应 body 会原样打印到 stdout。遇到 HTTP 错误时仍会打印 body 并以非零状态退出；401/403 还会在 stderr 额外打印 `run: <cli> login` 提示。

---

## 配置

vct 会把用户设置保存在 `~/.vct/config.toml` 中。该文件会在**首次运行时以默认值自动生成**，因此你完全不必手动编写——只有想修改某个默认值时才编辑它。它由 vct 的类型化设置生成，并在第一行带有 `#:schema` 指令，因此支持 schema 的 TOML 编辑器（taplo / VS Code 的 "Even Better TOML"）会提供自动补全与校验。你也可以用 `vct config schema` 自行打印该 schema。由旧版 vct 生成的文件会在下次被 vct 读取时就地升级到当前布局（也可用 `vct config migrate` 手动触发），因此升级后绝不会停留在过时的格式上。

```toml
#:schema https://raw.githubusercontent.com/Mai0313/VibeCodingTracker/main/vct.schema.json

[general]
# 未指定 --daily/--weekly/--monthly/--all flag 时使用的默认时间范围
# 取值之一: "daily" | "weekly" | "monthly" | "all"
default_time_range = "all"

[usage]
# 启动 usage 面板时就把跨 provider 前缀的 model 合并显示
# 可用 `m` 实时切换, 最后一次的状态会保存回这里
merge_models = false
# usage TUI 自动刷新的间隔秒数 (最小为 1)
refresh_interval = 10

[usage.quota]
# 显示哪些实时额度面板; 删除某个名称即可隐藏该面板, 用空列表 ([]) 隐藏整栏
panels = ["claude", "codex", "copilot", "cursor"]
# 每个 provider 共用的实时额度面板轮询间隔秒数 (最小为 1)
refresh_interval = 60

[analysis]
# analysis TUI 自动刷新的间隔秒数 (最小为 1)
refresh_interval = 10

[providers]
# 是否把各 provider 的 session 纳入 usage / analysis, 设为 false
# 就会完全跳过它 (不扫描目录, 也不调用 API)
claude = true
codex = true
copilot = true
gemini = true
opencode = true
cursor = true
hermes = true
grok = true

[logging]
# 写入 ~/.vct/logs/vct-YYYY-MM-DD.log 的最低日志级别。
# 取值: "off" | "error" | "warn" | "info" | "debug" | "trace"。
level = "warn"
# 保留多少天的每日日志文件; 更旧的文件会在启动时清除。0 表示全部保留。
retention_days = 7
```

| 设置项                         | 作用                                                                                             |
| ------------------------------ | ------------------------------------------------------------------------------------------------ |
| `general.default_time_range`   | 当你没有传入 `--daily/--weekly/--monthly/--all` 时使用的时间范围。显式传入的 flag 始终优先。     |
| `usage.merge_models`           | 让面板启动时就处于合并状态；`m` 切换会把你上次的选择保存回这里。`--merge-providers` 会强制开启。 |
| `usage.refresh_interval`       | `usage` 面板的自动刷新间隔（秒）。                                                               |
| `usage.quota.panels`           | 显示哪些额度面板（`claude` / `codex` / `copilot` / `cursor`）；删除名称即可隐藏，`[]` 隐藏整栏。 |
| `usage.quota.refresh_interval` | 每个实时额度面板的轮询间隔（秒）；数值越大越不容易触发 provider 的速率限制。                     |
| `analysis.refresh_interval`    | `analysis` 面板的自动刷新间隔（秒）。                                                            |
| `providers.*`                  | 设为 `false` 时完全跳过某个 provider（不扫描、不调用 API）——如果你不用某个 provider 会很方便。   |
| `logging.level`                | 写入日志文件的最低级别（`off`..`trace`）；从不打印到终端。                                       |
| `logging.retention_days`       | 保留多少天的每日日志文件；更旧的 `vct-*.log` 会在启动时清除（`0` 表示全部保留）。                |

> [!NOTE]
> vct 会把诊断信息写入 `~/.vct/logs/vct-YYYY-MM-DD.log`（纯文本，仅写文件，绝不显示在面板上）。健康运行时保持安静（默认级别 `warn`），且文件是惰性创建的，所以一次正常运行不会留下任何文件。当额度获取失败或某个 session 被跳过时，原因就记录在这里——需要完整细节时把 `logging.level` 调到 `debug`。

> [!NOTE]
> Cursor 的 `usage` 是从聊天库得出的**本地估算**，因此它会像 Claude Code / Codex / Copilot / Gemini 一样（都是从本地 session 文件计算得出）无需联网。该估算会低估 Cursor 的真实花费，因为其中很大一部分是以 Cursor 内部的 model 名称计费，本地数据无法为这些名称定价，所以请把 Cursor 费用视为近似值。

### 管理配置文件

```bash
# 打印配置文件路径
vct config path

# 打印当前设置
vct config show

# 在 $VISUAL / $EDITOR 中打开文件 (回退到 vi / notepad)
vct config edit

# 打印 JSON schema (可用以下命令重新生成: vct config schema > vct.schema.json)
vct config schema

# 就地把旧格式文件升级到当前布局
vct config migrate
```

---

## 智能定价系统

### 工作原理

1. **自动更新**：每天从 [LiteLLM](https://github.com/BerriAI/litellm) 获取最新定价
2. **智能缓存**：将定价信息存储在 `~/.vct/` 目录中，有效期 24 小时
3. **模糊匹配**：即使是自定义模型名称也能找到最佳匹配
4. **始终精确**：确保你获取到最新的定价信息

### 模型匹配

**优先级顺序**：

1. **精确匹配**：`claude-sonnet-4` → `claude-sonnet-4`
2. **标准化匹配**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. **子串匹配**：`custom-gpt-4` → `gpt-4`
4. **模糊匹配（AI 驱动）**：使用 Jaro-Winkler 相似度算法（70% 阈值）
5. **兜底方案**：如果未找到匹配，显示 $0.00

### 费用细节

- **不止 token**：Claude 的 web-search 工具调用（`server_tool_use.web_search_requests`）会在 token 费用之外按每次查询计费；其他所有 model 的每次查询费用均为 $0。
- **OpenCode**：只有在 LiteLLM 上**精确**匹配时，才会根据 token 为一个全新的 model 名称定价；若没有精确匹配，vct 会信任该 assistant message 自身存储的费用，而不是从一个只是大致相似的名称去猜测。
- **Hermes**：与 OpenCode 相同，LiteLLM 上**精确**匹配时按 token 定价，否则使用 Hermes 自身存储的费用。
- **Grok**：只会把 `contextTokensUsed` 作为 cache-read token 计价；这是单一时点的本地 context 估算，不是累计的 billed usage。
- **缓存为原始数据**：每日缓存存储的是经过筛选的上游 LiteLLM JSON（而非派生后的结构），因此无需重新获取即可保留分层 / 批量定价；此外还有一个小型的进程内 LRU，让 TUI 刷新期间的重复查询保持低开销。

---

## Docker 支持

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .
```
