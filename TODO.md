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
