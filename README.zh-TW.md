<center>

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
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</center>

**即時追蹤您的 AI 程式設計成本。** Vibe Coding Tracker 是一個強大的 CLI 工具，幫助您監控和分析 Claude Code、Codex 和 Gemini 的使用情況，提供詳細的成本分解、token 統計和程式碼操作洞察。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：以下 CLI 範例預設使用短別名 `vct`。若你是從原始碼建置，產生的二進位檔名稱為 `vibe_coding_tracker`，可以自行建立別名，或在執行指令時將 `vct` 換成完整名稱。

---

## 🎯 為什麼選擇 Vibe Coding Tracker？

### 💰 了解您的成本

不再疑惑您的 AI 程式設計會話花費多少。透過 [LiteLLM](https://github.com/BerriAI/litellm) 自動更新定價，獲取**即時成本追蹤**。

### 📊 精美的視覺化

選擇您偏好的檢視：

- **互動式儀表板**：自動重新整理的終端 UI，即時更新
- **靜態報表**：專業的表格，適合文件
- **指令碼友善**：純文字和 JSON，便於自動化
- **完整精度**：匯出精確成本，用於財務核算

### 🚀 零設定

自動偵測並處理 Claude Code、Codex 和 Gemini 的日誌。無需設定——只需執行和分析。

### 🎨 豐富的洞察

- 按模型和日期的 token 使用量
- 按快取類型的成本分解
- 檔案操作追蹤
- 命令執行歷史
- Git 儲存庫資訊

---

## ✨ 核心特性

| 特性                | 描述                                       |
| ------------------- | ------------------------------------------ |
| 🤖 **自動偵測**     | 智慧識別 Claude Code、Codex 或 Gemini 日誌 |
| 💵 **智慧定價**     | 模糊模型匹配 + 每日快取以提高速度          |
| 🎨 **4 種顯示模式** | 互動式、表格、文字和 JSON 輸出             |
| 📈 **全面統計**     | Token、成本、檔案操作和工具呼叫            |
| ⚡ **高效能**       | 使用 Rust 建置，速度快且可靠               |
| 🔄 **即時更新**     | 儀表板每秒重新整理                         |
| 💾 **高效快取**     | 智慧的每日快取減少 API 呼叫                |

---

## 🚀 快速開始

### 安裝

選擇最適合您的安裝方式：

#### 方式 1: 從 npm 安裝 (推薦 ✨)

**最簡單的安裝方式** - 包含針對您平台預編譯的二進位檔案，無需建置步驟！

選擇以下任一套件名稱（三者完全相同）：

```bash
# 主要套件
npm install -g vibe-coding-tracker

# 帶 scope 的短別名
npm install -g @mai0313/vct

# 帶 scope 的完整名稱
npm install -g @mai0313/vibe-coding-tracker
```

**前置條件**: [Node.js](https://nodejs.org/) v22 或更高版本

**支援平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### 方式 2: 從 PyPI 安裝

**適合 Python 使用者** - 包含針對您平台預編譯的二進位檔案，無需建置步驟！

```bash
# 使用 pip 安裝
pip install vibe_coding_tracker

# 使用 uv 安裝（推薦，安裝速度更快）
uv pip install vibe_coding_tracker
```

**前置條件**: Python 3.8 或更高版本

**支援平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### 方式 3: 從 crates.io 安裝

使用 Cargo 從 Rust 官方套件庫安裝：

```bash
cargo install vibe_coding_tracker
```

**前置條件**: [Rust 工具鏈](https://rustup.rs/) 1.85 或更高版本

> **注意**: 此專案使用 **Rust 2024 edition**，需要 Rust 1.85+。如需更新，請執行 `rustup update`。

#### 方式 4: 從原始碼編譯

適合想要自訂建置或貢獻開發的使用者：

```bash
# 1. 複製儲存庫
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker

# 2. 建置 release 版本
cargo build --release

# 3. 二進位檔案位置
./target/release/vibe_coding_tracker

# 4. （可選）建立短別名
# Linux/macOS:
sudo ln -sf "$(pwd)/target/release/vibe_coding_tracker" /usr/local/bin/vct

# 或安裝到使用者目錄:
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct
# 確保 ~/.local/bin 在您的 PATH 中
```

**前置條件**: [Rust 工具鏈](https://rustup.rs/) 1.85 或更高版本

> **注意**: 此專案使用 **Rust 2024 edition**，需要 Rust 1.85+。如需更新，請執行 `rustup update`。

#### 方式 5: 透過 Curl 快速安裝 (Linux/macOS)

**一行指令安裝** - 自動偵測您的平台並安裝最新版本：

```bash
curl -fsSLk https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.sh | bash
```

**前置條件**: `curl` 和 `tar` (通常已預先安裝)

**功能說明**:

- 自動偵測您的作業系統和架構
- 從 GitHub 下載最新版本
- 解壓縮並安裝到 `/usr/local/bin` 或 `~/.local/bin`
- 自動建立 `vct` 短別名
- 跳過 SSL 驗證，適用於受限網路環境

**支援平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)

#### 方式 6: 透過 PowerShell 快速安裝 (Windows)

**一行指令安裝** - 自動偵測您的架構並安裝最新版本：

```powershell
powershell -ExecutionPolicy ByPass -c "[System.Net.ServicePointManager]::ServerCertificateValidationCallback={$true}; irm https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.ps1 | iex"
```

**前置條件**: PowerShell 5.0 或更高版本 (Windows 10+ 已內建)

**功能說明**:

- 自動偵測您的 Windows 架構 (x64 或 ARM64)
- 從 GitHub 下載最新版本
- 安裝到 `%LOCALAPPDATA%\Programs\VibeCodingTracker`
- 自動建立 `vct.exe` 短別名
- 自動加入使用者 PATH
- 跳過 SSL 驗證，適用於受限網路環境

**注意**: 您可能需要重新啟動終端機，PATH 變更才會生效。

**支援平台**:

- Windows 10/11 (x64, ARM64)

### 首次執行

```bash
# 使用互動式儀表板檢視使用量（已設定短別名時）
vct usage

# 或使用完整名稱
./target/release/vibe_coding_tracker usage

# 分析特定對話
./target/release/vibe_coding_tracker analysis --path ~/.claude/projects/session.jsonl
```

> 💡 **提示**：使用 `vct` 作為 `vibe_coding_tracker` 的短別名，節省輸入時間——可透過 `ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct` 手動建立。

---

## 📖 命令指南

### 🔍 快速參考

```bash
vct <命令> [選項]
# 若未設定別名，請改用 `vibe_coding_tracker`

命令：
usage       顯示 token 使用量和成本（預設：互動式）
analysis    分析對話檔案並匯出資料
version     顯示版本資訊
update      從 GitHub releases 更新到最新版本
help        顯示說明資訊
```

---

## 💰 Usage 命令

**追蹤您所有 AI 程式設計會話的支出。**

### 基本用法

```bash
# 互動式儀表板（推薦）
vct usage

# 靜態表格，適合報表
vct usage --table

# 純文字，適合指令碼
vct usage --text

# JSON，適合資料處理
vct usage --json
```

### 您將獲得什麼

該工具自動掃描這些目錄：

- `~/.claude/projects/*.jsonl`（Claude Code）
- `~/.codex/sessions/*.jsonl`（Codex）
- `~/.gemini/tmp/<project_hash>/chats/*.json`（Gemini）

### 🎨 互動式模式（預設）

**每秒更新的即時儀表板**

```
┌──────────────────────────────────────────────────────────────────┐
│                  📊 Token 使用統計                               │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬──────────────────────┬────────────┬────────────┬────────────┬──────────────┬────────────┬────────────┐
│ 日期       │ 模型                 │ 輸入       │ 輸出       │ 快取讀取   │ 快取建立     │ 總計       │ 成本 (USD) │
├────────────┼──────────────────────┼────────────┼────────────┼────────────┼──────────────┼────────────┼────────────┤
│ 2025-10-01 │ claude-sonnet-4-20…  │ 45,230     │ 12,450     │ 230,500    │ 50,000       │ 338,180    │ $2.15      │
│ 2025-10-02 │ claude-sonnet-4-20…  │ 32,100     │ 8,920      │ 180,000    │ 30,000       │ 251,020    │ $1.58      │
│ 2025-10-03 │ claude-sonnet-4-20…  │ 28,500     │ 7,200      │ 150,000    │ 25,000       │ 210,700    │ $1.32      │
│ 2025-10-03 │ gpt-4-turbo          │ 15,000     │ 5,000      │ 0          │ 0            │ 20,000     │ $0.25      │
│            │ 總計                 │ 120,830    │ 33,570     │ 560,500    │ 105,000      │ 819,900    │ $5.30      │
└────────────┴──────────────────────┴────────────┴────────────┴────────────┴──────────────┴────────────┴────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 💰 總成本：$5.30  |  🔢 總 Token：819,900  |  📅 條目：4  |  ⚡ CPU：2.3%  |  🧠 記憶體：12.5 MB                    │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                            📈 每日平均                                                            │
│                                                                                                                   │
│  Claude Code: 266,667 tokens/天  |  $1.68/天                                                                     │
│  Codex: 20,000 tokens/天  |  $0.25/天                                                                            │
│  總體: 204,975 tokens/天  |  $1.33/天                                                                            │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**特性**：

- ✨ 每秒自動重新整理
- 🎯 突顯今日條目
- 🔄 顯示最近更新的列
- 💾 顯示記憶體使用量
- 📊 彙總統計
- 📈 按提供者（Claude Code、Codex、Gemini）的每日平均值

**控制**：按 `q`、`Esc` 或 `Ctrl+C` 退出

### 📋 靜態表格模式

**非常適合文件和報表**

```bash
vct usage --table
```

```
📊 Token 使用統計

╔════════════╦══════════════════════╦════════════╦════════════╦════════════╦══════════════╦══════════════╦════════════╗
║ 日期       ║ 模型                 ║ 輸入       ║ 輸出       ║ 快取讀取   ║ 快取建立     ║ 總 Token     ║ 成本 (USD) ║
╠════════════╬══════════════════════╬════════════╬════════════╬════════════╬══════════════╬══════════════╬════════════╣
║ 2025-10-01 ║ claude-sonnet-4-20…  ║ 45,230     ║ 12,450     ║ 230,500    ║ 50,000       ║ 338,180      ║ $2.15      ║
║ 2025-10-02 ║ claude-sonnet-4-20…  ║ 32,100     ║ 8,920      ║ 180,000    ║ 30,000       ║ 251,020      ║ $1.58      ║
║ 2025-10-03 ║ claude-sonnet-4-20…  ║ 28,500     ║ 7,200      ║ 150,000    ║ 25,000       ║ 210,700      ║ $1.32      ║
║ 2025-10-03 ║ gpt-4-turbo          ║ 15,000     ║ 5,000      ║ 0          ║ 0            ║ 20,000       ║ $0.25      ║
║            ║ 總計                 ║ 120,830    ║ 33,570     ║ 560,500    ║ 105,000      ║ 819,900      ║ $5.30      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝

📈 每日平均（按提供者）

╔═════════════╦════════════════╦══════════════╦══════╗
║ 提供者      ║ 平均 Token/天  ║ 平均成本/天  ║ 天數 ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Claude Code ║ 266,667        ║ $1.68        ║ 3    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Codex       ║ 20,000         ║ $0.25        ║ 1    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ 總體        ║ 204,975        ║ $1.33        ║ 4    ║
╚═════════════╩════════════════╩══════════════╩══════╝
```

### 📝 文字模式

**非常適合指令碼和解析**

```bash
vct usage --text
```

```
2025-10-01 > claude-sonnet-4-20250514: $2.154230
2025-10-02 > claude-sonnet-4-20250514: $1.583450
2025-10-03 > claude-sonnet-4-20250514: $1.321200
2025-10-03 > gpt-4-turbo: $0.250000
```

### 🗂️ JSON 模式

**完整精度，用於財務核算和整合**

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

### 🔍 輸出對比

| 特性         | 互動式 | 表格  | 文字      | JSON               |
| ------------ | ------ | ----- | --------- | ------------------ |
| **最適合**   | 監控   | 報表  | 指令碼    | 整合               |
| **成本格式** | $2.15  | $2.15 | $2.154230 | 2.1542304567890123 |
| **更新**     | 即時   | 靜態  | 靜態      | 靜態               |
| **顏色**     | ✅     | ✅    | ❌        | ❌                 |
| **可解析**   | ❌     | ❌    | ✅        | ✅                 |

### 💡 使用場景

- **預算追蹤**：監控您的每日 AI 支出
- **成本最佳化**：識別昂貴的會話
- **團隊報告**：為管理層產生使用報告
- **帳單**：匯出精確成本用於開票
- **監控**：活躍開發的即時儀表板

---

## 📊 Analysis 命令

**深入了解對話檔案 - 單檔案或批次分析。**

### 基本用法

```bash
# 單檔案：分析並顯示
vct analysis --path ~/.claude/projects/session.jsonl

# 單檔案：儲存到檔案
vct analysis --path ~/.claude/projects/session.jsonl --output report.json

# 批次：使用互動式表格分析所有會話（預設）
vct analysis

# 批次：靜態表格輸出並顯示每日平均
vct analysis --table

# 批次：將彙總結果儲存為 JSON
vct analysis --output batch_report.json

# 批次並依提供者分組：輸出完整的 records，依提供者分組（JSON 格式）
vct analysis --all

# 將分組結果儲存到檔案
vct analysis --all --output grouped_report.json
```

### 您將獲得什麼

**單檔案分析**：

- **Token 使用量**：按模型的輸入、輸出和快取統計
- **檔案操作**：每次讀取、寫入和編輯的完整詳情
- **命令歷史**：所有執行的 shell 命令
- **工具使用**：每種工具類型的使用次數
- **中繼資料**：使用者、機器 ID、Git 儲存庫、時間戳記

**批次分析**：

- **彙總指標**：按日期和模型分組
- **行數統計**：編輯、讀取和寫入操作
- **工具統計**：Bash、Edit、Read、TodoWrite、Write 計數
- **互動式顯示**：即時 TUI 表格（預設）
- **JSON 匯出**：結構化資料用於進一步處理

### 範例輸出 - 單檔案

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

### 範例輸出 - 批次分析

**互動式表格**（執行 `vct analysis` 時的預設輸出）：

```
┌──────────────────────────────────────────────────────────────────┐
│                  🔍 分析統計                                     │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬────────────────────┬────────────┬────────────┬────────────┬──────┬──────┬──────┬───────────┬───────┐
│ 日期       │ 模型               │ 編輯行數   │ 讀取行數   │ 寫入行數   │ Bash │ Edit │ Read │ TodoWrite │ Write │
├────────────┼────────────────────┼────────────┼────────────┼────────────┼──────┼──────┼──────┼───────────┼───────┤
│ 2025-10-02 │ claude-sonnet-4-5…│ 901        │ 11,525     │ 53         │ 13   │ 26   │ 27   │ 10        │ 1     │
│ 2025-10-03 │ claude-sonnet-4-5…│ 574        │ 10,057     │ 1,415      │ 53   │ 87   │ 78   │ 30        │ 8     │
│ 2025-10-03 │ gpt-5-codex        │ 0          │ 1,323      │ 0          │ 75   │ 0    │ 20   │ 0         │ 0     │
│            │ 總計               │ 1,475      │ 22,905     │ 1,468      │ 141  │ 113  │ 125  │ 40        │ 9     │
└────────────┴────────────────────┴────────────┴────────────┴────────────┴──────┴──────┴──────┴───────────┴───────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 📝 總行數：25,848  |  🔧 總工具：428  |  📅 條目：3  |  ⚡ CPU：1.8%  |  🧠 記憶體：8.2 MB                        │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    📈 每日平均（按提供者）                                                      │
│                                                                                                                 │
│  🤖 Claude Code: 737 編輯/天 | 10,791 讀取/天 | 734 寫入/天 | 3 天                                             │
│  💻 Codex: 0 編輯/天 | 1,323 讀取/天 | 0 寫入/天 | 1 天                                                         │
│  ⭐ 所有提供者: 491 編輯/天 | 7,635 讀取/天 | 489 寫入/天 | 3 天                                                │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**靜態表格模式**（使用 `--table`）：

```bash
vct analysis --table
```

```
🔍 分析統計

╔════════════╦════════════════════╦════════════╦════════════╦═════════════╦══════╦═══════╦═══════╦═══════════╦═══════╗
║ 日期       ║ 模型               ║ 編輯行數   ║ 讀取行數   ║ 寫入行數    ║ Bash ║  Edit ║  Read ║ TodoWrite ║ Write ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-02 ║ claude-sonnet-4-5…║ 901        ║ 11,525     ║ 53          ║ 13   ║ 26    ║ 27    ║ 10        ║ 1     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-03 ║ claude-sonnet-4-5…║ 574        ║ 10,057     ║ 1,415       ║ 53   ║ 87    ║ 78    ║ 30        ║ 8     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-03 ║ gpt-5-codex        ║ 0          ║ 1,323      ║ 0           ║ 75   ║ 0     ║ 20    ║ 0         ║ 0     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║            ║ 總計               ║ 1,475      ║ 22,905     ║ 1,468       ║ 141  ║ 113   ║ 125   ║ 40        ║ 9     ║
╚════════════╩════════════════════╩════════════╩════════════╩═════════════╩══════╩═══════╩═══════╩═══════════╩═══════╝

📈 每日平均（按提供者）

╔══════════════╦═══════════╦═══════════╦════════════╦══════════╦══════════╦══════════╦══════════╦═══════════╦══════╗
║ 提供者       ║ 編輯/天   ║ 讀取/天   ║ 寫入/天    ║ Bash/天  ║ Edit/天  ║ Read/天  ║ Todo/天  ║ Write/天  ║ 天數 ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ 🤖 Claude Code ║ 737.5     ║ 10,791    ║ 734        ║ 33.0     ║ 56.5     ║ 52.5     ║ 20.0     ║ 4.5       ║ 2    ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ 💻 Codex       ║ 0         ║ 1,323     ║ 0          ║ 75.0     ║ 0.0      ║ 20.0     ║ 0.0      ║ 0.0       ║ 1    ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ ⭐ 所有提供者  ║ 491.7     ║ 7,635     ║ 489.3      ║ 47.0     ║ 37.7     ║ 41.7     ║ 13.3     ║ 3.0       ║ 3    ║
╚══════════════╩═══════════╩═══════════╩════════════╩══════════╩══════════╩══════════╩══════════╩═══════════╩══════╝
```

**JSON 匯出**（使用 `--output`）：

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

### 💡 使用場景

**單檔案分析**：

- **使用稽核**：追蹤 AI 在每個會話中做了什麼
- **成本歸因**：計算每個專案或功能的成本
- **合規性**：匯出詳細的活動日誌
- **分析**：了解程式設計模式和工具使用

**批次分析**：

- **生產力追蹤**：監控隨時間推移的編碼活動
- **工具使用模式**：識別所有會話中最常用的工具
- **模型比較**：比較不同 AI 模型之間的效率
- **歷史分析**：按日期追蹤程式碼操作趨勢

---

## 🔧 Version 命令

**檢查您的安裝。**

```bash
# 格式化輸出
vct version

# JSON 格式
vct version --json

# 純文字
vct version --text
```

### 輸出

```
🚀 Vibe Coding Tracker

╔════════════════╦═════════╗
║ 版本           ║ 0.1.0   ║
╠════════════════╬═════════╣
║ Rust 版本      ║ 1.89.0  ║
╠════════════════╬═════════╣
║ Cargo 版本     ║ 1.89.0  ║
╚════════════════╩═════════╝
```

---

## 🔄 Update 命令

**自動保持安裝版本為最新。**

update 命令會檢查 GitHub releases 並為您的平台下載最新版本。

### 基本用法

```bash
# 互動式更新（會詢問確認）
vct update

# 僅檢查更新而不安裝
vct update --check

# 強制更新，不顯示確認提示
vct update --force
```

### 運作原理

1. **檢查最新版本**：從 GitHub API 取得最新 release
2. **比較版本**：比較目前版本與最新可用版本
3. **下載二進位檔**：下載適合您平台的二進位檔（Linux/macOS/Windows）
4. **智慧替換**：
   - **Linux/macOS**：自動替換二進位檔（將舊版本備份為 `.old`）
   - **Windows**：下載為 `.new` 並建立批次腳本以安全替換

### 平台支援

update 命令會自動偵測您的平台並下載正確的壓縮檔：

- **Linux**：`vibe_coding_tracker-v{版本}-linux-x64-gnu.tar.gz`、`vibe_coding_tracker-v{版本}-linux-arm64-gnu.tar.gz`
- **macOS**：`vibe_coding_tracker-v{版本}-macos-x64.tar.gz`、`vibe_coding_tracker-v{版本}-macos-arm64.tar.gz`
- **Windows**：`vibe_coding_tracker-v{版本}-windows-x64.zip`、`vibe_coding_tracker-v{版本}-windows-arm64.zip`

### Windows 更新流程

在 Windows 上，無法在程式執行時替換二進位檔。update 命令會：

1. 將新版本下載為 `vct.new`
2. 建立更新腳本（`update_vct.bat`）
3. 顯示完成更新的說明

關閉應用程式後執行批次腳本以完成更新。

### 自動更新通知

**自動取得新版本通知。**

啟動 `vct` 時，程式會每 24 小時自動檢查一次更新，如果有新版本可用會顯示通知。通知會智慧偵測您的安裝方式並顯示對應的更新指令：

- **npm**: `npm update -g @mai0313/vct`
- **pip**: `pip install --upgrade vibe_coding_tracker`
- **cargo**: `cargo install vibe_coding_tracker --force`
- **manual**: `vct update` 或重新執行安裝腳本

這確保您始終使用正確的更新方式，避免版本衝突。檢查在背景靜默執行，不會影響您的正常使用。

---

## 💡 智慧定價系統

### 運作原理

1. **自動更新**：每天從 [LiteLLM](https://github.com/BerriAI/litellm) 取得定價
2. **智慧快取**：在 `~/.vibe_coding_tracker/` 中儲存定價 24 小時
3. **模糊匹配**：即使對於自訂模型名稱也能找到最佳匹配
4. **始終準確**：確保您取得最新的定價

### 模型匹配

**優先順序**：

1. ✅ **精確匹配**：`claude-sonnet-4` → `claude-sonnet-4`
2. 🔄 **規範化**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. 🔍 **子字串**：`custom-gpt-4` → `gpt-4`
4. 🎯 **模糊（AI 驅動）**：使用 Jaro-Winkler 相似度（70% 閾值）
5. 💵 **後備**：如果找不到匹配則顯示 $0.00

### 成本計算

```
總成本 = (輸入 Token × 輸入成本) +
         (輸出 Token × 輸出成本) +
         (快取讀取 × 快取讀取成本) +
         (快取建立 × 快取建立成本)
```

---

## 🐳 Docker 支援

```bash
# 建置映像
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# 使用您的會話執行
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```

---

## 🔍 疑難排解

### 定價資料未載入

```bash
# 檢查快取
ls -la ~/.vibe_coding_tracker/

# 強制重新整理
rm -rf ~/.vibe_coding_tracker/
vct usage

# 除錯模式
RUST_LOG=debug vct usage
```

### 沒有顯示使用資料

```bash
# 驗證會話目錄
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/
ls -la ~/.gemini/tmp/

# 統計會話檔案
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
find ~/.gemini/tmp -name "*.json" | wc -l
```

### Analysis 命令失敗

```bash
# 驗證 JSONL 格式
jq empty < your-file.jsonl

# 檢查檔案權限
ls -la your-file.jsonl

# 使用除錯輸出執行
RUST_LOG=debug vct analysis --path your-file.jsonl
```

### 互動式模式問題

```bash
# 如果中斷則重設終端
reset

# 檢查終端類型
echo $TERM  # 應該是 xterm-256color 或相容

# 使用靜態表格作為後備
vct usage --table
```

---

## ⚡ 效能

使用 Rust 建置，追求**速度**和**可靠性**：

| 操作             | 時間   |
| ---------------- | ------ |
| 解析 10MB JSONL  | ~320ms |
| 分析 1000 個事件 | ~45ms  |
| 載入快取的定價   | ~2ms   |
| 互動式重新整理   | ~30ms  |

**二進位大小**：~3-5 MB（剝離後）

---

## 📚 了解更多

- **開發者文件**：參見 [.github/copilot-instructions.md](.github/copilot-instructions.md)
- **報告問題**：[GitHub Issues](https://github.com/Mai0313/VibeCodingTracker/issues)
- **原始碼**：[GitHub 儲存庫](https://github.com/Mai0313/VibeCodingTracker)

---

## 🤝 貢獻

歡迎貢獻！方法如下：

1. Fork 儲存庫
2. 建立您的功能分支
3. 進行變更
4. 提交拉取請求

有關開發設定和指南，請參見 [.github/copilot-instructions.md](.github/copilot-instructions.md)。

---

## 📄 授權

MIT 授權 - 詳見 [LICENSE](LICENSE)。

---

## 🙏 鳴謝

- [LiteLLM](https://github.com/BerriAI/litellm) 提供模型定價資料
- Claude Code、Codex 和 Gemini 團隊建立了出色的 AI 程式設計助手
- Rust 社群提供了優秀的工具

---

<center>

**省錢。追蹤使用量。更智慧地編寫程式。**

如果您覺得有用，請[⭐ Star 這個專案](https://github.com/Mai0313/VibeCodingTracker)！

使用 🦀 Rust 製作

</center>
