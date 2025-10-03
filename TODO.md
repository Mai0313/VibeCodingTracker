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

## 更新 Usage Table 顯示內容

這裡有所有模型的價格 `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json`
他的格式大概是這樣的
```json
{
    "gpt-5": {
        "cache_read_input_token_cost": 1.25e-07,
        "cache_read_input_token_cost_flex": 6.25e-08,
        "cache_read_input_token_cost_priority": 2.5e-07,
        "input_cost_per_token": 1.25e-06,
        "input_cost_per_token_flex": 6.25e-07,
        "input_cost_per_token_priority": 2.5e-06,
        "litellm_provider": "openai",
        "max_input_tokens": 272000,
        "max_output_tokens": 128000,
        "max_tokens": 128000,
        "mode": "chat",
        "output_cost_per_token": 1e-05,
        "output_cost_per_token_flex": 5e-06,
        "output_cost_per_token_priority": 2e-05,
        "supported_endpoints": [
            "/v1/chat/completions",
            "/v1/batch",
            "/v1/responses"
        ],
        "supported_modalities": [
            "text",
            "image"
        ],
        "supported_output_modalities": [
            "text"
        ],
        "supports_function_calling": true,
        "supports_native_streaming": true,
        "supports_parallel_function_calling": true,
        "supports_pdf_input": true,
        "supports_prompt_caching": true,
        "supports_reasoning": true,
        "supports_response_schema": true,
        "supports_system_messages": true,
        "supports_tool_choice": true,
        "supports_vision": true
    }
}
```
我希望計算usage 的時候 可以先從這裡取得價格, 最後做計算
而不是單純顯示 token 使用量

我希望欄位有 `Date`, `Model`, `Input`, `Output`, `Cache Read`, `Cache Creation`, `Total Tokens` 和 `Cost (USD)`
