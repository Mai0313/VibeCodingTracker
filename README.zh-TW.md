<center>

# Vibe Coding Tracker — AI 程式設計助手使用量追蹤器

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/VibeCodingTracker/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</center>

**即時追蹤您的 AI 程式設計成本。** Vibe Coding Tracker 是一個強大的 CLI 工具，幫助您監控和分析 Claude Code 和 Codex 的使用情況，提供詳細的成本分解、token 統計和程式碼操作洞察。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

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
自動偵測並處理 Claude Code 和 Codex 的日誌。無需設定——只需執行和分析。

### 🎨 豐富的洞察
- 按模型和日期的 token 使用量
- 按快取類型的成本分解
- 檔案操作追蹤
- 命令執行歷史
- Git 儲存庫資訊

---

## ✨ 核心特性

| 特性 | 描述 |
|---------|-------------|
| 🤖 **自動偵測** | 智慧識別 Claude Code 或 Codex 日誌 |
| 💵 **智慧定價** | 模糊模型匹配 + 每日快取以提高速度 |
| 🎨 **4 種顯示模式** | 互動式、表格、文字和 JSON 輸出 |
| 📈 **全面統計** | Token、成本、檔案操作和工具呼叫 |
| ⚡ **高效能** | 使用 Rust 建置，速度快且可靠 |
| 🔄 **即時更新** | 儀表板每秒重新整理 |
| 💾 **高效快取** | 智慧的每日快取減少 API 呼叫 |

---

## 🚀 快速開始

### 安裝

**前置條件**：[Rust 工具鏈](https://rustup.rs/)（1.70+）

```bash
# 複製和建置
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker
cargo build --release

# 二進位檔案位置：
# - ./target/release/vibe_coding_tracker (完整名稱)
# - ./target/release/vct (短別名)
```

### 首次執行

```bash
# 使用互動式儀表板檢視使用量（使用短別名）
./target/release/vct usage

# 或使用完整名稱
./target/release/vibe_coding_tracker usage

# 分析特定對話
./target/release/vct analysis --path ~/.claude/projects/session.jsonl
```

> 💡 **提示**：使用 `vct` 作為 `vibe_coding_tracker` 的短別名，節省輸入時間！

---

## 📖 命令指南

### 🔍 快速參考

```bash
vibe_coding_tracker <命令> [選項]

命令：
  usage       顯示 token 使用量和成本（預設：互動式）
  analysis    分析對話檔案並匯出資料
  version     顯示版本資訊
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
│ 💰 總成本：$5.30  |  🔢 總 Token：819,900  |  📅 條目：4  |  🧠 記憶體：12.5 MB                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**特性**：
- ✨ 每秒自動重新整理
- 🎯 突顯今日條目
- 🔄 顯示最近更新的列
- 💾 顯示記憶體使用量
- 📊 彙總統計

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
║            ║ 總計                 ║ 105,830    ║ 28,570     ║ 560,500    ║ 105,000      ║ 799,900      ║ $5.05      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝
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
      "cost_usd": 2.1542304567890123
    }
  ]
}
```

### 🔍 輸出對比

| 特性 | 互動式 | 表格 | 文字 | JSON |
|---------|-------------|-------|------|------|
| **最適合** | 監控 | 報表 | 指令碼 | 整合 |
| **成本格式** | $2.15 | $2.15 | $2.154230 | 2.1542304567890123 |
| **更新** | 即時 | 靜態 | 靜態 | 靜態 |
| **顏色** | ✅ | ✅ | ❌ | ❌ |
| **可解析** | ❌ | ❌ | ✅ | ✅ |

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

# 批次：將彙總結果儲存為 JSON
vct analysis --output batch_report.json
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
│ 📝 總行數：25,848  |  🔧 總工具：428  |  📅 條目：3  |  🧠 記憶體：8.2 MB                                      │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
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

## 💡 智慧定價系統

### 運作原理

1. **自動更新**：每天從 [LiteLLM](https://github.com/BerriAI/litellm) 取得定價
2. **智慧快取**：在 `~/.vibe-coding-tracker/` 中儲存定價 24 小時
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
  vibe_coding_tracker:latest usage
```

---

## 🔍 疑難排解

### 定價資料未載入

```bash
# 檢查快取
ls -la ~/.vibe-coding-tracker/

# 強制重新整理
rm -rf ~/.vibe-coding-tracker/
vct usage

# 除錯模式
RUST_LOG=debug vct usage
```

### 沒有顯示使用資料

```bash
# 驗證會話目錄
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/

# 統計 JSONL 檔案
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
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

| 操作 | 時間 |
|-----------|------|
| 解析 10MB JSONL | ~320ms |
| 分析 1000 個事件 | ~45ms |
| 載入快取的定價 | ~2ms |
| 互動式重新整理 | ~30ms |

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
- Claude Code 和 Codex 團隊建立了出色的 AI 程式設計助手
- Rust 社群提供了優秀的工具

---

<center>

**省錢。追蹤使用量。更智慧地編寫程式。**

如果您覺得有用，請[⭐ Star 這個專案](https://github.com/Mai0313/VibeCodingTracker)！

使用 🦀 Rust 製作

</center>
