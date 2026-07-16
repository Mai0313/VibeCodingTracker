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

**即時追蹤你的 AI 程式設計花費。** Vibe Coding Tracker 是一款以 Rust 打造的輕量、高效能 CLI 工具，能監控與分析你在 Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 及 Grok 的使用狀況——提供詳細的費用明細、token 統計資料與程式碼操作分析，同時維持極低的記憶體使用量。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：CLI 範例使用簡短別名 `vct`。如果你是透過 npm/pip/cargo 安裝，執行檔可能命名為 `vibe_coding_tracker` 或 `vct`。如有需要，請建立別名或在執行指令時將 `vct` 替換為完整名稱。

---

## 為什麼選擇 Vibe Coding Tracker？

### 掌握你的花費

不用再猜測 AI 程式設計工作階段花了多少錢。透過 [LiteLLM](https://github.com/BerriAI/litellm) 自動更新價格，取得**即時費用追蹤**。

### 超輕量

以 Rust 打造, 資源佔用極低. 互動式 TUI 儀表板完成首次刷新後通常維持在 **~50 MB 以內的常駐記憶體**, 即使硬碟上有數百個長 context session 也一樣. 首次掃描後, 精簡的 process-local summary cache 只會重新解析新增或變更的 source, dedicated scan worker 與 glibc allocator 調整也能讓長時間執行時的 CPU 和 RSS 維持穩定.

### 精美視覺化

選擇你偏好的檢視方式：

- **互動式儀表板**: 可立即顯示 loading spinner 的響應式終端 UI, 支援背景 incremental refresh, 可捲動的 model 清單 (方向鍵), process 層級的 CPU/記憶體即時讀數, 以及 K/M/B 精簡數字格式
- **靜態報表**:專業的表格格式,適合撰寫文件
- **腳本友好**:純文字及 JSON 輸出,方便自動化處理
- **完整精度**:匯出精確費用供會計使用

### 零設定

自動偵測並處理 Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 及 Grok 的日誌檔。不需要任何設定——直接執行就能分析。首次執行時會以合理的預設值建立 `~/.vct/config.toml`，日後若想微調行為即可編輯它（見 [設定](#%E8%A8%AD%E5%AE%9A)）。

### 豐富洞察

- 依模型與日期分類的 token 使用量
- 依 cache 類型（讀取 / 建立）的費用明細
- 檔案操作追蹤（編輯、讀取、寫入行數）
- 工具呼叫歷史（Bash、Edit、Read、Write、TodoWrite）
- 每個供應商的總計

---

## 主要功能

| 功能             | 說明                                                                  |
| ---------------- | --------------------------------------------------------------------- |
| **多供應商支援** | Claude Code、Codex、Copilot、Gemini、OpenCode、Cursor、Hermes 及 Grok |
| **智慧定價**     | 模糊模型比對 + 每日從 LiteLLM cache 更新                              |
| **4 種顯示模式** | 互動式 TUI、靜態表格、純文字及 JSON                                   |
| **雙重分析**     | Token / 費用統計（`usage`）+ 程式碼操作統計（`analysis`）             |
| **即時額度面板** | 即時顯示 Claude、Codex、Copilot 與 Cursor 的剩餘額度                  |
| **超輕量**       | TUI 常駐記憶體 ~50 MB 以內、精簡的 incremental scan, 以 Rust 打造     |
| **即時更新**     | 響應式 loading 與背景 refresh, 並突顯變更                             |

---

## 快速開始

### 安裝

選擇最適合你的安裝方式：

> **開發者**: 如果你想從原始碼建置或參與開發, 請參閱 [CONTRIBUTING.md](.github/CONTRIBUTING.md).

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
  analysis    Analyze local session data (single file or all sessions)
  usage       Display token usage statistics
  version     Display version information
  update      Update to the latest version from GitHub releases
  fetch       Fetch a provider's raw quota/usage API response
  config      Show or edit the persistent settings file (~/.vct/config.toml)
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

| Flag                                           | 用途                                                                          |
| ---------------------------------------------- | ----------------------------------------------------------------------------- |
| *(不帶參數)*                                   | 互動式 TUI 儀表板（預設）                                                     |
| `--table`                                      | 靜態表格，不啟動 TUI                                                          |
| `--text`                                       | 純文字，適合腳本處理                                                          |
| `--json`                                       | JSON 輸出，附帶 pricing 資訊                                                  |
| `--merge-providers`                            | 合併共享同一 base 名稱、僅 provider 前綴不同的 model（`--json` 會忽略此選項） |
| `--daily` / `--weekly` / `--monthly` / `--all` | 時間範圍篩選（見上方表格）                                                    |

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

# 透過 shell redirection 儲存富化 JSON
vct usage --json > report.json

# 時間範圍與輸出格式可自由組合
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily

# 合併同一 model 在不同 provider 前綴下的多列
# (例如 openai/gpt-5.5 + azure/gpt-5.5 + gpt-5.5 -> 一列)
vct usage --table --merge-providers
```

> [!NOTE]
> Model 列會依 cost 由小到大排序，所以花費最高的 model 會排在最後(在 `--table` 中緊鄰 `TOTAL` 列上方)。這個排序會套用到互動式儀表板、`--table` 與 `--text` 三種輸出;`--json` 也會保持相同順序。互動式儀表板也會隱藏在所選範圍內用量為 0 的 model。

> [!TIP]
> 同一個 model 在不同 provider 前綴下路由時會顯示成多列（`openai/gpt-5.5`、`azure/gpt-5.5`、純 `gpt-5.5`）。`--merge-providers` 會把第一個 `/` 之後 base 名稱相同的列合併（`gpt-5.5` 與 `gpt-5.4` 這類版本不同的仍分開），並把它們已定價的 cost 相加。在互動式儀表板中按 `m` 可即時切換（這個選擇會存進 `~/.vct/config.toml`，所以下次啟動時會記住這個設定）;`--merge-providers` 則讓儀表板一打開就是合併狀態。`--json` 保持為逐一 model 的原始輸出。

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
│ Total Cost: $79.33  |  Total Tokens: 49.3M  |  Models: 3  |  Memory: 42.8 MB  |  CPU: 17.9% │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  m merge  r refresh  q quit  |  Star on GitHub
```

兩個互動式儀表板都會在 terminal setup 完成後立即繪製置中的 `Loading sessions...` spinner. Loading 期間仍可處理 `q`, Ctrl+C 與 resize event. 後續掃描由單一 background worker 執行, 並在 `Refreshing...` footer 下保留上一次成功的資料. 重複的 refresh 要求最多只會合併為一個 pending scan. 如果 refresh 失敗, 儀表板會保留 last-known-good view, 並在下次排程或手動刷新時重試.

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
      "reasoning_output_tokens": 0,
      "total_tokens": 145495885
    }
  }
]
```

無論來源 provider 為何，每一列都會輸出相同的扁平 token 欄位（Codex 內部的巢狀結構會在輸出前正規化）。

### 掃描範圍

此工具會自動掃描以下目錄：

- `~/.claude/projects/**/*.jsonl`（Claude Code，遞迴包含 subagent 日誌）
- `~/.codex/sessions/**/*.jsonl`（Codex，遞迴包含每日子目錄）
- `~/.copilot/session-state/<sessionId>/events.jsonl`（Copilot CLI）
- `~/.gemini/tmp/<project_hash>/chats/*.jsonl`（Gemini CLI）
- `~/.local/share/opencode/opencode.db`（OpenCode，SQLite 資料庫；遵循 `$XDG_DATA_HOME`）
- `~/.cursor/chats/*/*/store.db`（Cursor，SQLite 對話庫，用於 `analysis`，並產生與其他 provider 一致的本地 `usage` 估算）
- `~/.hermes/state.db`（Hermes，SQLite 資料庫，遵循 `$HERMES_HOME`；僅 `usage`）
- `$GROK_HOME/sessions/*/*/signals.json`（Grok CLI，預設使用 `~/.grok`；同層的 `updates.jsonl` 提供 `analysis` 資料）

Grok 的 `usage` 是單一當下的本地 context 估算：vct 會把 `signals.json` 的 `contextTokensUsed` 記為 cache-read token，並以該 model 的 cache-read 費率估算費用。這不是累計的 billed usage。`analysis` 會從同層的 `updates.jsonl` 還原已完成的 Read / Write / Edit / Bash / TodoWrite 操作。Grok 不支援 quota panel 或 `vct fetch`。

對於非互動式 `usage` 與 `analysis` 掃描, 如果所有找到的 source 都失敗, vct 會回傳錯誤. 如果只有部分 source 失敗, vct 會保留成功的結果, 並向 stderr 印出一則診斷摘要. TUI 則保持 best-effort, 並保留上一次成功的 payload.

### 即時額度面板

`vct usage` 會**在儀表板中直接顯示 Claude Code、Codex、GitHub Copilot 與 Cursor 的即時剩餘額度——完全零設定。** 不需要 status-line hook，也不需要手動輸入憑證：vct 會讀取各 provider 自己的 OAuth 憑證，在背景執行緒呼叫其用量 API，並在你工作時讓面板保持最新。（想要更清爽的儀表板嗎？在 [`config.toml`](#%E8%A8%AD%E5%AE%9A) 中精簡 `[usage.quota]` 下的 `panels`,或設為 `[]` 隱藏整條。）

```
┌ Claude ─────────────────┐┌ Codex ──────────────────┐┌ Copilot ────────────────┐┌ Cursor ─────────────────┐
│ Plan: max 20x           ││ Plan: plus              ││ Plan: individual        ││ Plan: free              │
│ 5h    ▰▱▱▱▱  13% ↻ 1h42m││ 5h    ▰▰▱▱▱  33% ↻ 12m  ││ prem  ▰▱▱▱▱   3% ↻ 24d  ││ total ▰▱▱▱▱   6% ↻ 16d  │
│ 7d    ▰▰▰▱▱  58% ↻ 1d23h││ 7d    ▰▰▱▱▱  36% ↻ 1h54m││ reqs  ▰▱▱▱▱ 45/1500     ││ auto  ▱▱▱▱▱   0% ↻ 16d  │
│ Fable ▰▰▰▰▱  79% ↻ 1d23h││ Credits: 0  +3 reset    ││ updated just now        ││ api   ▰▰▰▱▱  56% ↻ 16d  │
│ Balance: -   $0.00 used ││ reset expires 17d0h     ││                         ││ updated just now        │
│ updated just now        ││ updated just now        ││                         ││                         │
└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘└─────────────────────────┘
```

- **Claude** — 方案類型、5 小時、每週以及單模型每週用量，來自官方 OAuth 用量 API（`GET /api/oauth/usage`），從 `~/.claude/.credentials.json` 讀取，並顯示額度餘額。約每分鐘輪詢一次以避開該端點的速率限制；觸及上限時標題會出現紅色 `LIMIT` 標記。單模型每週那一行屬於盡力而為，未回傳該範圍時就自動隱藏。
- **Codex** — 方案類型、5 小時和每週用量、額度餘額以及已取得的可用 reset credit 中最早的到期時間，使用 `~/.codex/auth.json` 從 ChatGPT 後端（`wham/usage` + `wham/rate-limit-reset-credits`）取得（在適用時顯示大致剩餘訊息數 / 消費上限）；API 無法使用時回退到 Codex 工作階段紀錄中最新的 `rate_limits`（標題顯示 `Codex` 或 `Codex (session)`）。
- **Copilot** — 方案類型以及你的 premium 請求額度，以兩個進度條呈現：已用百分比，以及已用 / 總量請求數（例如 `45/1500`），來自 GitHub 的 Copilot API（`GET /copilot_internal/user`），從 `~/.copilot/config.json` 讀取。該請求會模擬 Copilot CLI。token 為長期有效，因此不需要刷新；遇到 `401` / `403` 時會顯示 `run: copilot login` 提示。
- **Cursor** — 方案類型、total / auto / API **已用**百分比，以及按需消費，來自 cursor.com（`GET /api/usage-summary`），使用 `~/.config/cursor/auth.json` 中的 session token。刷新是被動式的：vct 每次輪詢都會重新讀取該檔案，並在 token 有效期內使用它，因為官方 Cursor 用戶端會讓它保持最新。

**自動刷新 token。** 對 Claude 和 Codex，當 token 接近過期或被拒絕時，vct 會刷新它並把新的 token 寫回該 provider 自己的憑證檔案（採用該 CLI 的原始格式），因此 token 會在多次檢查之間重複使用，而不是每次都重新刷新。如果刷新失敗，面板會顯示 `run: <provider> auth login` 提示，而不會直接中斷。Copilot（長期有效的 token）和 Cursor（由其自身用戶端保持最新）為唯讀——vct 從不寫入它們的憑證檔案。

只有在某個 provider 的憑證存在時，才會顯示對應的面板。當四個面板都顯示時，Provider Usage 表格會從這一列中折疊隱藏；在較窄的寬度下，面板會折行成 2×2 網格。額度面板僅在互動式 TUI 中顯示；`--table`、`--text`、`--json` 不受影響。

> **平台說明：** 在 macOS 上，Claude Code 會把 OAuth 憑證儲存在系統 Keychain 中，而不是 `~/.claude/.credentials.json`，因此在 macOS 上不會顯示 Claude 面板。Cursor 的 `~/.config/cursor` 憑證路徑偏向 Linux。

---

## Analysis 指令

**深入分析程式碼操作——精確掌握你的 AI 助手做了哪些事。**

### 參數與 Flag

| 參數 / Flag                                    | 用途                                                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------- |
| *(不帶參數)*                                   | 互動式 TUI 儀表板, 涵蓋所有 session                                          |
| `<FILE>`                                       | 分析單一 JSONL/JSON session 檔案, 並將完整 `CodeAnalysis` JSON 輸出到 stdout |
| `--table`                                      | 靜態摘要表格, 附帶 provider 總計                                             |
| `--text`                                       | 純文字摘要, 方便腳本處理                                                     |
| `--json`                                       | 完整 parser 結果. 搭配 `<FILE>` 時為單一 object, 否則為 object 陣列          |
| `--daily` / `--weekly` / `--monthly` / `--all` | 所有 session 的時間範圍篩選. 不可與 `<FILE>` 同時使用, 其他說明見上方表格    |

請參考 [`tests/fixtures/sessions/`](tests/fixtures/sessions/) 目錄，裡面有四種 JSONL provider 的範例輸入與對應的 JSON 輸出，以及 [`tests/fixtures/sessions/grok/`](tests/fixtures/sessions/grok/) 下的 Grok session fixture。

### 基本用法

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with per-provider totals
vct analysis --table

# 純文字輸出，方便腳本處理
vct analysis --text

# 輸出所有 session 的完整 parser 結果
vct analysis --json

# 分析單一對話檔案並輸出 JSON
vct analysis ~/.claude/projects/session.jsonl

# 只摘要這個對話檔案
vct analysis ~/.claude/projects/session.jsonl --table

# 透過 shell redirection 儲存完整 JSON
vct analysis --json > report.json
vct analysis ~/.claude/projects/session.jsonl > session-analysis.json

# 時間範圍與輸出格式可自由組合
vct analysis --weekly
vct analysis --table --monthly
vct analysis --json --daily
vct analysis --json --daily > today.json
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
│ Total Lines: 16.1K  |  Total Tools: 619  |  Models: 3  |  Memory: 41.2 MB  |  CPU: 17.9%                        │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
  ↑/↓ scroll  r refresh  q quit  |  Star on GitHub
```

