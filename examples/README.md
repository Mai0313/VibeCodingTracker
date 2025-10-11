## 任務：將專案描述中的 "Vibe Coding Tracker" 改為 "Vibe Code Tracker" 並更新文檔

### 操作內容：

1. **讀取部分檔案**：查看 `Cargo.toml` 的前 15 行，了解專案基本資訊
2. **讀取整份文件**：完整讀取 `README.md`
3. **修改文件內容**：
   - 修改 `src/cli.rs` 第 4 行的註解：`Vibe Coding Tracker` → `Vibe Code Tracker`
   - 修改 `README.md` 中的專案名稱（如果有的話）
4. **新增文件**：創建一個簡單的 `CHANGELOG_DEMO.md` 記錄這次改動
5. **上網搜索**：搜索 "Rust CLI tool naming best practices" 了解命名慣例
6. **執行 bash 指令**：
   - `cargo check` 確認程式碼沒問題
   - `grep -n "Vibe Coding Tracker" src/cli.rs` 確認修改成功

### 改動範例

**修改前**（cli.rs 第 4 行）：
```rust
/// Vibe Coding Tracker - AI coding assistant usage analyzer
```

**修改後**：
```rust
/// Vibe Code Tracker - AI coding assistant usage analyzer
```

**新增的 CHANGELOG_DEMO.md**：
```markdown
# 專案名稱變更記錄

- 2025-10-11: 將 "Vibe Coding Tracker" 簡化為 "Vibe Code Tracker"
```
