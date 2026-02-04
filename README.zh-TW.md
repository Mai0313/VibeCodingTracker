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

**即時追蹤您的 AI 程式設計成本。** Vibe Coding Tracker 是一個強大的 CLI 工具，幫助您監控和分析 Claude Code、Codex、Copilot 和 Gemini 的使用情況，提供詳細的成本分解、token 統計和程式碼操作洞察。

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

自動偵測並處理 Claude Code、Codex、Copilot 和 Gemini 的日誌。無需設定——只需執行和分析。

### 🎨 豐富的洞察

- 按模型和日期的 token 使用量
- 按快取類型的成本分解
- 檔案操作追蹤
- 命令執行歷史
- Git 儲存庫資訊

---

## ✨ 核心特性

| 特性                | 描述                                                |
| ------------------- | --------------------------------------------------- |
| 🤖 **自動偵測**     | 智慧識別 Claude Code、Codex、Copilot 或 Gemini 日誌 |
| 💵 **智慧定價**     | 模糊模型匹配 + 每日快取以提高速度                   |
| 🎨 **4 種顯示模式** | 互動式、表格、文字和 JSON 輸出                      |
| 📈 **全面統計**     | Token、成本、檔案操作和工具呼叫                     |
| ⚡ **高效能**       | 使用 Rust 建置，速度快且可靠                        |
| 🔄 **即時更新**     | 儀表板每秒重新整理                                  |
| 💾 **高效快取**     | 智慧的每日快取減少 API 呼叫                         |

---

## 🚀 快速開始

### 安裝

選擇最適合您的安裝方式：

#### 方式 1: 從原始碼編譯 (推薦開發者 ✨)

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

#### 方式 2: 從 crates.io 安裝

使用 Cargo 從 Rust 官方套件庫安裝：

```bash
cargo install vibe_coding_tracker
```

#### 方式 3: 從 npm 安裝

**前置條件**: [Node.js](https://nodejs.org/) v22 或更高版本

選擇以下任一套件名稱（三者完全相同）：

```bash
# 主要套件
npm install -g vibe-coding-tracker

# 帶 scope 的短別名
npm install -g @mai0313/vct

# 帶 scope 的完整名稱
npm install -g @mai0313/vibe-coding-tracker
```

#### 方式 4: 從 PyPI 安裝

**前置條件**: Python 3.8 或更高版本

```bash
pip install vibe_coding_tracker
# 或使用 uv
uv pip install vibe_coding_tracker
```

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
# 若未設定別名，請改用 `vibe_coding_tracker`完整二進位名稱

命令：
analysis    分析對話檔案並匯出資料（支援單檔案或所有會話）
usage       顯示 token 使用量統計
version     顯示版本資訊
update      從 GitHub releases 更新到最新版本
help        顯示此訊息或給定子命令的說明
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
- `~/.copilot/history-session-state/*.json`（Copilot）
- `~/.gemini/tmp/<project_hash>/chats/*.json`（Gemini）

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

---

## 🔄 Update 命令

**自動保持安裝版本為最新。**

update 命令適用於**所有安裝方式**（npm/pip/cargo/manual），直接從 GitHub releases 下載並替換二進位檔。

### 基本用法

```bash
# 檢查更新
vct update --check

# 互動式更新（會詢問確認）
vct update

# 強制更新 - 總是下載最新版本（即使已是最新版本）
vct update --force
```

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

---

## 🐳 Docker 支援

```bash
# 建置映像
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# 使用您的會話執行
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.copilot:/root/.copilot \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```