### 預覽：表格與 JSON（`vct analysis`）

`--table` 會顯示各 model 的明細, 並附上各 provider 的摘要, 包含 Active Days 欄位. `--text` 與 `--table` 都是相同 normalized parser records 的精簡 projection. `--json` 會保留完整 records, 包括每次操作的 details 與 token usage. 未提供 `<FILE>` 時, 外層陣列中的每個元素都是一個 session 的 `CodeAnalysis` object. 提供 `<FILE>` 時, stdout 只會輸出該 object, shape 與 [`tests/fixtures/sessions/`](tests/fixtures/sessions/) 中對應的結果相同.

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
> 完整 analysis JSON 可能很大, 也可能包含 source text, edit body, shell command, absolute path, repository URL, user name, machine identifier 與 token metadata. 分享前請先檢查內容.

Batch analysis 會讀取 Provider 的即時資料. 如果 assistant 在掃描期間繼續寫入 session, 後續執行可能合理地包含更新資料. 未變動的 input 會產生固定順序的輸出.

如果找到的 source 全部讀取失敗或使用無法辨識的 schema, 非互動式 analysis 會回傳 error. 如果只有部分 source 失敗, 成功結果會保留, warning 會寫入 stderr.

`analysis FILE` 對單一檔案內格式錯誤或不受支援的 record 採用相同行為: 在 stdout 保留已解析的 JSON/text/table 輸出, 並將一般性的 skipped-record warning 寫入 stderr.

