<div align="center" markdown="1">

# Vibe Coding Tracker — AI 程式設計助手使用量追蹤器

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

**即時追蹤你的 AI 程式設計花費。** Vibe Coding Tracker 是一款以 Rust 打造的輕量、高效能 CLI 工具，能監控與分析你在 Claude Code、Codex、Copilot、Gemini 及 OpenCode 的使用狀況——提供詳細的費用明細、token 統計資料與程式碼操作分析，同時維持極低的記憶體使用量。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：CLI 範例使用簡短別名 `vct`。如果你是透過 npm/pip/cargo 安裝，執行檔可能命名為 `vibe_coding_tracker` 或 `vct`。如有需要，請建立別名或在執行指令時將 `vct` 替換為完整名稱。

---

## 為什麼選擇 Vibe Coding Tracker？

### 掌握你的花費

不用再猜測 AI 程式設計工作階段花了多少錢。透過 [LiteLLM](https://github.com/BerriAI/litellm) 自動更新價格，取得**即時費用追蹤**。

### 超輕量

以 Rust 打造，資源佔用極低。互動式 TUI 儀表板穩定後通常維持在 **~50 MB 以內的常駐記憶體**，即使硬碟上有數百個長 context session 也一樣——不用 Electron，不用臃腫的執行環境。usage 路徑用精簡模式串流 parse 每個 session 檔案並繞過 cache，啟動時也會調整 glibc 的 arena 數量，讓長時間執行的 RSS 保持誠實。

### 精美視覺化

選擇你偏好的檢視方式：

- **互動式儀表板**：自動更新的終端機 UI,即時顯示最新資訊、可捲動的 model 清單(方向鍵),以及 K/M/B 精簡數字格式
- **靜態報表**：專業的表格格式，適合撰寫文件
- **腳本友好**：純文字及 JSON 輸出，方便自動化處理
- **完整精度**：匯出精確費用供會計使用

### 零設定

自動偵測並處理 Claude Code、Codex、Copilot、Gemini 及 OpenCode 的日誌檔。不需要任何設定——直接執行就能分析。

### 豐富洞察

- 依模型與日期分類的 token 使用量
- 依 cache 類型（讀取 / 建立）的費用明細
- 檔案操作追蹤（編輯、讀取、寫入行數）
- 工具呼叫歷史（Bash、Edit、Read、Write、TodoWrite）
- 每個供應商的總計

---

## 主要功能

| 功能             | 說明                                                      |
| ---------------- | --------------------------------------------------------- |
| **多供應商支援** | Claude Code、Codex、Copilot、Gemini 及 OpenCode——一站整合 |
| **智慧定價**     | 模糊模型比對 + 每日從 LiteLLM cache 更新                  |
| **4 種顯示模式** | 互動式 TUI、靜態表格、純文字及 JSON                       |
| **雙重分析**     | Token / 費用統計（`usage`）+ 程式碼操作統計（`analysis`） |
| **即時額度面板** | 即時顯示 Claude、Codex、Copilot 與 Cursor 的剩餘額度      |
| **超輕量**       | TUI 常駐記憶體 ~50 MB 以內、串流 JSONL 解析——以 Rust 打造 |
| **即時更新**     | 每 10 秒自動刷新的儀表板並突顯變更                        |

---

## 快速開始

### 安裝

選擇最適合你的安裝方式：

> **開發者**：如果你想從原始碼建置或參與開發，請參閱 [CONTRIBUTING.md](.github/CONTRIBUTING.md)。

#### 方法一：透過 npm 安裝

**前置條件**：[Node.js](https://nodejs.org/) v22 或更高版本

選擇以下任一套件名稱（內容完全相同）：

```bash
# Main package
npm install -g vibe-coding-tracker

# Short alias with scope
npm install -g @mai0313/vct

# Full name with scope
npm install -g @mai0313/vibe-coding-tracker
```

#### 方法二：透過 PyPI 安裝

**前置條件**：Python 3.8 或更高版本

```bash
pip install vibe_coding_tracker
# Or with uv
uv pip install vibe_coding_tracker

# Run without installing, straight from PyPI (uv)
uvx vibe_coding_tracker usage
```

#### 方法三：透過 crates.io 安裝

使用 Cargo 從官方 Rust 套件倉庫安裝：

```bash
cargo install vibe_coding_tracker
```

### 首次執行

```bash
# View your usage with the interactive dashboard
vct usage

# Or run the binary built by Cargo/pip
vibe_coding_tracker usage

# Analyze code operations across all sessions
vct analysis
```

---

## 指令指南

### 快速參考

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

時間範圍 flag（`usage` 與 `analysis` 共用，互斥，預設 `--all`）：

| Flag          | 範圍                       |
| ------------- | -------------------------- |
| `--daily`     | 今天更新過的 session       |
| `--weekly`    | 本 ISO 週（週一 → 今天）   |
| `--monthly`   | 本自然月                   |
| `-a`, `--all` | 磁碟上所有 session（預設） |

---

## Usage 指令

**追蹤你在所有 AI 程式設計工作階段的花費。**

### Flag 一覽

| Flag                                           | 用途                         |
| ---------------------------------------------- | ---------------------------- |
| *(不帶參數)*                                   | 互動式 TUI 儀表板（預設）    |
| `--table`                                      | 靜態表格，不啟動 TUI         |
| `--text`                                       | 純文字，適合腳本處理         |
| `--json`                                       | JSON 輸出，附帶 pricing 資訊 |
| `--output <FILE>`                              | 將富化 JSON 存成檔案         |
| `--daily` / `--weekly` / `--monthly` / `--all` | 時間範圍篩選（見上方表格）   |

### 基本用法

```bash
# Interactive dashboard (recommended)
vct usage

# Static table for reports
vct usage --table

# Plain text for scripts
vct usage --text

# JSON 輸出，包含 cost_usd 與 matched_model 欄位
vct usage --json

# 直接把富化 JSON 存成檔案
vct usage --output report.json

# 時間範圍與輸出格式可自由組合
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily
```

> [!NOTE]
> Model 列會依 cost 由小到大排序，所以花費最高的 model 會排在最後(在 `--table` 中緊鄰 `TOTAL` 列上方)。這個排序會套用到互動式儀表板、`--table` 與 `--text` 三種輸出;`--json` 也會保持相同順序。互動式儀表板也會隱藏在所選範圍內用量為 0 的 model。

### 預覽：互動式儀表板（`vct usage`）

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
  ↑/↓ scroll  PgUp/PgDn page  g/G top/end  r refresh  q quit  |  ★ github.com/Mai0313/VibeCodingTracker
```

### 預覽：表格與 JSON（`vct usage`）

`--table` 會以靜態報表印出相同的數字，並附上每個供應商的彙總；`--json` 則為每個 model 輸出一列富化資料（各自帶有 `cost_usd`），方便腳本處理。

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

### 掃描範圍

此工具會自動掃描以下目錄：

- `~/.claude/projects/**/*.jsonl`（Claude Code，遞迴包含 subagent 日誌）
- `~/.codex/sessions/**/*.jsonl`（Codex，遞迴包含每日子目錄）
- `~/.copilot/session-state/<sessionId>/events.jsonl`（Copilot CLI）
- `~/.gemini/tmp/<project_hash>/chats/*.jsonl`（Gemini CLI）
- `~/.local/share/opencode/opencode.db`（OpenCode，SQLite 資料庫；遵循 `$XDG_DATA_HOME`）

### 即時額度面板

`vct usage` 會**在儀表板中直接顯示 Claude Code、Codex、GitHub Copilot 與 Cursor 的即時剩餘額度——完全零設定。** 不需要 status-line hook，也不需要設定檔：vct 會讀取各 provider 自己的 OAuth 憑證，在背景執行緒呼叫其用量 API，並在你工作時讓面板保持最新。

```
┌ Claude ─────────────────┐┌ Codex ──────────────────┐┌ Copilot ────────────────┐┌ Cursor ─────────────────┐
│ Plan: max 20x           ││ Plan: plus              ││ Plan: individual        ││ Plan: free              │
│ 5h    ▰▱▱▱▱  13% ↻ 1h42m││ 5h    ▰▰▱▱▱  33% ↻ 12m  ││ prem  ▰▱▱▱▱   3% ↻ 24d  ││ total ▰▱▱▱▱   6% ↻ 16d  │
│ 7d    ▰▰▰▱▱  58% ↻ 1d23h││ 7d    ▰▰▱▱▱  36% ↻ 1h54m││ reqs  ▰▱▱▱▱ 45/1500     ││ auto  ▱▱▱▱▱   0% ↻ 16d  │
│ Fable ▰▰▰▰▱  79% ↻ 1d23h││ Credits: 0  +3 reset    ││ updated just now        ││ api   ▰▰▰▱▱  56% ↻ 16d  │
│ Balance: -   $0.00 used ││ updated just now        ││                         ││ updated just now        │
└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘
```

- **Claude** — 方案類型、5 小時、每週以及單模型每週用量，來自官方 OAuth 用量 API（`GET /api/oauth/usage`），從 `~/.claude/.credentials.json` 讀取，並顯示額度餘額。約每分鐘輪詢一次以避開該端點的速率限制；觸及上限時標題會出現紅色 `LIMIT` 標記。單模型每週那一行屬於盡力而為，未回傳該範圍時就自動隱藏。
- **Codex** — 方案類型、5 小時和每週用量以及額度餘額，使用 `~/.codex/auth.json` 從 ChatGPT 後端（`wham/usage`）取得（在適用時顯示大致剩餘訊息數 / 消費上限）；API 無法使用時回退到 Codex 工作階段紀錄中最新的 `rate_limits`（標題顯示 `Codex` 或 `Codex (session)`）。
- **Copilot** — 方案類型以及你的 premium 請求額度，以兩個進度條呈現：已用百分比，以及已用 / 總量請求數（例如 `45/1500`），來自 GitHub 的 Copilot API（`GET /copilot_internal/user`），從 `~/.copilot/config.json` 讀取。該請求會模擬 Copilot CLI。token 為長期有效，因此不需要刷新；遇到 `401` / `403` 時會顯示 `run: copilot login` 提示。
- **Cursor** — 方案類型、total / auto / API **已用**百分比，以及按需消費，來自 cursor.com（`GET /api/usage-summary`），使用 `~/.config/cursor/auth.json` 中的 session token。刷新是被動式的：vct 每次輪詢都會重新讀取該檔案，並在 token 有效期內使用它，因為官方 Cursor 用戶端會讓它保持最新。

**自動刷新 token。** 對 Claude 和 Codex，當 token 接近過期或被拒絕時，vct 會刷新它並把新的 token 寫回該 provider 自己的憑證檔案（採用該 CLI 的原始格式），因此 token 會在多次檢查之間重複使用，而不是每次都重新刷新。如果刷新失敗，面板會顯示 `run: <provider> auth login` 提示，而不會直接中斷。Copilot（長期有效的 token）和 Cursor（由其自身用戶端保持最新）為唯讀——vct 從不寫入它們的憑證檔案。

只有在某個 provider 的憑證存在時，才會顯示對應的面板。當四個面板都顯示時，Provider Usage 表格會從這一列中折疊隱藏；在較窄的寬度下，面板會折行成 2×2 網格。額度面板僅在互動式 TUI 中顯示；`--table`、`--text`、`--json` 不受影響。

> **平台說明：** 在 macOS 上，Claude Code 會把 OAuth 憑證儲存在系統 Keychain 中，而不是 `~/.claude/.credentials.json`，因此在 macOS 上不會顯示 Claude 面板。Cursor 的 `~/.config/cursor` 憑證路徑偏向 Linux。

---

## Analysis 指令

**深入分析程式碼操作——精確掌握你的 AI 助手做了哪些事。**

### Flag 一覽

| Flag                                           | 用途                                             |
| ---------------------------------------------- | ------------------------------------------------ |
| *(不帶參數)*                                   | 互動式 TUI 儀表板，涵蓋所有 session              |
| `--path <FILE>`                                | 分析單一 JSONL/JSON 對話檔案（stdout 輸出 JSON） |
| `--table`                                      | 靜態表格，附帶供應商總計                         |
| `--text`                                       | 純文字，方便腳本處理                             |
| `--json`                                       | 將聚合 row 以 JSON 陣列輸出到 stdout             |
| `--output <FILE>`                              | 將結果以格式化 JSON 存成檔案                     |
| `--daily` / `--weekly` / `--monthly` / `--all` | 時間範圍篩選（見上方表格）                       |

請參考 [`examples/`](examples/) 目錄，裡面有四種 provider 的範例輸入與對應的 JSON 輸出。

### 基本用法

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with per-provider totals
vct analysis --table

# 純文字輸出，方便腳本處理
vct analysis --text

# 聚合資料以 JSON 輸出，方便後續處理
vct analysis --json

# 分析單一對話檔案 → stdout JSON
vct analysis --path ~/.claude/projects/session.jsonl

# Save results to JSON
vct analysis --output report.json

# 時間範圍與輸出格式可自由組合
vct analysis --weekly
vct analysis --table --monthly
vct analysis --json --daily
vct analysis --output today.json --daily
```

### 預覽：互動式儀表板（`vct analysis`）

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

### 預覽：表格與 JSON（`vct analysis`）

`--table` 會呈現每個 model 的明細以及每個供應商的彙總（含 Active Days 欄位）；`--json` 則為每個 model 輸出一列聚合資料。

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

## Update 指令

**自動保持安裝為最新版本。**

Update 指令適用於**所有安裝方式**（npm/pip/cargo/手動安裝），透過直接從 GitHub releases 下載並替換執行檔來完成更新。

### 基本用法

```bash
# Check for updates
vct update --check

# Interactive update with confirmation
vct update

# Force update — always downloads latest version
vct update --force
```

### 預覽（`vct update --check`）

```
Current version: v1.3.0
Checking for latest release...
Latest version: v1.3.0 — you are up to date!
```

---

## Version 指令

檢視內建的建置資訊（binary version、Rust toolchain、Cargo version）：

```bash
vct version          # 彩色表格
vct version --text   # 每行一個欄位，適合腳本
vct version --json   # 機器可讀的 JSON
```

```text
┌───────────────┬──────────┐
│ Version       ┆ 1.3.0    │
│ Rust Version  ┆ 1.96.0   │
│ Cargo Version ┆ 1.96.0   │
└───────────────┴──────────┘
```

Binary version 由 `build.rs` 在編譯期透過 `git describe` 寫入，開發版本會附上 commit 數、short SHA 與 `dirty` 後綴。

---

## 智慧定價系統

### 運作方式

1. **自動更新**：每日從 [LiteLLM](https://github.com/BerriAI/litellm) 取得最新價格
2. **智慧快取**：將價格資料儲存於 `~/.vct/`，有效期 24 小時
3. **模糊比對**：即使是自訂模型名稱也能找到最佳配對
4. **始終精準**：確保你取得最新的定價資訊

### 模型比對

**優先順序**：

1. **完全比對**：`claude-sonnet-4` → `claude-sonnet-4`
2. **正規化比對**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. **子字串比對**：`custom-gpt-4` → `gpt-4`
4. **模糊比對（AI 驅動）**：使用 Jaro-Winkler 相似度（70% 門檻值）
5. **備援方案**：若無法配對則顯示 $0.00

### 費用細節

- **不只 token**：Claude 的網頁搜尋工具呼叫（`server_tool_use.web_search_requests`）會在 token 費用之外，按每次查詢計費；其他所有 model 的每次查詢費用皆為 $0。
- **OpenCode**：只有在 LiteLLM **完全比對**成功時，才會依 token 為新型 model 計價；若沒有完全比對，vct 會採信該 assistant 訊息本身儲存的費用，而不是從名稱相近的 model 去猜測。
- **原始 cache**：每日 cache 儲存的是經過篩選的上游 LiteLLM JSON（而非衍生後的結構），因此分層 / 批次定價無需重新抓取即可使用；另外一個小型的行內 LRU 會讓 TUI 刷新期間的重複查詢維持低成本。

---

## Docker 支援

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .
```
