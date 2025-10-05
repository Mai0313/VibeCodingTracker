## 新增額外分析功能

目前這些功能已完成並且可以順利運作
```bash
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation.jsonl
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation.jsonl --output examples/analysis_result.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl --output examples/analysis_result_oai.json
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

## 更新專案名稱

我想把專案名稱改成 Vibe Coding Tracker
但這個改動可能會涉及到很多名稱 例如 `codex-usage`, `vibe_coding_tracker`, `CodexUsage`, etc...
repo 連結未來會改為 `https://github.com/Mai0313/VibeCodingTracker`
專案縮寫是 `vct` 方便調用或紀錄

這個任務已經由其他助理完成 請幫我檢查是否有遺漏

## 幫我補齊測試覆蓋率

請透過 `cargo llvm-cov --workspace` 查看當前覆蓋率 並幫我完全所有代碼測試
這裡是目前狀態
```
Filename                        Regions    Missed Regions     Cover   Functions  Missed Functions  Executed       Lines      Missed Lines     Cover    Branches   Missed Branches     Cover
-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
analysis/analyzer.rs                 54                 6    88.89%           3                 0   100.00%          25                 1    96.00%           0                 0         -
analysis/batch_analyzer.rs          204                23    88.73%          17                 0   100.00%         135                11    91.85%           0                 0         -
analysis/claude_analyzer.rs         493                18    96.35%          29                 0   100.00%         296                17    94.26%           0                 0         -
analysis/codex_analyzer.rs          647                52    91.96%          19                 0   100.00%         465                34    92.69%           0                 0         -
analysis/detector.rs                 17                 0   100.00%           1                 0   100.00%          12                 0   100.00%           0                 0         -
analysis/display.rs                 679               679     0.00%           7                 7     0.00%         369               369     0.00%           0                 0         -
cli.rs                                3                 3     0.00%           1                 1     0.00%           3                 3     0.00%           0                 0         -
lib.rs                               49                 4    91.84%          11                 2    81.82%          47                 2    95.74%           0                 0         -
main.rs                             288               288     0.00%          10                10     0.00%         168               168     0.00%           0                 0         -
models/analysis.rs                    9                 0   100.00%           1                 0   100.00%           5                 0   100.00%           0                 0         -
pricing.rs                          268               155    42.16%          17                11    35.29%         190                90    52.63%           0                 0         -
usage/calculator.rs                 600                86    85.67%          38                 2    94.74%         338                55    83.73%           0                 0         -
usage/display.rs                    840               840     0.00%          19                19     0.00%         427               427     0.00%           0                 0         -
utils/file.rs                        69                 9    86.96%           7                 2    71.43%          31                 2    93.55%           0                 0         -
utils/git.rs                         34                 1    97.06%           1                 0   100.00%          18                 1    94.44%           0                 0         -
utils/paths.rs                       47                16    65.96%           6                 3    50.00%          36                10    72.22%           0                 0         -
utils/time.rs                        23                 3    86.96%           1                 0   100.00%          19                 1    94.74%           0                 0         -
-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
TOTAL                              4324              2183    49.51%         188                57    69.68%        2584              1191    53.91%           0                 0         -
```