Codex code mode session 會提供已完成的 JavaScript `exec` cell, 但沒有 nested tool 的結構化 trace. VCT 會將該 cell 計為一次 Bash call, 並在完整 JSON 中保留 source, 但不會猜測 nested Read/Edit/Write operation.

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

## Fetch 指令

**印出某個供應商的原始 quota/usage API 回應 — 不解析、不彙整。**

對 `usage` 面板使用的同一個 quota 端點（Claude / Codex / Copilot / Cursor）發一次請求，直接印出原始 body，方便你檢視 API 的實際結構或確認憑證是否正常。它讀取各供應商已儲存的憑證，而且**不會**刷新 token：token 過期時，請用對應供應商自己的 CLI 重新登入（`claude` / `codex` / `copilot` / `cursor-agent`）。

### 參數

| 參數      | 用途                          |
| --------- | ----------------------------- |
| *(無)*    | 彩色 JSON（預設）             |
| `--json`  | 彩色 JSON                     |
| `--text`  | 攤平成 `key: value`，適合腳本 |
| `--table` | 攤平成 Field / Value 表格     |

### 基本用法

```bash
# 原始 JSON（預設）
vct fetch claude
vct fetch codex
vct fetch copilot
vct fetch cursor

# 攤平成純文字
vct fetch codex --text

# 攤平成 key/value 表格
vct fetch copilot --table
```

