## 新增額外分析功能

目前這些功能已完成並且可以順利運作

```bash
# Claude Code
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation.jsonl
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation.jsonl --output examples/analysis_result.json
# Codex
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl --output examples/analysis_result_oai.json
# Gemini
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_gemini.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_gemini.json --output examples/analysis_result_gemini.json
```

我希望新增一個新功能是 `./target/debug/vibe_coding_tracker analysis`
這個功能會將 `~/.codex/sessions` 和 `~/.claude/projects` 裡面的所有 `jsonl` 全部進行分析
最後組成一個 list dict 的 json文件

我可以透過指定 `--output` 去輸出到某一份 `json` 中

但是當我沒指定 `--output` 的時候, 就需要透過 `ratatui` 製作一個 interative table
但是上面我想顯示的是
`Date`, `Model`, `Edit Lines`, `Read Lines`, `Write Lines`, `Bash`, `Edit`, `Read`, `TodoWrite`, `Write`
這些資訊在 parse 完畢以後都會出現, 只是需要分模型與日期進行加總

此次任務已完成 請更新 README.md README.zh-CN.md README.zh-TW.md 並將所有輸出寫成範例包含在裡面

## 查看版本資訊

```bash
./target/debug/vibe_coding_tracker version
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
./target/debug/vibe_coding_tracker version --json
# {
#     "Version": "5.0.6",
#     "Rust Version": "1.28.2",
#     "Cargo Version": "1.89.0"
# }
./target/debug/vibe_coding_tracker version --text
# Version: 5.0.6
# Rust Version: 1.28.2
# Cargo Version: 1.89.0
```

## 查看使用狀況

```bash
./target/debug/vibe_coding_tracker update
# 先不用完成 忽略
./target/debug/vibe_coding_tracker usage
# 目前功能正確 但請透過 `Ratatui` 美化輸出的 Table
./target/debug/vibe_coding_tracker usage --json
# 目前功能正確 忽略
./target/debug/vibe_coding_tracker help
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

這個功能 `--json` 模式下也要支援, 並且 usage 請幫我新增一個 `--text` 功能

## 新增 interactive table

幫我把 `./target/debug/vibe_coding_tracker usage` 的輸出改成 實時更新的 interactive table, 每五秒更新一次
可以用 Ratatui 這個 library
然後將當前 `./target/debug/vibe_coding_tracker usage` 顯示出來的 table 改放到 `./target/debug/vibe_coding_tracker usage --table`

`--text` 功能請幫我將它改成單純的 `Date > model name: cost` 這樣的格式
另外 table 的 cost 取小數點兩位 四捨五入即可, `--json`, `--text` 則是按照現在的狀態 不要進行四捨五入

## 更新版本取得邏輯

我有點不確定我目前 `version` 功能中的 的 Version 這一欄位是如何取得的, 因為他每一次都是顯示 `0.1.0`
例如

```bash
❯ ./target/debug/vibe_coding_tracker version
🚀 Vibe Coding Tracker

