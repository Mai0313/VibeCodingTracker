# Codex Usage TODO

以此計畫將提供的「Telemetry Parser 規格說明」落地為可維護、可測的實作（語言無關，優先以模組化與測試友好為目標）。傳輸（SendAnalysisData）不在本計畫範圍。

## 里程碑總覽
- [ ] M0 規格與樣本準備：確認輸入樣本、驗收準則、資料欄位對齊
- [ ] M1 資料模型：定義所有結構、序列化格式、欄位名稱與型別
- [ ] M2 來源偵測與輸入處理：JSONL 讀寫、來源自動判別、路徑解析
- [ ] M3 Claude 解析器：事件迭代、工具統計、讀寫編輯/命令細節
- [ ] M4 Codex 解析器：shell 呼叫解析、apply_patch/讀取推論、狀態聚合
- [ ] M5 聚合輸出：組裝 CodeAnalysisRecord/CodeAnalysis、gitRemote 解析
- [ ] M6 除錯輸出：來源專屬 log 目錄、原始/解析結果/回應檔案
- [ ] M7 測試與樣本：單元+整合+迴歸測試、fixtures、覆蓋率基準
- [ ] M8 文件與 CI：使用說明、格式/靜態檢查、測試在 CI 通過

---

## M0 規格與樣本準備
- [ ] 收斂最小驗收樣本：
  - [ ] Claude JSONL（含 tool_use、tool_result 各型態）
  - [ ] Codex JSONL（含 function_call/Output、token_count、apply_patch、sed/cat）
- [ ] 明確驗收標準（至少）：
  - [ ] 每個樣本可產生非空 CodeAnalysis 且符合預期計數（Read/Write/Edit/Bash、唯一檔案、行數/字元數、usage 聚合）
  - [ ] 缺失欄位/格式異常時不中斷（靜默略過）
  - [ ] 啟用除錯時輸出 parse.json 並含必要細節

## M1 資料模型（語言無關，命名對齊規格）
- [ ] CodeAnalysisDetailBase（path/lineCount/charCount/timestampMs）
- [ ] Write/Read/ApplyDiff/RunCommand 細節結構
- [ ] CodeAnalysisToolCalls（Read/Write/Edit/TodoWrite/Bash）
- [ ] ConversationUsage（model → usage；保留彈性 map）
- [ ] CodeAnalysisRecord（唯一檔案數、總計、細節陣列、工具統計、usage、taskID、cwd、gitRemote）
- [ ] CodeAnalysis（user、extension、machineID、insightsVersion、records[]）
- [ ] ClaudeCodeLog / CodexLog（反序列化用資料模型）
- [ ] Codex 中介型別：codexAnalysisState / codexShellCall / codexShellOutput / codexPatch

## M2 來源偵測與輸入處理
- [ ] detectExtensionType：以是否存在 parentUuid 判斷 Claude/Codex
- [ ] ProcessInput：處理 CLI/外部附加資訊，解析 JSONL 路徑與除錯原始資料
- [ ] AnalyzeJSONLFile：流式讀取 JSONL，每行轉 map，再轉對應 Log 結構
- [ ] 路徑解析/正規化：normalizePath 將路徑統一為絕對路徑

## M3 Claude 解析器（analyzeClaudeConversations）
- [ ] 初次事件：記錄 cwd(folderPath)、taskID(sessionID)，維護最後 timestamp
- [ ] assistant + message：
  - [ ] 解析 model/usage，processClaudeUsageData 累計（input/output/cache）
  - [ ] 巡覽 content，累計工具使用次數；Bash 記錄 command/description 至 runDetails
- [ ] ToolUseResult：
  - [ ] type == text → 視為檔案讀取（行數/字元、唯一檔案、總計）
  - [ ] type == create → 視為檔案寫入（保存完整內容）
  - [ ] 同時有 oldString/newString → 視為檔案編輯（保存前後片段）
- [ ] 解析結束：讀取 .git/config 取得 origin URL