> [!NOTE]
> 回應 body 會原樣印到 stdout。遇到 HTTP 錯誤時仍會印出 body 並以非零狀態結束；401/403 會在 stderr 額外印出 `run: <cli> login` 提示。

---

## 設定

vct 會把使用者設定存放在 `~/.vct/config.toml`。這個檔案會在**首次執行時以預設值自動建立**，所以你完全不必手動撰寫，只有想更改某個預設值時才需要編輯它。它由 vct 的型別化設定產生，並在第一行帶有 `#:schema` 指令，因此支援 schema 的 TOML 編輯器（taplo / VS Code 的 "Even Better TOML"）會提供自動補全與驗證。你也可以用 `vct config schema` 自行印出該 schema。由舊版 vct 產生的檔案會在下次被 vct 讀取時就地升級到目前的版面(也可用 `vct config migrate` 手動觸發),因此升級後絕不會停留在過時的格式上。

```toml
#:schema https://raw.githubusercontent.com/Mai0313/VibeCodingTracker/main/vct.schema.json

[general]
# 未指定 --daily/--weekly/--monthly/--all flag 時使用的預設時間範圍。
# 可選值："daily" | "weekly" | "monthly" | "all"
default_time_range = "all"

[usage]
# 啟動 usage 儀表板時，是否先把不同 provider 前綴的 model 合併。
# 可用 `m` 即時切換;最後的狀態會存回這裡。
merge_models = false
# usage TUI 自動刷新的間隔秒數（最少 1）。
refresh_interval = 10

[usage.quota]
# 顯示哪些即時額度面板;移除某個名稱即可隱藏該面板,用空列表 ([]) 隱藏整條。
panels = ["claude", "codex", "copilot", "cursor"]
# 每個 provider 共用的即時額度面板輪詢間隔秒數（最少 1）。
refresh_interval = 60

[analysis]
# analysis TUI 自動刷新的間隔秒數（最少 1）。
refresh_interval = 10

[performance]
# CLI session scan 使用的 Rayon worker 數. 0 代表實測最佳的 auto 預設值;
# 正整數會限制在機器的 available parallelism 以內.
scan_threads = 0

[providers]
# 是否把各 provider 的 session 納入 usage / analysis。把某個 provider 設為 false
# 就會完全略過它（不掃描目錄，也不呼叫 API）。
claude = true
codex = true
copilot = true
gemini = true
opencode = true
cursor = true
hermes = true
grok = true

[logging]
# 寫入 ~/.vct/logs/vct-YYYY-MM-DD.log 的最低日誌等級。
# 取值: "off" | "error" | "warn" | "info" | "debug" | "trace"。
level = "warn"
# 保留幾天的每日日誌檔; 更舊的檔案會在啟動時清除。0 表示全部保留。
retention_days = 7
```

