# TODO: Translate from Golang to Rust

請參考以下檔案 並幫我原封不動整理到 `./parser_example.go`, 他必須是可以讀立運行
- `core/telemetry/input.go`
- `core/telemetry/parser.go`
- `core/telemetry/usage.go`

但請無視發送 analysis 資訊到 API 的部分
所有 commands / subcommands 請完整保留, 你可以參考 `./cmd/coding-cli-helper/main.go`

# TODO: Codex Usage

請參考 `./parser_example.go` 並透過 Crossterm 和 Ratatui 幫我完成以下功能

## 分析某個 conversation (此功能已完成)

```bash
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl --output examples/analysis_claude_code.json
./target/debug/codex_usage analysis --path examples/test_conversation_oai.jsonl
./target/debug/codex_usage analysis --path examples/test_conversation_oai.jsonl --output examples/analysis_codex.json
```

## 查看版本資訊
```bash
./target/debug/codex_usage version
# 🚀 Coding CLI Helper
#
# ╭────────────────────────────────────╮
# │                                    │
# │  Version:    5.0.6                 │
# │  Rust Version: 1.28.2              │
# │  Cargo Version: 1.89.0             │
# │                                    │
# ╰────────────────────────────────────╯
#
./target/debug/codex_usage version --json
# {
#     "Version": "5.0.6",
#     "Rust Version": "1.28.2",
#     "Cargo Version": "1.89.0"
# }
./target/debug/codex_usage version --text
# Version: 5.0.6
# Rust Version: 1.28.2
# Cargo Version: 1.89.0
```

## 查看使用狀況
```bash
./target/debug/codex_usage update
# 先不用完成 忽略
./target/debug/codex_usage usage
# 目前功能正確 但請透過 `Ratatui` 美化輸出的 Table
./target/debug/codex_usage usage --json
# 目前功能正確 忽略
./target/debug/codex_usage help
# 目前功能正確 忽略
```
