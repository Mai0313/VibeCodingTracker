# TODO: Codex Usage

請參考 `parser.go`, 將他翻譯成 rust 的專案
目前我傾向支援 CLI功能, 未來會支援 TUI
以下是 CLI 狀態下需要支援的所有功能 請完整幫我設計並歸類
TUI的部分可以使用 https://github.com/vadimdemedes/ink 來完成
但TUI的部分先不用設計 先專注於 CLI 的功能就好

```bash
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl --output examples/claude_code_log.json
./target/debug/codex_usage analysis --path examples/test_conversation_oai.jsonl
./target/debug/codex_usage analysis --path examples/test_conversation_oai.jsonl --output examples/claude_code_log_oai.json
./target/debug/codex_usage usage
./target/debug/codex_usage usage --json
```