| 設定項                         | 效果                                                                                                          |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------- |
| `general.default_time_range`   | 未指定 `--daily/--weekly/--monthly/--all` 時使用的時間範圍。明確指定的 flag 一律優先。                        |
| `usage.merge_models`           | 讓儀表板一開始就是合併狀態;`m` 切換會把你最後的選擇存回這裡。`--merge-providers` 會強制開啟。                 |
| `usage.refresh_interval`       | `usage` 儀表板自動刷新的間隔（秒）。                                                                          |
| `usage.quota.panels`           | 顯示哪些額度面板（`claude` / `codex` / `copilot` / `cursor`）；移除名稱即可隱藏,`[]` 隱藏整條。               |
| `usage.quota.refresh_interval` | 每個即時額度面板的輪詢間隔（秒）；數值越大越不容易觸發 provider 的速率限制。                                  |
| `analysis.refresh_interval`    | `analysis` 儀表板自動刷新的間隔（秒）。                                                                       |
| `performance.scan_threads`     | CLI scan worker 數. `0` 優先採用正數的 `RAYON_NUM_THREADS`, 否則最多使用兩個 worker; 所有值都受 CPU 數量限制. |
| `providers.*`                  | 設為 `false` 時完全略過某個 provider（不掃描、不呼叫 API），沒在用的話很方便。                                |
| `logging.level`                | 寫入日誌檔的最低等級（`off`..`trace`）；絕不會印到終端機。                                                    |
| `logging.retention_days`       | 保留幾天的每日日誌檔；更舊的 `vct-*.log` 會在啟動時清除（`0` 表示全部保留）。                                 |

