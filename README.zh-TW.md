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
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/tree/master?tab=License-1-ov-file)
[![Star on GitHub](https://img.shields.io/github/stars/Mai0313/VibeCodingTracker?style=social&label=Star)](https://github.com/Mai0313/VibeCodingTracker)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</div>

**即時追蹤你的 AI 程式設計花費。** Vibe Coding Tracker 是一款以 Rust 打造的輕量、高效能 CLI 工具，能監控與分析你在 Claude Code、Codex、Copilot 及 Gemini 的使用狀況——提供詳細的費用明細、token 統計資料與程式碼操作分析，同時維持極低的記憶體使用量。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：CLI 範例使用簡短別名 `vct`。如果你是透過 npm/pip/cargo 安裝，執行檔可能命名為 `vibe_coding_tracker` 或 `vct`。如有需要，請建立別名或在執行指令時將 `vct` 替換為完整名稱。

---

## 🎯 為什麼選擇 Vibe Coding Tracker？

### 💰 掌握你的花費

不用再猜測 AI 程式設計工作階段花了多少錢。透過 [LiteLLM](https://github.com/BerriAI/litellm) 自動更新價格，取得**即時費用追蹤**。

### 🪶 超輕量

以 Rust 打造，資源佔用極低。互動式 TUI 儀表板穩定後通常維持在 **~50 MB 以內的常駐記憶體**，即使硬碟上有數百個長 context session 也一樣——不用 Electron，不用臃腫的執行環境。usage 路徑用精簡模式串流 parse 每個 session 檔案並繞過 cache，啟動時也會調整 glibc 的 arena 數量，讓長時間執行的 RSS 保持誠實。

### 📊 精美視覺化

選擇你偏好的檢視方式：

- **互動式儀表板**：自動更新的終端機 UI，即時顯示最新資訊
- **靜態報表**：專業的表格格式，適合撰寫文件
- **腳本友好**：純文字及 JSON 輸出，方便自動化處理
- **完整精度**：匯出精確費用供會計使用

### 🚀 零設定

自動偵測並處理 Claude Code、Codex、Copilot 及 Gemini 的日誌檔。不需要任何設定——直接執行就能分析。

### 🎨 豐富洞察

- 依模型與日期分類的 token 使用量
- 依 cache 類型（讀取 / 建立）的費用明細
- 檔案操作追蹤（編輯、讀取、寫入行數）
- 工具呼叫歷史（Bash、Edit、Read、Write、TodoWrite）
- 每個供應商的每日平均值

---

## ✨ 主要功能

| 功能                | 說明                                                      |
| ------------------- | --------------------------------------------------------- |
| 🤖 **多供應商支援** | Claude Code、Codex、Copilot 及 Gemini——一站整合           |
| 💵 **智慧定價**     | 模糊模型比對 + 每日從 LiteLLM cache 更新                  |
| 🎨 **4 種顯示模式** | 互動式 TUI、靜態表格、純文字及 JSON                       |
| 📈 **雙重分析**     | Token / 費用統計（`usage`）+ 程式碼操作統計（`analysis`） |
| 🪶 **超輕量**       | TUI 常駐記憶體 ~50 MB 以內、串流 JSONL 解析——以 Rust 打造 |
| 🔄 **即時更新**     | 即時儀表板每秒自動刷新                                    |
| 💾 **高效快取**     | 智慧每日 cache 減少 API 呼叫次數                          |

---

## 🚀 快速開始

### 安裝

選擇最適合你的安裝方式：

> 👨‍💻 **開發者**：如果你想從原始碼建置或參與開發，請參閱 [CONTRIBUTING.md](CONTRIBUTING.md)。

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

## 📖 指令指南

### 🔍 快速參考

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

| Flag        | 範圍                       |
| ----------- | -------------------------- |
| `--daily`   | 今天更新過的 session       |
| `--weekly`  | 本 ISO 週（週一 → 今天）   |
| `--monthly` | 本自然月                   |
| `--all`     | 磁碟上所有 session（預設） |

---

## 💰 Usage 指令

**追蹤你在所有 AI 程式設計工作階段的花費。**

### Flag 一覽

| Flag                                           | 用途                         |
| ---------------------------------------------- | ---------------------------- |
| *(不帶參數)*                                   | 互動式 TUI 儀表板（預設）    |
| `--table`                                      | 靜態表格，不啟動 TUI         |
| `--text`                                       | 純文字，適合腳本處理         |
| `--json`                                       | JSON 輸出，附帶 pricing 資訊 |
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

# 時間範圍與輸出格式可自由組合
vct usage --weekly
vct usage --table --monthly
vct usage --json --daily
```

> [!NOTE]
> Model 列會依 cost 由小到大排序，所以花費最高的 model 會緊鄰在 `TOTAL` 列上方。這個排序會套用到互動式儀表板、`--table` 與 `--text` 三種輸出；`--json` 也會保持相同順序。

### 預覽：互動式儀表板（`vct usage`）

```
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    📊 Token Usage Statistics                                │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Model                              Input     Output    Cache Read  Cache Create  Total Cost │
│                                                                                             │
│ gemini-3.1-pro-preview             129,115   10,339    67,385      0             $0.40      │
│ claude-haiku-4-5-20251001          5,567     19,769    4,627,938   619,816       $1.34      │
│ claude-opus-4-6                    25,651    179,066   40,830,154  2,572,258     $77.59     │
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
│   💰 Total: $79.33  |  🔢 Tokens: 49,087,058  |  📊 Models: 3  |  🧠 Memory: 42.8 MB        │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
                          Press 'q', 'Esc', 'Ctrl+C' to quit | Press 'r' to refresh
```

### 掃描範圍

此工具會自動掃描以下目錄：

- `~/.claude/projects/**/*.jsonl`（Claude Code，遞迴包含 subagent 日誌）
- `~/.codex/sessions/**/*.jsonl`（Codex，遞迴包含每日子目錄）
- `~/.copilot/session-state/<sessionId>/events.jsonl`（Copilot CLI）
- `~/.gemini/tmp/<project_hash>/chats/*.jsonl`（Gemini CLI）

---

## 📊 Analysis 指令

**深入分析程式碼操作——精確掌握你的 AI 助手做了哪些事。**

### Flag 一覽

| Flag                                           | 用途                                                            |
| ---------------------------------------------- | --------------------------------------------------------------- |
| *(不帶參數)*                                   | 互動式 TUI 儀表板，涵蓋所有 session                             |
| `--path <FILE>`                                | 分析單一 JSONL/JSON 對話檔案（stdout 輸出 JSON）                |
| `--table`                                      | 靜態表格，附帶每日平均值                                        |
| `--output <FILE>`                              | 將結果以格式化 JSON 存成檔案                                    |
| `--by-provider`                                | 依 provider（claude / codex / copilot / gemini）分組並輸出 JSON |
| `--daily` / `--weekly` / `--monthly` / `--all` | 時間範圍篩選（見上方表格）                                      |

請參考 [`examples/`](examples/) 目錄，裡面有四種 provider 的範例輸入與對應的 JSON 輸出。

### 基本用法

```bash
# Interactive dashboard for all sessions (default)
vct analysis

# Static table output with daily averages
vct analysis --table

# 分析單一對話檔案 → stdout JSON
vct analysis --path ~/.claude/projects/session.jsonl

# Save results to JSON
vct analysis --output report.json

# Group results by provider
vct analysis --by-provider

# Save grouped results
vct analysis --by-provider --output grouped_report.json

# 時間範圍與輸出格式可自由組合
vct analysis --weekly
vct analysis --table --monthly
vct analysis --by-provider --daily --output today.json
```

### 預覽：互動式儀表板（`vct analysis`）

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
│  📝 Lines: 16,119  |  🔧 Tools: 601  |  📊 Models: 3  |  🧠 Memory: 41.2 MB                │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
                          Press 'q', 'Esc', 'Ctrl+C' to quit | Press 'r' to refresh
```

---

## 🔄 Update 指令

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
📋 Current version: v0.5.10
🔍 Checking for latest release...
✅ Latest version: v0.5.10 — you are up to date!
```

---

## 🏷️ Version 指令

檢視內建的建置資訊（binary version、Rust toolchain、Cargo version）：

```bash
vct version          # 彩色表格
vct version --text   # 每行一個欄位，適合腳本
vct version --json   # 機器可讀的 JSON
```

Binary version 由 `build.rs` 在編譯期透過 `git describe` 寫入，開發版本會附上 commit 數、short SHA 與 `dirty` 後綴。

---

## 💡 智慧定價系統

### 運作方式

1. **自動更新**：每日從 [LiteLLM](https://github.com/BerriAI/litellm) 取得最新價格
2. **智慧快取**：將價格資料儲存於 `~/.vibe_coding_tracker/`，有效期 24 小時
3. **模糊比對**：即使是自訂模型名稱也能找到最佳配對
4. **始終精準**：確保你取得最新的定價資訊

### 模型比對

**優先順序**：

1. ✅ **完全比對**：`claude-sonnet-4` → `claude-sonnet-4`
2. 🔄 **正規化比對**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. 🔍 **子字串比對**：`custom-gpt-4` → `gpt-4`
4. 🎯 **模糊比對（AI 驅動）**：使用 Jaro-Winkler 相似度（70% 門檻值）
5. 💵 **備援方案**：若無法配對則顯示 $0.00

---

## 🐳 Docker 支援

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
