<center>

# Rust 專案模板

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/rust_template/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/rust_template/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/rust_template/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/rust_template/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/rust_template/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/rust_template/pulls)

</center>

🚀 幫助 Rust 開發者「快速建立新專案」的模板。內建 Cargo 佈局、Docker 與完整 CI/CD 流程。

點擊 [使用此模板](https://github.com/Mai0313/rust_template/generate) 後即可開始。

其他語言: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## ✨ 重點特色

- 現代 Cargo 結構（`src/lib.rs`、`src/main.rs`、`tests/`）
- clippy + rustfmt 品質把關
- GitHub Actions：測試、品質、打包、Docker 推送、發布草稿、Rust 自動標籤、祕密掃描、語義化 PR、每週依賴更新
- 多階段 Dockerfile，產出精簡執行映像

## 🚀 快速開始

前置：安裝 Rust（rustup）、可選 Docker

```bash
make fmt            # 格式化 + clippy
make test           # 測試（包含所有目標與詳細輸出）
make build          # 建置（release 模式）
make build-release  # 發布建置（release 模式）
make run            # 執行（release 模式）
make clean          # 清理建置產物與快取
make package        # 建立 crate 套件（允許 dirty）
make help           # 檢視可用目標
```

## 🐳 Docker

```bash
docker build -f docker/Dockerfile --target prod -t ghcr.io/<owner>/<repo>:latest .
docker run --rm ghcr.io/<owner>/<repo>:latest
```

或使用實際的二進位名稱：
```bash
docker build -f docker/Dockerfile --target prod -t rust_template:latest .
docker run --rm rust_template:latest
```

## 📦 打包發佈

```bash
make package        # 建立 crate 套件（允許 dirty）
# 或直接使用 cargo：
cargo package --locked --allow-dirty
# CARGO_REGISTRY_TOKEN=... cargo publish
```

CI 會在建立 `v*` 標籤時自動打包並上傳 `.crate` 產物。若需自動發佈到 crates.io，請在 `build_package.yml` 開啟發佈步驟並設定密鑰。

## 🧩 跨平台建置

目前模板預設不含本機跨編譯工具。若需在本機使用 cross 或 zig，請依環境安裝與設定。

GitHub Actions `build_release.yml` 會在建立 `v*` 標籤時針對多平台建置釋出二進位，並上傳至 GitHub Release。

目標平台（targets）：
- x86_64-unknown-linux-gnu、x86_64-unknown-linux-musl
- aarch64-unknown-linux-gnu、aarch64-unknown-linux-musl
- x86_64-apple-darwin、aarch64-apple-darwin
- x86_64-pc-windows-msvc、aarch64-pc-windows-msvc

資產命名（assets）：
- `<bin>-v<version>-<target>.tar.gz`（所有平台）
- `<bin>-v<version>-<target>.zip`（Windows 另附）

## 🔁 CI/CD

### 主要工作流程
- 測試（`test.yml`）：建置與測試，生成 LCOV 格式覆蓋率報告並上傳 artifact
- 品質（`code-quality-check.yml`）：rustfmt 檢查 + clippy（拒絕警告）
- 打包（`build_package.yml`）：標籤 `v*` 觸發打包，可選 crates.io 發佈
- 映像（`build_image.yml`）：在 `main/master` 與標籤 `v*` 推送至 GHCR
- 發佈建置（`build_release.yml`）：標籤 `v*` 時建置 Linux 釋出二進位並上傳

### 其他自動化功能
- 自動標籤（`auto_labeler.yml`）：根據分支名稱與檔案變更自動為 PR 添加標籤
- 程式碼掃描（`code_scan.yml`）：多層次安全性掃描（GitLeaks、Trufflehog 祕密掃描、CodeQL 程式碼分析、Trivy 漏洞掃描）
- 發佈草稿（`release_drafter.yml`）：自動生成 release notes
- 語義化 PR（`semantic-pull-request.yml`）：檢查 PR 標題格式
- Dependabot 每週依賴更新

## 🤝 貢獻

- 歡迎 Issue/PR
- PR 標題遵循 Conventional Commits
- 請保持格式化並通過 clippy 檢查

- 每次編輯完畢後，請執行 `cargo build` 來確認編譯是否成功
- 在送出 PR 前，請先本機執行：
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`

## 📄 授權

MIT — 詳見 `LICENSE`。