> [!NOTE]
> vct 會把診斷訊息寫入 `~/.vct/logs/vct-YYYY-MM-DD.log`（純文字，只寫檔案，絕不顯示在儀表板上）。健康運行時保持安靜（預設等級 `warn`），而且檔案是延遲建立的，所以一次正常執行不會留下任何檔案。當額度抓取失敗或某個 session 被略過時，原因就記錄在這裡——需要完整細節時把 `logging.level` 調到 `debug`。

> [!NOTE]
> Cursor 的 `usage` 是從對話庫產生的**本地估算**，因此行為與 Claude Code / Codex / Copilot / Gemini 一致（全都是從本地 session 檔案計算），而且不需要連網。這個估算會低估 Cursor 的真實花費，因為其中有很大一部分是以 Cursor 內部的 model 名稱計費，而本地資料無法為這些名稱定價，所以請把 Cursor 的費用視為概估值。

### 管理設定檔

```bash
# 印出設定檔路徑
vct config path

# 印出目前的設定
vct config show

# 用 $VISUAL / $EDITOR 開啟檔案（找不到時回退到 vi / notepad）
vct config edit

# 印出 JSON schema（可用以下指令重新產生：vct config schema > vct.schema.json）
vct config schema

# 就地把舊格式檔案升級到目前的版面
vct config migrate
```

