<center>

# CodexUsage（支援 Codex 與 Claude Code）

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/CodexUsage/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/CodexUsage/pulls)

</center>

以 Rust 實作的遙測日誌解析器：讀取 Codex 與 Claude Code 產生的 JSONL 事件，產出彙總的 CodeAnalysis JSON，並可選擇輸出除錯檔案。

其他語言: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## 功能

本專案是將原本的 Go 語言實作 (`parser.go`) 完整翻譯成 Rust 的專案，主要功能是解析和分析 Claude Code 和 Codex 的 JSONL 日誌檔案。

- 解析來自 Claude Code 與 Codex 的 JSONL 事件
- 自動判別來源並正規化路徑，避免重複統計
- 彙總 Read/Write/Edit/Command、工具呼叫次數與對話 token 使用量
- 輸出單筆 CodeAnalysis（JSON）與可選的除錯檔案

專注於資料萃取、統計與檔案處理；結果傳輸（如 SendAnalysisData）不在本專案範圍。

## 特色功能

1. **自動偵測**: 自動識別 Claude Code 或 Codex 日誌格式
2. **完整統計**: 包含檔案操作、工具呼叫、token 使用量等詳細統計
3. **美觀輸出**: 使用量統計提供格式化的表格顯示，附千位分隔符
4. **健全錯誤處理**: 使用 Rust 的型別系統提供可靠的錯誤管理
5. **效能優化**: Release 建置包含 LTO 和符號剝離最佳化

## 快速開始

前置：Rust 工具鏈（rustup），Docker 可選

```bash
# 建置專案
make fmt            # 格式化 + clippy
make test           # 測試（詳細輸出）
make build          # 建置
make release        # 發布建置（release）
make package        # 產生 .crate 套件
```

## CLI 使用方式

### 分析命令

分析 JSONL 對話檔案並取得詳細統計：

```bash
# 分析並輸出到標準輸出
codex_usage analysis --path examples/test_conversation.jsonl

# 分析並儲存到檔案
codex_usage analysis --path examples/test_conversation.jsonl --output result.json

# 分析 Codex 日誌
codex_usage analysis --path examples/test_conversation_oai.jsonl
```

### 使用量命令

顯示 Claude Code 和 Codex 會話的 token 使用統計：

```bash
# 以表格格式顯示使用量
codex_usage usage

# 以 JSON 格式顯示使用量
codex_usage usage --json
```

### 版本命令

顯示版本資訊：

```bash
codex_usage version
```

## 專案結構

```
codex_usage/
├── src/
│   ├── lib.rs              # 函式庫主檔
│   ├── main.rs             # CLI 入口點
│   ├── cli.rs              # CLI 參數解析
│   ├── models/             # 數據模型
│   │   ├── mod.rs
│   │   ├── analysis.rs     # 分析數據結構
│   │   ├── usage.rs        # 使用量數據結構
│   │   ├── claude.rs       # Claude Code 日誌模型
│   │   └── codex.rs        # Codex 日誌模型
│   ├── analysis/           # 分析功能
│   │   ├── mod.rs
│   │   ├── analyzer.rs     # 主分析器
│   │   ├── claude_analyzer.rs  # Claude Code 分析器
│   │   ├── codex_analyzer.rs   # Codex 分析器
│   │   └── detector.rs     # 擴展類型偵測
│   ├── usage/              # 使用量統計
│   │   ├── mod.rs
│   │   ├── calculator.rs   # 使用量計算
│   │   └── display.rs      # 使用量顯示格式化
│   └── utils/              # 工具函數
│       ├── mod.rs
│       ├── paths.rs        # 路徑處理
│       ├── time.rs         # 時間解析
│       ├── file.rs         # 檔案 I/O
│       └── git.rs          # Git 操作
├── examples/               # 範例 JSONL 檔案
├── tests/                  # 整合測試
└── parser.go              # 原始 Go 實作（參考用）
```

## 主要依賴

- **CLI**: clap (v4.5) - 命令列參數解析
- **序列化**: serde, serde_json - JSON 處理
- **錯誤處理**: anyhow, thiserror - 健全的錯誤管理
- **時間**: chrono - 時間戳解析
- **檔案系統**: walkdir, home - 目錄遍歷和路徑解析
- **正則表達式**: regex - 日誌解析中的模式匹配
- **日誌**: log, env_logger - 除錯輸出

## Go 到 Rust 的對應

| Go 功能 | Rust 實作 | 說明 |
|---------|-----------|------|
| `analyzeConversations` | `analysis::claude_analyzer::analyze_claude_conversations` | Claude Code 分析 |
| `analyzeCodexConversations` | `analysis::codex_analyzer::analyze_codex_conversations` | Codex 分析 |
| `CalculateUsageFromJSONL` | `usage::calculator::calculate_usage_from_jsonl` | 單檔使用量計算 |
| `GetUsageFromDirectories` | `usage::calculator::get_usage_from_directories` | 目錄使用量統計 |
| `ReadJSONL` | `utils::file::read_jsonl` | JSONL 檔案讀取 |
| `parseISOTimestamp` | `utils::time::parse_iso_timestamp` | 時間戳解析 |
| `getGitRemoteOriginURL` | `utils::git::get_git_remote_url` | Git 遠端 URL 提取 |

## Docker

```bash
docker build -f docker/Dockerfile --target prod -t ghcr.io/<owner>/<repo>:latest .
docker run --rm ghcr.io/<owner>/<repo>:latest
```

二進位映像標籤：
```bash
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .
docker run --rm codex_usage:latest
```

## 命名

- crate/二進位：`codex_usage`
- 儲存庫連結：`https://github.com/Mai0313/codex_usage`
- CI 已固定使用 `codex_usage` 作為二進位名稱，避免與 repo 名稱綁定

## 授權

MIT — 見 `LICENSE`。
