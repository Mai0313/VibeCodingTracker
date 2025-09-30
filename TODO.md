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

## 這裡是關於兩個套件的使用想法 請參考這個想法 幫我設計 TUI
````markdown
## Crossterm 的作用

### 1. **底層終端控制**
Crossterm 負責與終端進行低階互動，提供跨平台的終端控制功能：

```rust
// 終端模式設定
use crossterm::event::EnableBracketedPaste;
use crossterm::event::EnableFocusChange;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;

pub fn set_modes() -> Result<()> {
    execute!(stdout(), EnableBracketedPaste)?;  // 啟用括號貼上
    execute!(stdout(), EnableFocusChange)?;    // 啟用焦點變更事件
    enable_raw_mode()?;                       // 啟用原始模式
    execute!(stdout(), EnterAlternateScreen)?; // 進入替代螢幕
    Ok(())
}
```

### 2. **事件處理**
處理鍵盤、滑鼠、貼上等終端事件：

```rust
pub enum TuiEvent {
    Key(KeyEvent),      // 鍵盤事件
    Paste(String),      // 貼上事件
    Draw,               // 重繪事件
}

// 事件串流處理
let mut crossterm_events = crossterm::event::EventStream::new();
match event {
    crossterm::event::Event::Key(key_event) => {
        // 處理鍵盤事件
    }
    crossterm::event::Event::Paste(paste_data) => {
        // 處理貼上事件
    }
}
```

### 3. **終端狀態管理**
- **原始模式**: 禁用行緩衝，直接讀取按鍵
- **替代螢幕**: 保存正常螢幕狀態，使用替代螢幕
- **游標控制**: 隱藏/顯示游標，移動游標位置
- **顏色和樣式**: 設定文字顏色、背景色、粗體等

### 4. **跨平台相容性**
- 統一 Windows、macOS、Linux 的終端 API
- 處理不同終端的差異
- 提供一致的終端控制介面

## Ratatui 的作用

### 1. **高階 UI 框架**
Ratatui 提供現代化的 TUI 組件和佈局系統：

```rust
// 佈局系統
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Widget, WidgetRef};

// 建立佈局
let layout = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(3),  // 標題區域
        Constraint::Min(0),     // 主要內容區域
        Constraint::Length(3),  // 底部輸入區域
    ])
    .split(area);
```

### 2. **Widget 系統**
提供豐富的 UI 組件：

```rust
// 文字區域
use ratatui::widgets::TextArea;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;

// 聊天輸入框
pub struct ChatComposer {
    textarea: TextArea<'static>,
    // ... 其他狀態
}

impl Widget for ChatComposer {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // 渲染聊天輸入框
    }
}
```

### 3. **樣式系統**
提供統一的樣式和主題管理：

```rust
use ratatui::style::{Color, Style, Stylize};

// 樣式設定
let style = Style::default()
    .fg(Color::White)
    .bg(Color::Black)
    .add_modifier(Modifier::BOLD);

// 使用 Stylize trait
let text = "Hello".white().on_black().bold();
```

### 4. **緩衝區管理**
管理終端輸出緩衝區，提供差異更新：

```rust
use ratatui::buffer::Buffer;
use ratatui::backend::Backend;

// 自訂終端實作
pub struct CustomTerminal<B: Backend> {
    backend: B,
    buffers: [Buffer; 2],
    current: usize,
}
```

## 兩者的分工合作

### 1. **分層架構**
```
┌─────────────────────────────────────┐
│           Ratatui (高階)            │  ← UI 組件、佈局、樣式
├─────────────────────────────────────┤
│         Crossterm (低階)            │  ← 終端控制、事件處理
├─────────────────────────────────────┤
│           終端/作業系統              │
└─────────────────────────────────────┘
```

### 2. **具體分工**

#### **Crossterm 負責**:
- ✅ 終端原始模式切換
- ✅ 鍵盤事件捕獲
- ✅ 游標位置控制
- ✅ 顏色和樣式設定
- ✅ 螢幕清除和重置
- ✅ 跨平台終端差異處理

#### **Ratatui 負責**:
- ✅ UI 組件渲染
- ✅ 佈局管理
- ✅ 文字格式化
- ✅ 緩衝區差異更新
- ✅ Widget 生命週期管理
- ✅ 響應式設計

### 3. **實際使用範例**

```rust
// Crossterm: 處理終端事件
let mut crossterm_events = crossterm::event::EventStream::new();
match event {
    Event::Key(key_event) => {
        // 處理按鍵
    }
}

// Ratatui: 渲染 UI
impl Widget for ChatWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // 使用 Ratatui 的佈局和組件
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([...])
            .split(area);
        
        // 渲染各個組件
        self.chat_area.render(layout[0], buf);
        self.input_area.render(layout[1], buf);
    }
}
```

## 總結

**Crossterm** 和 **Ratatui** 在 codex TUI 中形成了完美的分工：

- **Crossterm** 是「底層工人」，負責與終端硬體互動
- **Ratatui** 是「高階建築師」，負責 UI 設計和渲染

這種分層設計讓 codex 能夠：
1. 充分利用終端的低階功能（Crossterm）
2. 提供現代化的 UI 體驗（Ratatui）
3. 保持跨平台相容性
4. 實現高效能渲染

兩者結合創造了一個功能強大、使用者體驗優良的 TUI 應用程式！
````

我希望實際TUI的介面可以類似下面這種格式

```bash
❯ codex
╭────────────────────────────────────────────────╮
│ >_ OpenAI Codex (v0.42.0)                      │
│                                                │
│ model:     gpt-5-codex high   /model to change │
│ directory: ~/repo/codex_usage                  │
╰────────────────────────────────────────────────╯

  To get started, describe a task or try one of these commands:

  /init - create an AGENTS.md file with instructions for Codex
  /status - show current session configuration
  /approvals - choose what Codex can do without approval
  /model - choose what model and reasoning effort to use

▌ Explain this codebase

⏎ send   Ctrl+J newline   Ctrl+T transcript   Ctrl+C quit
``` 

用斜線來 trigger, 因為未來有可能會增加其他新功能
當我打 斜線以後 下面會有一些 hint

```bash
❯ codex
╭────────────────────────────────────────────────╮
│ >_ OpenAI Codex (v0.42.0)                      │
│                                                │
│ model:     gpt-5-codex high   /model to change │
│ directory: ~/repo/codex_usage                  │
╰────────────────────────────────────────────────╯

  To get started, describe a task or try one of these commands:

  /init - create an AGENTS.md file with instructions for Codex
  /status - show current session configuration
  /approvals - choose what Codex can do without approval
  /model - choose what model and reasoning effort to use

▌ /
▌ /model      choose what model and reasoning effort to use
▌ /approvals  choose what Codex can do without approval
▌ /review     review my current changes and find issues
▌ /new        start a new chat during a conversation
▌ /init       create an AGENTS.md file with instructions for Codex
▌ /compact    summarize conversation to prevent hitting the context limit
▌ /diff       show git diff (including untracked files)
▌ /mention    mention a file
```