## M4 Codex 解析器（analyzeCodexConversations）
- [ ] 初始化：codexAnalysisState、ConversationUsage、shellCalls 對照表
- [ ] 事件處理：
  - [ ] session_meta/turn_context → 補齊 cwd、taskID、gitRemote、當前 model
  - [ ] event_msg + payload.type == token_count → processCodexUsageData
  - [ ] response_item：
    - [ ] function_call name==shell → 暫存命令與呼叫時間戳
    - [ ] function_call_output → 找對應呼叫並交由 state.handleShellCall；處理後移除
- [ ] state.handleShellCall：
  - [ ] apply_patch/applypatch：parseApplyPatchScript → 逐段 handlePatch（Add/Delete/Update）
  - [ ] sed -n pattern → 視為檔案讀取（計算行/字元）
  - [ ] cat 顯示檔案（支援裁切 --- 之後內容）→ 讀取事件
  - [ ] 其他 → recordRunCommand + 工具 Bash 計數
- [ ] handlePatch 規則：
  - [ ] add → 新檔寫入（writeDetails + 總計）
  - [ ] delete → 僅當刪除內容非空記錄為編輯（OldString）
  - [ ] update → 依舊內容是否為空分流至寫入或編輯
  - [ ] 一律更新唯一檔案集合與各類總計
- [ ] 結束前若無 gitRemote → 再讀取 .git/config

## M5 聚合與輸出
- [ ] 由 state/累計資料收斂為單筆 CodeAnalysisRecord
- [ ] 組合 CodeAnalysis（config.Default 依來源帶入 user/extension/machineID/insightsVersion）
- [ ] 支援序列化為 JSON 並輸出

### 擴充設定整合（依規格補強）
- [ ] detectExtensionType：以事件是否含 `parentUuid` 區分 Claude/Codex
- [ ] config.Default：依來源載入預設 `user`、`extension`、`machineID`、`insightsVersion`，最後填入 `CodeAnalysis`
- [ ] 若最終仍缺 `gitRemote`，在收斂前再次嘗試讀取 `.git/config`

## Usage 統計處理（依來源分流）
- [ ] Claude：`processClaudeUsageData`
  - [ ] 轉換 usage map → `ClaudeUsage` 結構
  - [ ] 累加 `input_tokens`、`output_tokens`、快取相關欄位，保留 `service_tier`
  - [ ] 針對 `assistant.message.content` 內的 `tool_use` 項目累計工具使用次數
- [ ] Codex：`processCodexUsageData`
  - [ ] 從 `event_msg.payload.info` 解析 token 統計
  - [ ] 寫入 `CodexUsage`，區分 `total_token_usage` 與 `last_token_usage`
  - [ ] 在 `response_item` 偵測 `function_call name==shell` 時更新 `Bash` 計數
- [ ] ConversationUsage：以模型名稱為 key 保存各自來源特有欄位（保持彈性 map 或具型別結構）

## M6 除錯輸出（LogEnabled=true）
- [ ] 決定 log 目錄（Claude/Codex 各自目錄）
- [ ] 保存：原始事件 JSON、歷史紀錄、會話路徑、parse.json（分析結果）
- [ ] 若有外部回應則保存 response.json

---

## RunAnalysis 執行流程（自動化管線）
- [ ] 輸入處理：`ProcessInput` 解析 CLI 參數與 Codex 附加資訊，回傳實際 JSONL 路徑與除錯原始資料
- [ ] 資料分析：`AnalyzeJSONLFile` 讀檔後委派至 `analyzeRecordSet`，再依來源呼叫 Claude/Codex 解析
- [ ] 除錯紀錄（LogEnabled）：
  - [ ] 透過 `paths.ResolvePaths` 決定 Claude/Codex 專屬 log 目錄
  - [ ] 保存原始事件 JSON、歷史紀錄、會話路徑等輔助資訊
  - [ ] 寫入 `parse.json`（分析結果）