┌───────────────┬────────┐
│ Version       ┆ 0.1.0  │
├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌┤
│ Rust Version  ┆ 1.89.0 │
├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌┤
│ Cargo Version ┆ 1.89.0 │
└───────────────┴────────┘
```

看起來 `Version` 應該是透過 `Cargo.toml` 裡面直接取得
經過我自己的追蹤 我發現版本獲取是從 `src/lib.rs` 裡面的 `CARGO_PKG_VERSION` 取得的
但我不知道如何做到像是 dunamai 那種工具一樣會有 `0.1.6-dirty-...` 之類的版本號
看有沒有辦法透過編譯時注入的方式來完成

## 簡化 `usage` 和 `analysis` 的 parsing

請幫我檢查一下目前 `usage`, `analysis`, 和透過 `--path` 選擇文件 的 parsing 邏輯是否屬於同一套
我覺得三種功能應該使用同一套流程, 都是先從 examples/test_conversation_gemini.json examples/test_conversation_oai.jsonl examples/test_conversation.jsonl 這種文件中先 parse 完畢以後, 再去取得 並 依照對應方式來顯示結果

## 幫我新增 `--all` 功能到 `analysis`

當我透過 `analysis --all` 呼叫時, 腳本自動從 gemini / codex / claude code 資料夾中parse所有數據並印出
假設我有給 `analysis --all --output`, 則要存起來
印出來與存入的資料都是變成 gemini / codex / claude code 當作 key, 對應的 value 則是各自的 `list[dict]`

## 幫我更新一下 usage 的計算

token 價格的資訊需要修改一下讓他更精確 因為每一個模型的計算方式不同 所以計算時需要取不同的 key

Claude Code 的 Usage:

```json
"usage":{"input_tokens":12,"cache_creation_input_tokens":1541,"cache_read_input_tokens":15247,"cache_creation":{"ephemeral_5m_input_tokens":1541,"ephemeral_1h_input_tokens":0},"output_tokens":2,"service_tier":"standard"}
```
需要從 `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json` 取得以下資訊
"cache_creation_input_token_cost": 3.75e-06
"cache_read_input_token_cost": 3e-07
"input_cost_per_token": 3e-06
"output_cost_per_token": 1.5e-05
# 如果 token 超出 200K (請注意 這裡不是總和 而是當次):
"cache_creation_input_token_cost_above_200k_tokens": 7.5e-06
"cache_read_input_token_cost_above_200k_tokens": 6e-07
"input_cost_per_token_above_200k_tokens": 6e-06
"output_cost_per_token_above_200k_tokens": 2.25e-05

Codex 的 Usage:

```json
"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":58619,"cached_input_tokens":19840,"output_tokens":6589,"reasoning_output_tokens":5824,"total_tokens":65208},"last_token_usage":{"input_tokens":13061,"cached_input_tokens":5248,"output_tokens":1444,"reasoning_output_tokens":1280,"total_tokens":14505},"model_context_window":null}}
```
需要從 `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json` 取得以下資訊
"cache_read_input_token_cost": 1.25e-07
"input_cost_per_token": 1.25e-06
"output_cost_per_token": 1e-05

Gemini 的 Usage:

```json
"tokens": {
  "input": 7228,
  "output": 94,
  "cached": 0,
  "thoughts": 2659,
  "tool": 0,
  "total": 9981
},
```
需要從 `https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json` 取得以下資訊
"input_cost_per_token": 1.25e-06
"output_cost_per_token": 1e-05
"cache_read_input_token_cost": 3.125e-07
# 如果 token 超出 200K (請注意 這裡不是總和 而是當次):
"input_cost_per_token_above_200k_tokens": 2.5e-06
"output_cost_per_token_above_200k_tokens": 1.5e-05

我覺得邏輯可以較為簡單得做成
先假設每一個模型都會有 `above_200k`, 如果沒有的話就透過原價當作預設直來計算

## 請檢查一下 .github/workflows/build_release.yml 這裡的流程 我希望做以下改動

發佈到npm的時候 改成直接將檔案下載下來一起放到 npm
我希望可以發佈三種名稱的包到 `https://registry.npmjs.org`
- `@mai0313/vibe-coding-tracker` (新增 scope)
- `@mai0313/vct` (新增 scope + short name)
- `vibe-coding-tracker` (已存在)

取得所有安裝包的方式可以透過 `gh release download` 指令來完成 可能會更好一點

另外我不確定 `update` 功能的邏輯是否需要修改 請順便檢查 我認為不用 因為我記得我是 `inplace` 的方式去更新
但我確定經過這次改動以後 `./cli` 裡面可以大幅簡化

## 請幫我檢查所有代碼 查看一下有沒有地方是需要優化或冗餘代碼

這個專案經過了多輪跌代 我擔心會有一些影響效能的邏輯出現 或 重複邏輯出現 或 為了向後兼容產生的代碼 這些都請你幫我重構
你不需要考慮像後兼容, 可以大幅度改動 只要功能能正常運作即可
請使用繁體中文