---

## 智慧定價系統

### 運作方式

1. **自動更新**: 每個 UTC 日期從 [LiteLLM](https://github.com/BerriAI/litellm) 取得一次最新價格
2. **驗證後 cache**: 只接受成功且包含實際價格的 JSON model map, 再 atomic 寫入 `~/.vct/`
3. **確定性比對**: 即使 model 名稱含有版本或 provider 前綴, 也會選擇最具體的配對
4. **失敗保護**: 取得失敗不會覆蓋有效 cache, vct 會保留舊 map, 並在五分鐘 backoff 後才再次嘗試

### 模型比對

**優先順序**：

1. **完全比對**：`claude-sonnet-4` → `claude-sonnet-4`
2. **正規化比對**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. **子字串比對**：`custom-gpt-4` → `gpt-4`
4. **模糊比對（AI 驅動）**：使用 Jaro-Winkler 相似度（70% 門檻值）
5. **備援方案**：若無法配對則顯示 $0.00

泛用的佔位名稱（例如 cursor-agent 在 auto 模式寫入的 `default`）與過短的名稱不會進行子字串或模糊比對——寧可不計價，也不撿相似名稱的價格。

### 費用細節

- **Context tier 以單一 request 計**：LiteLLM 的「above Nk tokens」費率（如 GPT-5.x 超過 272k、Gemini 超過 200k）只套用在自身 prompt context 超過門檻的那些 request 上。沒有 per-request 粒度的 provider（以及離線掃描）一律以基本費率計價，因此這類 model 的費用是下界。
- **不只 token**：Claude 的網頁搜尋工具呼叫（`server_tool_use.web_search_requests`）會在 token 費用之外，按每次查詢計費；其他所有 model 的每次查詢費用皆為 $0。
- **OpenCode**：只有在 LiteLLM **完全比對**成功時，才會依 token 為新型 model 計價；若沒有完全比對，vct 會採信該 assistant 訊息本身儲存的費用，而不是從名稱相近的 model 去猜測。
- **Hermes**：與 OpenCode 相同，LiteLLM **完全比對**成功時依 token 計價，否則使用 Hermes 本身儲存的費用。
- **Grok**：只會把 `contextTokensUsed` 當成 cache-read token 計價（若該 model 沒有公布 cache-read 費率則改用 input 費率）；這是單一當下的本地 context 估算，不是累計的 billed usage。
- **原始 cache**: 每日 cache 儲存經過篩選的 LiteLLM 上游原始 JSON (而非衍生結構), 因此 tiered / batch 定價不需重新取得即可使用. 每個 pricing map 各自擁有一個小型 process-local LRU, 重複查詢維持低成本, 也不會在不同 map 之間互相污染.

---

## Docker 支援

```bash
# Build image
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .
```