- [ ] 輸出檔案：若指定 `OutputPath`，呼叫 `saveAnalysisLog` 將結果寫入指定位置
- [ ] 分析結果回饋：
  - [ ] 指定 `InputPath` 時，印出標準輸出
  - [ ] 互動模式交由外部傳輸元件（傳輸細節不在範圍）
- [ ] 回應除錯：若啟用除錯且有回應，儲存至 `response.json`
- [ ] 程序結束：
  - [ ] 正常 → `os.Exit(0)`
  - [ ] 分析結果為空 → `os.Exit(1)` 提早結束

## M7 測試與樣本
- [ ] 單元測試（建議）：
  - [ ] parseApplyPatchScript / extractPatchStrings
  - [ ] extractSedFilePath / extractCatRead
  - [ ] countLines / parseISOTimestamp
  - [ ] convertMapToStruct / normalizePath
  - [ ] getGitRemoteOriginURL
  - [ ] processClaudeUsageData / processCodexUsageData
- [ ] 整合測試：
  - [ ] Claude 全流程：輸入 JSONL → 輸出 CodeAnalysis 符合期望
  - [ ] Codex 全流程：涵蓋 shell、apply_patch、sed/cat 各情境
- [ ] 測試資料夾規劃：
  - [ ] tests/fixtures/claude/*.jsonl
  - [ ] tests/fixtures/codex/*.jsonl
  - [ ] tests/fixtures/patches/*.txt
- [ ] 覆蓋率門檻（可選）：語言工具支援下設定最低門檻

## M8 文件與 CI
- [ ] README 區段：
  - [ ] 目的/輸入輸出/限制
  - [ ] CLI 使用方式與參數（AnalysisParams）
  - [ ] 除錯輸出位置與檔案說明
- [ ] CI：格式化/靜態分析/測試全通過（以專案工具鏈為準）

## 邊界情境與防護（落實規格）
- [ ] 缺少有效路徑或空內容 → 略過細節紀錄
- [ ] JSON/Arguments 解析失敗 → 靜默略過不中斷
- [ ] .git/config 不存在 → gitRemote 空字串
- [ ] ProcessInput/AnalyzeJSONLFile 錯誤 → 提早結束並回報
- [ ] shellCalls 以 callID 對應，僅在收到對應輸出後評估
- [ ] 未識別的 shell 腳本 → 保守策略：記錄命令、不假設檔案操作

## 實作指引與檔案結構建議（參考）
- [ ] 模組化切分：
  - [ ] input/params：ProcessInput, 路徑處理
  - [ ] models：所有資料結構與序列化（CodeAnalysis* / ClaudeCodeLog / CodexLog）
  - [ ] analyze/claude：analyzeClaudeConversations + processClaudeUsageData
  - [ ] analyze/codex：analyzeCodexConversations + state/handleShellCall
  - [ ] patch：parseApplyPatchScript / extractPatchStrings / handlePatch
  - [ ] shell_read：extractSedFilePath / extractCatRead
  - [ ] utils：countLines / parseISOTimestamp / convertMapToStruct / normalizePath / getGitRemoteOriginURL
  - [ ] run：AnalyzeJSONLFile / analyzeRecordSet / saveAnalysisLog
- [ ] 對外 API：RunAnalysis(params: AnalysisParams) → CodeAnalysis

## 驗收清單（高層）
- [ ] 兩組樣本（Claude / Codex）均可產生正確的 CodeAnalysis 統計
- [ ] 工具使用次數與對話 usage 累計符合原始事件
- [ ] 讀/寫/編輯/命令細節有對應事件可追
- [ ] 啟用除錯能產生完整輔助檔案（含 parse.json）
- [ ] 無效或未知輸入不會中斷程式

## 後續優化（非必要）
- [ ] 將 state 與 patch 解析抽為純函式，便於跨語言單元測試
- [ ] ConversationUsage 維持彈性（map + 型別標記），或提供轉換器
- [ ] 增加性能剖析樣本（大檔 JSONL、長對話）
- [ ] 以特性旗標分離 CLI/Library（視語言而定）
