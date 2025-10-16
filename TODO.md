## 專案經過了多輪跌代 目前已經有記憶體占用過高的問題產生

我希望你專注於優化記憶體占用, 讓記憶體占用降到最低
目前不知道為何 感覺 `TUI` 剛啟動時 記憶體佔用大約只有 1x MB 屬於合理值
當計算分析結束後 記憶體佔用會飆升到接近 200 mb
我希望你優化 LRU 緩存機制

1. 需要釋放記憶體的邏輯
2. `usage` / `analysis` 改成每十秒掃一次
3. usage 分析完畢以後 `TUI` 只需要保留加總完的 `conversationUsage` 即可, 其餘全部數據都可以忽略並釋放緩存 避免 TUI 佔用內存
4. analysis 同理, 分析完畢以後其實只需要保留 `totalEditCharacters`, `totalEditLines`, `totalReadCharacters`, `totalReadLines` 這種必須的資訊即可 其餘皆可刪除釋放緩存 避免 TUI 佔用內存

## 請查看 src 裡面的所有代碼 我希望你幫我將他透過 rust workspace 將功能劃分成五大主要核心模組

```
analysis  Analyze JSONL conversation files (single file or all sessions)
usage     Display token usage statistics
version   Display version information
update    Update to the latest version from GitHub releases
help      Print this message or the help of the given subcommand(s)
```

最後再透過 cli 當作主要入口 (跟根目錄下的 `cli` 有本質不同, 我現在說的是可能會放在 `./vct-rs/cli` 底下)
tui 負責終端使用者介面
common 共享工具庫 (有可能 help 可以被定義在這 這點我不確定是要放在 tui 還是 common)

我想法是將 `./src` 整個改成 `./vct-rs`, 裡面則是各自功能的模組
例如 `./vct-rs/analysis`, `./vct-rs/usage`, `./vct-rs/version`, `./vct-rs/update`, `./vct-rs/help`, `./vct-rs/cli`, `./vct-rs/tui`, `./vct-rs/common`...
