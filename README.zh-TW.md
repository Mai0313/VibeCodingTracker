<center>

# CodexUsage（支援 Codex 與 Claude Code）

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](LICENSE)

</center>

以 Rust 實作的遙測日誌解析器：讀取 Codex 與 Claude Code 產生的 JSONL 事件，產出彙總的 CodeAnalysis JSON，並可選擇輸出除錯檔案。

其他語言: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## 功能

- 解析來自 Claude Code 與 Codex 的 JSONL 事件
- 自動判別來源並正規化路徑，避免重複統計
- 彙總 Read/Write/Edit/Command、工具呼叫次數與對話 token 使用量
- 輸出單筆 CodeAnalysis（JSON）與可選的除錯檔案

專注於資料萃取、統計與檔案處理；結果傳輸（如 SendAnalysisData）不在本專案範圍。

## 快速開始

前置：Rust 工具鏈（rustup），Docker 可選

```bash
make fmt            # 格式化 + clippy
make test           # 測試（詳細輸出）
make build          # 建置
make build-release  # 發布建置（release）
make run            # 執行 release 二進位
make package        # 產生 .crate 套件
```

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
- 儲存庫連結：`https://github.com/<owner>/codex-usage`
- CI 已固定使用 `codex_usage` 作為二進位名稱，避免與 repo 名稱綁定

## 授權

MIT — 見 `LICENSE`。
