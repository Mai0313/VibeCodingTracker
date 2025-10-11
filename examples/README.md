## 任務：為 usage 命令新增 CSV 輸出格式

這個任務會為 `vct usage` 命令添加 CSV 格式輸出選項，讓使用者可以用 Excel 或其他工具進一步分析資料。

### 這個任務會涵蓋你要求的所有操作：

1. **讀取部分檔案**：查看現有 text.rs 的前幾十行，了解輸出格式的實作方式
2. **讀取整份文件**：完整讀取 usage/mod.rs 了解模組結構
3. **修改文件內容**：
   - 修改 `cli.rs` 添加 `--csv` 選項
   - 修改 `usage/mod.rs` 導出新的 CSV 函數
   - 修改 `main.rs` 處理 CSV 輸出邏輯
4. **新增文件**：創建 `src/display/usage/csv.rs` 實現 CSV 輸出功能
5. **上網搜索**：搜索 Rust 的 CSV 處理最佳實踐和推薦的 crate
6. **執行 bash 指令**：
   - `cargo add csv` 添加 CSV 依賴
   - `cargo build` 確認編譯成功
   - `cargo test` 執行測試

### 輸出範例

CSV 格式會是：

```csv
Date,Model,Input Tokens,Output Tokens,Cache Read,Cache Creation,Total Tokens,Cost (USD)
2025-10-01,claude-sonnet-4-20250514,45230,12450,230500,50000,338180,2.15
2025-10-02,gpt-4-turbo,15000,5000,0,0,20000,0.25
```
