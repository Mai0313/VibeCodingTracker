## 專案經過了多輪跌代 可能會產生

- 為了向後兼容產生的代碼
- 記憶體占用過高

我希望你專注於優化記憶體占用, 讓記憶體占用降到最低
目前不知道為何 感覺軟體剛啟動時 記憶體佔用大約只有 1xMB 屬於合理值
當計算分析結束後 記憶體佔用會飆升到 1xxMB 這就不合理了
我希望你把 LRU cache, 和所有緩存相關的全部刪除
現代電腦做這些計算其實根本不消耗什麼資源, 可以把 `usage` / `analysis` 改成每十秒掃一次

## 新增 `copilot cli` 支援

這裡是 `copilot cli` 的範例 log 文件 `examples/test_conversation_copilot.json`
請注意 他是 `json` 格式, 我希望他可以支援 `analysis` 和 `usage` 的功能
你主要需要新增的是一個 `parser`, 後續 `analysis` 和 `usage` 功能都會掉用相同的 `funcion` 來執行
你需要注意的地方可以從 `timeline` 這個 `key` 開始 (`chatMessages` 可以直接忽略), 裡面是整個 `conversation` 的細節流程 (`list[dict]`)
然後依照 `toolTitle` 和 `arguments` 的值去分類出類似於 `examples/analysis_result.json` 的資訊

- `toolTitle` 分為以下幾種:
    - str_replace_editor
        - 可能是 `readFileDetails`, `editFileDetails`, 或 `writeFileDetails` 請查看後續分類方式
    - bash
        - 只會是 `runCommandDetails`

- `readFileDetails`: `arguments` 的 `command` 為 `view`, `path` 就會是讀取的檔案, `view_range` 可能會沒有
    - 假設有 `view_range` 他會是由開始的 line 到 結束的 line 來記錄
    - 假設沒有 `view_range` 表示他是讀取整份文件
`editFileDetails`: `arguments` 裡面有 `command` 為 `str_replace` 的資訊 裡面也有提供 `path` `old_str` `new_str` 可以直接使用
`runCommandDetails`: `toolTitle` 為 `bash` 就是我們要記錄的 runCommand 了, 這邊的 command 內容就不用再細分了
`writeFileDetails`: `command` 為 `create`, 裡面有 `path` 和 `file_text` 可以直接使用

`usage` 的部分目前好像沒辦法知道 所以全部當做 0 就好
模型名稱暫時寫死 `copilot`, 未來等 copilot cli 更新再去看有沒有其他方式可以處理 因為目前沒有其他方式可以取得

## 提示自動更新

請幫我把提示更新的功能完整刪除 當作這功能從來沒出現過

## 請幫我檢查一下目前的 `update` 功能

請幫我確認 `update` 功能有使用對應的更新方式
因為這個專案支援透過 pip / cargo / npm 來安裝
假設使用者原本是使用 `pip` 安裝, 直接透過 `npm` 安裝肯定是錯誤的 因為會更新到錯誤的文件
我不確定是否可以取得當前運行的目錄, 我覺得應該可以透過這個資訊來判斷安裝來源

## 請幫我在我的 analysis TUI / usage TUI / update command 新增兩個小提示

1. 如果有問題 可以submit ticket
2. 如果喜歡 幫我點 star

這裡是我的 repo: https://github.com/Mai0313/VibeCodingTracker
我不確定超連結要如何顯示在TUI上

自動更新完成時 希望也可以提示用戶 如果喜歡的話給一個星星之類的
