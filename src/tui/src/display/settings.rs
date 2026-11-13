//! The Settings tab: every `config.toml` field as a form, saved on change.
//!
//! The fields are one table ([`FIELDS`]), drawn as one list the selection
//! walks from end to end; a section is a run of fields sharing a name, and the
//! section list beside the form only marks which one the selection is in.
//! Values are read and written by path through
//! [`vct_core::config`], so the typed [`Config`] stays the only definition of
//! what a field holds, and its doc-comment is the help text shown here.

use crate::display::common::table::{create_controls_with_status, render_too_small, title_line};
use crate::display::common::theme::theme;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use vct_core::config::{Config, SettingValue, apply_setting, get_setting, setting_description};

/// Below this only a notice is drawn.
const SETTINGS_MIN_W: u16 = 64;
const SETTINGS_MIN_H: u16 = 16;
/// Width of the section list, borders included.
const SECTIONS_W: u16 = 18;

/// How a field is shown and edited.
#[derive(Clone, Copy)]
enum Kind {
    /// On or off.
    Bool,
    /// One of a fixed set of names.
    Choice(&'static [&'static str]),
    /// A whole number no smaller than `min`. `zero` names what `0` means when
    /// it is not a quantity.
    Number {
        min: i64,
        unit: &'static str,
        zero: Option<&'static str>,
    },
    /// Free text; empty shows as `auto`.
    Text,
    /// A list of ssh destinations.
    Hosts,
    /// Whether this name is in the list at the field's path.
    Member(&'static str),
}

/// One row of the form.
struct Field {
    section: &'static str,
    label: &'static str,
    path: &'static [&'static str],
    kind: Kind,
    /// Whether a change shows in the running app, or only from the next start.
    live: bool,
}

const fn field(
    section: &'static str,
    label: &'static str,
    path: &'static [&'static str],
    kind: Kind,
    live: bool,
) -> Field {
    Field {
        section,
        label,
        path,
        kind,
        live,
    }
}

const SECONDS: Kind = Kind::Number {
    min: 1,
    unit: " s",
    zero: None,
};
const PANELS: &[&str] = &["usage", "quota", "panels"];

const FIELDS: &[Field] = &[
    field(
        "General",
        "Default time range",
        &["general", "default_time_range"],
        Kind::Choice(&["daily", "weekly", "monthly", "all"]),
        false,
    ),
    field(
        "General",
        "Check for updates on start",
        &["general", "auto_update"],
        Kind::Bool,
        false,
    ),
    field(
        "Usage",
        "Merge models across providers",
        &["usage", "merge_models"],
        Kind::Bool,
        true,
    ),
    field(
        "Usage",
        "Redraw interval",
        &["usage", "refresh_interval"],
        SECONDS,
        true,
    ),
    field(
        "Usage",
        "Quota panel: Claude",
        PANELS,
        Kind::Member("claude"),
        true,
    ),
    field(
        "Usage",
        "Quota panel: Codex",
        PANELS,
        Kind::Member("codex"),
        true,
    ),
    field(
        "Usage",
        "Quota panel: Copilot",
        PANELS,
        Kind::Member("copilot"),
        true,
    ),
    field(
        "Usage",
        "Quota panel: Cursor",
        PANELS,
        Kind::Member("cursor"),
        true,
    ),
    field(
        "Usage",
        "Quota panel: Grok",
        PANELS,
        Kind::Member("grok"),
        true,
    ),
    field(
        "Usage",
        "Quota panel: Antigravity",
        PANELS,
        Kind::Member("antigravity"),
        true,
    ),
    field(
        "Usage",
        "Quota poll interval",
        &["usage", "quota", "refresh_interval"],
        SECONDS,
        true,
    ),
    field(
        "Analysis",
        "Redraw interval",
        &["analysis", "refresh_interval"],
        SECONDS,
        true,
    ),
    field(
        "Providers",
        "Claude Code",
        &["providers", "claude"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Codex",
        &["providers", "codex"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Copilot CLI",
        &["providers", "copilot"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Gemini CLI",
        &["providers", "gemini"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "OpenCode",
        &["providers", "opencode"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Cursor",
        &["providers", "cursor"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Hermes",
        &["providers", "hermes"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Grok CLI",
        &["providers", "grok"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "DeepSeek Harness",
        &["providers", "dsh"],
        Kind::Bool,
        true,
    ),
    field(
        "Providers",
        "Antigravity",
        &["providers", "antigravity"],
        Kind::Bool,
        true,
    ),
    field(
        "Remotes",
        "Hosts",
        &["remotes", "hosts"],
        Kind::Hosts,
        false,
    ),
    field(
        "Remotes",
        "vct command on the hosts",
        &["remotes", "command"],
        Kind::Text,
        false,
    ),
    field(
        "Remotes",
        "Fetch interval",
        &["remotes", "refresh_interval"],
        Kind::Number {
            min: 60,
            unit: " s",
            zero: None,
        },
        false,
    ),
    field(
        "Performance",
        "Scan threads",
        &["performance", "scan_threads"],
        Kind::Number {
            min: 0,
            unit: "",
            zero: Some("auto"),
        },
        false,
    ),
    field(
        "Logging",
        "Log level",
        &["logging", "level"],
        Kind::Choice(&["off", "error", "warn", "info", "debug", "trace"]),
        false,
    ),
    field(
        "Logging",
        "Keep log files for",
        &["logging", "retention_days"],
        Kind::Number {
            min: 0,
            unit: " days",
            zero: Some("forever"),
        },
        false,
    ),
];

/// The section names, in the order they first appear in [`FIELDS`].
fn sections() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for field in FIELDS {
        if names.last() != Some(&field.section) {
            names.push(field.section);
        }
    }
    names
}

/// An editor opened over the form.
enum Editor {
    /// Picking one name from a field's options.
    Choice { selected: usize },
    /// Typing a number or a line of text.
    Input { buffer: String },
    /// Editing the host list. `input` is the line being typed, with the index
    /// it replaces, or `None` for a new entry.
    Hosts {
        items: Vec<String>,
        selected: usize,
        input: Option<(Option<usize>, String)>,
    },
}

/// What a key press in the Settings tab asks of the app.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SettingsOutcome {
    /// Nothing beyond a redraw.
    Stay,
    /// Leave the app.
    Quit,
    /// Show another tab (zero-based).
    SwitchTab(usize),
    /// Show the tab after this one (`Tab`).
    NextTab,
    /// Show the tab before this one (`Shift+Tab`).
    PrevTab,
    /// Show the key list (`?`).
    Help,
    /// A field changed and was saved; the app re-reads [`SettingsUiState::config`].
    Changed,
    /// Open the config file in the user's editor (`e`).
    OpenEditor,
}

/// Writes one field to the config file.
pub(crate) type SaveSetting = Box<dyn FnMut(&[&str], &SettingValue) -> anyhow::Result<()>>;

/// What the Settings tab keeps between frames.
pub(crate) struct SettingsUiState {
    config: Config,
    save: SaveSetting,
    sections: Vec<&'static str>,
    descriptions: Vec<String>,
    /// Index into [`FIELDS`] of the selected field.
    row: usize,
    /// First line of the form on screen.
    offset: usize,
    editor: Option<Editor>,
    /// The last save's outcome and whether it succeeded.
    message: Option<(String, bool)>,
}

impl SettingsUiState {
    /// A form over `config` that persists each change through `save`.
    pub(crate) fn new(config: Config, save: SaveSetting) -> Self {
        Self {
            config,
            save,
            sections: sections(),
            descriptions: FIELDS
                .iter()
                .map(|field| {
                    // A field with no comment of its own is explained by its
                    // section's.
                    setting_description(field.path)
                        .or_else(|| setting_description(&field.path[..field.path.len() - 1]))
                        .unwrap_or_default()
                        // The comment's own line breaks would fight the box's.
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect(),
            row: 0,
            offset: 0,
            editor: None,
            message: None,
        }
    }

    /// The settings as they now stand.
    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    /// Replaces the settings, after something else wrote the file.
    pub(crate) fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    /// Shows `text` beside the key hints, as a success or a failure.
    pub(crate) fn set_message(&mut self, text: String, ok: bool) {
        self.message = Some((text, ok));
    }

    fn current(&self) -> usize {
        self.row
    }

    /// The first field of the section `step` sections away, stopping at
    /// either end.
    fn section_start(&self, step: isize) -> usize {
        let section = FIELDS[self.row].section;
        let at = self
            .sections
            .iter()
            .position(|name| *name == section)
            .unwrap_or(0) as isize;
        let target = self.sections[(at + step).clamp(0, self.sections.len() as isize - 1) as usize];
        FIELDS
            .iter()
            .position(|field| field.section == target)
            .unwrap_or(0)
    }

    fn value(&self, config: &Config, index: usize) -> Option<SettingValue> {
        get_setting(config, FIELDS[index].path)
    }

    /// The text shown for a field's value in `config`.
    fn display(&self, config: &Config, index: usize) -> String {
        let field = &FIELDS[index];
        match (field.kind, self.value(config, index)) {
            (Kind::Bool, Some(SettingValue::Bool(on))) => checkbox(on),
            (Kind::Member(name), Some(SettingValue::List(items))) => {
                checkbox(items.iter().any(|item| item.eq_ignore_ascii_case(name)))
            }
            (Kind::Choice(_), Some(SettingValue::String(name))) => format!("‹ {name} ›"),
            (Kind::Number { unit, zero, .. }, Some(SettingValue::Integer(number))) => {
                match zero.filter(|_| number == 0) {
                    Some(name) => format!("‹ {name} ›"),
                    None => format!("‹ {number}{unit} ›"),
                }
            }
            (Kind::Text, Some(SettingValue::String(text))) if text.is_empty() => "auto".to_string(),
            (Kind::Text, Some(SettingValue::String(text))) => text,
            (Kind::Hosts, Some(SettingValue::List(items))) if items.is_empty() => {
                "none".to_string()
            }
            (Kind::Hosts, Some(SettingValue::List(items))) => items.join(", "),
            _ => "?".to_string(),
        }
    }

    /// Applies and saves `value` for the field at `index`.
    fn commit(&mut self, index: usize, value: SettingValue) -> SettingsOutcome {
        let path = FIELDS[index].path;
        let mut next = self.config.clone();
        let result =
            apply_setting(&mut next, path, &value).and_then(|()| (self.save)(path, &value));
        match result {
            Ok(()) => {
                self.config = next;
                let note = if FIELDS[index].live {
                    "saved"
                } else {
                    "saved, applies on next start"
                };
                self.message = Some((note.to_string(), true));
                SettingsOutcome::Changed
            }
            Err(error) => {
                self.message = Some((format!("not saved: {error:#}"), false));
                SettingsOutcome::Stay
            }
        }
    }

    /// Flips a Bool or Member field, or steps a Choice or Number by `step`.
    fn nudge(&mut self, step: i64) -> SettingsOutcome {
        let index = self.current();
        let Some(value) = self.value(&self.config, index) else {
            return SettingsOutcome::Stay;
        };
        let next = match (FIELDS[index].kind, value) {
            (Kind::Bool, SettingValue::Bool(on)) => SettingValue::Bool(!on),
            (Kind::Member(name), SettingValue::List(mut items)) => {
                let before = items.len();
                items.retain(|item| !item.eq_ignore_ascii_case(name));
                if items.len() == before {
                    items.push(name.to_string());
                }
                SettingValue::List(items)
            }
            (Kind::Choice(options), SettingValue::String(name)) => {
                let at = options
                    .iter()
                    .position(|option| *option == name)
                    .unwrap_or(0) as i64;
                let next = (at + step).rem_euclid(options.len() as i64) as usize;
                SettingValue::String(options[next].to_string())
            }
            (Kind::Number { min, .. }, SettingValue::Integer(number)) => {
                SettingValue::Integer((number + step).max(min))
            }
            _ => return SettingsOutcome::Stay,
        };
        self.commit(index, next)
    }

    /// Opens the editor a field's kind calls for; a Bool just flips.
    fn open_editor(&mut self) -> SettingsOutcome {
        let index = self.current();
        let value = self.value(&self.config, index);
        self.editor = match (FIELDS[index].kind, value) {
            (Kind::Bool | Kind::Member(_), _) => return self.nudge(1),
            (Kind::Choice(options), Some(SettingValue::String(name))) => Some(Editor::Choice {
                selected: options
                    .iter()
                    .position(|option| *option == name)
                    .unwrap_or(0),
            }),
            (Kind::Number { .. }, Some(SettingValue::Integer(number))) => Some(Editor::Input {
                buffer: number.to_string(),
            }),
            (Kind::Text, Some(SettingValue::String(text))) => Some(Editor::Input { buffer: text }),
            (Kind::Hosts, Some(SettingValue::List(items))) => Some(Editor::Hosts {
                items,
                selected: 0,
                input: None,
            }),
            _ => None,
        };
        SettingsOutcome::Stay
    }

    /// Puts the focused field back to its default.
    fn reset(&mut self) -> SettingsOutcome {
        let index = self.current();
        let defaults = Config::default();
        let Some(default) = self.value(&defaults, index) else {
            return SettingsOutcome::Stay;
        };
        // A panel row resets its own name, not the whole list.
        if let Kind::Member(name) = FIELDS[index].kind {
            let on = |value: Option<SettingValue>| {
                matches!(value, Some(SettingValue::List(items))
                    if items.iter().any(|item| item.eq_ignore_ascii_case(name)))
            };
            if on(Some(default)) == on(self.value(&self.config, index)) {
                return SettingsOutcome::Stay;
            }
            return self.nudge(1);
        }
        self.commit(index, default)
    }

    /// Handles one key press.
    pub(crate) fn handle_key(&mut self, key: KeyCode) -> SettingsOutcome {
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        match key {
            KeyCode::Char('q') => return SettingsOutcome::Quit,
            KeyCode::Char('e') => return SettingsOutcome::OpenEditor,
            KeyCode::Char('?') => return SettingsOutcome::Help,
            KeyCode::Char(digit @ '1'..='4') => {
                return SettingsOutcome::SwitchTab(digit as usize - '1' as usize);
            }
            KeyCode::Tab => return SettingsOutcome::NextTab,
            KeyCode::BackTab => return SettingsOutcome::PrevTab,
            KeyCode::Up => self.row = self.row.saturating_sub(1),
            KeyCode::Down => self.row = (self.row + 1).min(FIELDS.len() - 1),
            KeyCode::PageUp => self.row = self.section_start(-1),
            KeyCode::PageDown => self.row = self.section_start(1),
            KeyCode::Left => return self.nudge(-1),
            KeyCode::Right | KeyCode::Char(' ') => return self.nudge(1),
            KeyCode::Enter => return self.open_editor(),
            KeyCode::Char('x') => return self.reset(),
            _ => return SettingsOutcome::Stay,
        }
        self.message = None;
        SettingsOutcome::Stay
    }

    fn handle_editor_key(&mut self, key: KeyCode) -> SettingsOutcome {
        let index = self.current();
        let Some(editor) = self.editor.as_mut() else {
            return SettingsOutcome::Stay;
        };
        match editor {
            Editor::Choice { selected } => {
                let Kind::Choice(options) = FIELDS[index].kind else {
                    return SettingsOutcome::Stay;
                };
                match key {
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => *selected = (*selected + 1).min(options.len() - 1),
                    KeyCode::Enter => {
                        let value = SettingValue::String(options[*selected].to_string());
                        self.editor = None;
                        return self.commit(index, value);
                    }
                    KeyCode::Esc => self.editor = None,
                    _ => {}
                }
            }
            Editor::Input { buffer } => match key {
                KeyCode::Esc => self.editor = None,
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Enter => {
                    let value = match FIELDS[index].kind {
                        Kind::Number { min, .. } => match buffer.trim().parse::<i64>() {
                            Ok(number) if number >= min => SettingValue::Integer(number),
                            _ => {
                                self.message = Some((format!("Must be at least {min}."), false));
                                return SettingsOutcome::Stay;
                            }
                        },
                        _ => SettingValue::String(buffer.trim().to_string()),
                    };
                    self.editor = None;
                    return self.commit(index, value);
                }
                KeyCode::Char(typed) => {
                    let number = matches!(FIELDS[index].kind, Kind::Number { .. });
                    if !number || typed.is_ascii_digit() {
                        buffer.push(typed);
                    }
                }
                _ => {}
            },
            Editor::Hosts {
                items,
                selected,
                input: Some((slot, buffer)),
            } => match key {
                KeyCode::Esc => {
                    if let Some(Editor::Hosts { input, .. }) = self.editor.as_mut() {
                        *input = None;
                    }
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(typed) => buffer.push(typed),
                KeyCode::Enter => {
                    let host = buffer.trim().to_string();
                    // What `remote::sync` refuses before running ssh.
                    if host.is_empty()
                        || host.starts_with('-')
                        || host.contains(char::is_whitespace)
                    {
                        self.message = Some((
                            "A host has no spaces and does not start with '-'.".to_string(),
                            false,
                        ));
                        return SettingsOutcome::Stay;
                    }
                    match *slot {
                        Some(at) => items[at] = host,
                        None => {
                            items.push(host);
                            *selected = items.len() - 1;
                        }
                    }
                    let value = SettingValue::List(items.clone());
                    if let Some(Editor::Hosts { input, .. }) = self.editor.as_mut() {
                        *input = None;
                    }
                    return self.commit(index, value);
                }
                _ => {}
            },
            Editor::Hosts {
                items,
                selected,
                input,
            } => match key {
                KeyCode::Esc => self.editor = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(items.len().saturating_sub(1)),
                KeyCode::Char('a') => *input = Some((None, String::new())),
                KeyCode::Enter if !items.is_empty() => {
                    *input = Some((Some(*selected), items[*selected].clone()));
                }
                KeyCode::Char('d') if !items.is_empty() => {
                    items.remove(*selected);
                    *selected = selected.saturating_sub(1);
                    let value = SettingValue::List(items.clone());
                    return self.commit(index, value);
                }
                _ => {}
            },
        }
        SettingsOutcome::Stay
    }

    /// Draws the tab into `area`.
    pub(crate) fn draw(&mut self, f: &mut Frame, area: Rect) {
        if area.width < SETTINGS_MIN_W || area.height < SETTINGS_MIN_H {
            render_too_small(f, SETTINGS_MIN_W, SETTINGS_MIN_H);
            return;
        }
        let [main, help, controls] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .areas(area);
        let [left, right] =
            Layout::horizontal([Constraint::Length(SECTIONS_W), Constraint::Min(0)]).areas(main);
        let theme = theme();
        let dim = theme.dim();
        let here = FIELDS[self.row].section;

        let section_lines: Vec<Line> = self
            .sections
            .iter()
            .map(|name| {
                if *name == here {
                    Line::from(vec![
                        Span::styled(" › ", theme.accent()),
                        Span::styled(*name, Style::default().bold()),
                    ])
                } else {
                    Line::from(Span::styled(format!("   {name}"), dim))
                }
            })
            .collect();
        f.render_widget(
            Paragraph::new(section_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title_line("Sections"))
                    .border_style(theme.border()),
            ),
            left,
        );

        // One list: a heading before each section's fields, a blank line
        // after them.
        let inner_w = usize::from(right.width.saturating_sub(5));
        let defaults = Config::default();
        let mut lines: Vec<Line> = Vec::new();
        let mut selected_line = 0;
        let mut changed_count = 0;
        for (index, field) in FIELDS.iter().enumerate() {
            if index == 0 || FIELDS[index - 1].section != field.section {
                if index > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(Span::styled(
                    format!(" {}", field.section),
                    dim.bold(),
                )));
            }
            let value = self.display(&self.config, index);
            let label_w = inner_w.saturating_sub(value.chars().count());
            let label: String = field.label.chars().take(label_w).collect();
            let changed = self.value(&self.config, index) != self.value(&defaults, index);
            changed_count += usize::from(changed);
            let value_style = match value.as_str() {
                "[x] on" => Style::default().fg(theme.good),
                "[ ] off" => dim,
                _ => Style::default(),
            };
            let mut line = Line::from(vec![
                Span::styled(
                    if changed { " * " } else { "   " },
                    Style::default().fg(theme.warn),
                ),
                Span::raw(format!("{label:<label_w$}")),
                Span::styled(value, value_style),
                Span::raw(" "),
            ]);
            if index == self.row {
                selected_line = lines.len();
                line = line.style(theme.selected());
            }
            lines.push(line);
        }
        // Keep the selection on screen, with its section heading when it is
        // the first field under one.
        let visible = usize::from(right.height.saturating_sub(2));
        let top = if self.row == 0 || FIELDS[self.row - 1].section != here {
            selected_line - 1
        } else {
            selected_line
        };
        self.offset = self.offset.min(top);
        if selected_line >= self.offset + visible {
            self.offset = selected_line + 1 - visible;
        }
        let hidden_below = lines.len().saturating_sub(self.offset + visible);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(title_line("Settings"))
            .border_style(theme.border());
        let mut notes = Vec::new();
        if changed_count > 0 {
            notes.push(format!("* {changed_count} changed from default"));
        }
        if hidden_below > 0 {
            notes.push(format!("{hidden_below} more ↓"));
        }
        if !notes.is_empty() {
            block = block.title(
                Line::from(Span::styled(format!(" {} ", notes.join(" · ")), dim)).right_aligned(),
            );
        }
        f.render_widget(
            Paragraph::new(lines)
                .scroll((self.offset as u16, 0))
                .block(block),
            right,
        );

        let index = self.current();
        let field = &FIELDS[index];
        let mut title = vec![
            Span::styled(format!(" {} ", field.label), theme.title()),
            Span::styled(format!("· {} ", field.path.join(".")), dim),
        ];
        if !field.live {
            title.push(Span::styled(
                "· applies on next start ",
                Style::default().fg(theme.warn),
            ));
        }
        let default = format!(" default: {} ", self.display(&defaults, index));
        f.render_widget(
            Paragraph::new(self.descriptions[index].as_str())
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme.border())
                        .title(Line::from(title))
                        .title(Line::from(Span::styled(default, dim)).right_aligned()),
                ),
            help,
        );

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    ("←→", "change"),
                    ("space", "toggle"),
                    ("⏎", "edit"),
                    ("x", "reset"),
                    ("e", "editor"),
                ],
                self.message
                    .as_ref()
                    .map(|(text, ok)| (text.as_str(), if *ok { theme.good } else { theme.bad })),
                controls.width,
            ),
            controls,
        );

        if let Some(editor) = &self.editor {
            self.draw_editor(f, area, editor, field);
        }
    }

    /// Draws the open editor centered over `area`.
    fn draw_editor(&self, f: &mut Frame, area: Rect, editor: &Editor, field: &Field) {
        let theme = theme();
        let dim = theme.dim();
        let selected = theme.selected();
        let caret = Span::styled("▏", theme.accent());
        let (lines, hint): (Vec<Line>, &str) = match editor {
            Editor::Choice { selected: at } => {
                let Kind::Choice(options) = field.kind else {
                    return;
                };
                let lines = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let text = format!(" {option} ");
                        if index == *at {
                            Line::from(Span::styled(text, selected))
                        } else {
                            Line::from(text)
                        }
                    })
                    .collect();
                (lines, "↑↓ move  ⏎ choose  esc cancel")
            }
            Editor::Input { buffer } => (
                vec![Line::from(vec![Span::raw(format!(" {buffer}")), caret])],
                "⏎ save  esc cancel",
            ),
            Editor::Hosts {
                items,
                selected: at,
                input,
            } => {
                let mut lines: Vec<Line> = items
                    .iter()
                    .enumerate()
                    .map(|(index, host)| {
                        let text = format!(" {host} ");
                        if index == *at && input.is_none() {
                            Line::from(Span::styled(text, selected))
                        } else {
                            Line::from(text)
                        }
                    })
                    .collect();
                if items.is_empty() && input.is_none() {
                    lines.push(Line::from(Span::styled(" no hosts yet ", dim)));
                }
                match input {
                    Some((_, buffer)) => {
                        lines.push(Line::from(vec![Span::raw(format!(" > {buffer}")), caret]));
                        (lines, "⏎ save  esc cancel")
                    }
                    None => (lines, "a add  ⏎ edit  d delete  esc done"),
                }
            }
        };
        let width = area.width.saturating_sub(4).min(56);
        let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        let mut body = lines;
        body.push(Line::default());
        body.push(Line::from(Span::styled(format!(" {hint}"), dim)));
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(body).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.accent())
                    .title(Line::from(Span::styled(
                        format!(" {} ", field.label),
                        theme.title(),
                    ))),
            ),
            rect,
        );
    }
}

fn checkbox(on: bool) -> String {
    if on { "[x] on" } else { "[ ] off" }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Saved = Rc<RefCell<Vec<(String, SettingValue)>>>;

    /// A form over the defaults whose saves are recorded, not written.
    fn form() -> (SettingsUiState, Saved) {
        let saved: Saved = Rc::default();
        let sink = Rc::clone(&saved);
        let state = SettingsUiState::new(
            Config::default(),
            Box::new(move |path, value| {
                sink.borrow_mut().push((path.join("."), value.clone()));
                Ok(())
            }),
        );
        (state, saved)
    }

    fn press(state: &mut SettingsUiState, keys: &[KeyCode]) -> SettingsOutcome {
        let mut last = SettingsOutcome::Stay;
        for key in keys {
            last = state.handle_key(*key);
        }
        last
    }

    fn go_to(state: &mut SettingsUiState, section: &str, label: &str) {
        state.row = FIELDS
            .iter()
            .position(|field| field.section == section && field.label == label)
            .unwrap();
    }

    fn screen(state: &mut SettingsUiState, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| state.draw(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn every_field_reads_a_value_and_has_help() {
        let (state, _) = form();
        let config = Config::default();
        for (index, field) in FIELDS.iter().enumerate() {
            assert_ne!(state.display(&config, index), "?", "{}", field.label);
            assert!(!state.descriptions[index].is_empty(), "{}", field.label);
        }
    }

    #[test]
    fn a_toggle_saves_at_once_and_reports_the_change() {
        let (mut state, saved) = form();
        go_to(&mut state, "Usage", "Merge models across providers");
        let before = state.config().usage.merge_models;
        assert_eq!(
            press(&mut state, &[KeyCode::Char(' ')]),
            SettingsOutcome::Changed
        );
        assert_eq!(state.config().usage.merge_models, !before);
        assert_eq!(
            saved.borrow().as_slice(),
            [(
                "usage.merge_models".to_string(),
                SettingValue::Bool(!before)
            )]
        );
    }

    #[test]
    fn a_panel_row_edits_one_name_in_the_list() {
        let (mut state, _) = form();
        go_to(&mut state, "Usage", "Quota panel: Codex");
        press(&mut state, &[KeyCode::Enter]);
        assert!(
            !state
                .config()
                .usage
                .quota
                .panels
                .contains(&"codex".to_string())
        );
        assert!(
            state
                .config()
                .usage
                .quota
                .panels
                .contains(&"claude".to_string())
        );
        // Reset puts that one name back and leaves the rest alone.
        assert_eq!(
            press(&mut state, &[KeyCode::Char('x')]),
            SettingsOutcome::Changed
        );
        assert!(
            state
                .config()
                .usage
                .quota
                .panels
                .contains(&"codex".to_string())
        );
        assert_eq!(
            press(&mut state, &[KeyCode::Char('x')]),
            SettingsOutcome::Stay
        );
    }

    #[test]
    fn a_number_refuses_what_is_below_its_minimum() {
        let (mut state, saved) = form();
        go_to(&mut state, "Usage", "Quota poll interval");
        press(
            &mut state,
            &[KeyCode::Enter, KeyCode::Backspace, KeyCode::Backspace],
        );
        // `q` is a letter here, not quit, and a number takes digits only.
        assert_eq!(
            press(&mut state, &[KeyCode::Char('q'), KeyCode::Char('0')]),
            SettingsOutcome::Stay
        );
        assert_eq!(press(&mut state, &[KeyCode::Enter]), SettingsOutcome::Stay);
        assert!(saved.borrow().is_empty());
        assert!(
            state.editor.is_some(),
            "the input stays open to be corrected"
        );
        press(
            &mut state,
            &[KeyCode::Backspace, KeyCode::Char('9'), KeyCode::Char('0')],
        );
        assert_eq!(
            press(&mut state, &[KeyCode::Enter]),
            SettingsOutcome::Changed
        );
        assert_eq!(state.config().usage.quota.refresh_interval, 90);
        // Stepping never goes under the minimum either.
        go_to(&mut state, "Usage", "Redraw interval");
        for _ in 0..20 {
            press(&mut state, &[KeyCode::Left]);
        }
        assert_eq!(state.config().usage.refresh_interval, 1);
    }

    #[test]
    fn a_choice_is_picked_from_its_list() {
        let (mut state, _) = form();
        go_to(&mut state, "General", "Default time range");
        press(
            &mut state,
            &[
                KeyCode::Enter,
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Up,
            ],
        );
        assert_eq!(
            press(&mut state, &[KeyCode::Enter]),
            SettingsOutcome::Changed
        );
        assert_eq!(
            state.config().general.default_time_range,
            vct_core::models::TimeRange::Daily
        );
    }

    #[test]
    fn hosts_are_added_edited_and_deleted() {
        let (mut state, _) = form();
        go_to(&mut state, "Remotes", "Hosts");
        press(&mut state, &[KeyCode::Enter, KeyCode::Char('a')]);
        for typed in "-bad".chars() {
            press(&mut state, &[KeyCode::Char(typed)]);
        }
        assert_eq!(press(&mut state, &[KeyCode::Enter]), SettingsOutcome::Stay);
        for _ in 0..4 {
            press(&mut state, &[KeyCode::Backspace]);
        }
        for typed in "box".chars() {
            press(&mut state, &[KeyCode::Char(typed)]);
        }
        assert_eq!(
            press(&mut state, &[KeyCode::Enter]),
            SettingsOutcome::Changed
        );
        assert_eq!(state.config().remotes.hosts, ["box"]);
        press(&mut state, &[KeyCode::Enter, KeyCode::Char('2')]);
        assert_eq!(
            press(&mut state, &[KeyCode::Enter]),
            SettingsOutcome::Changed
        );
        assert_eq!(state.config().remotes.hosts, ["box2"]);
        assert_eq!(
            press(&mut state, &[KeyCode::Char('d')]),
            SettingsOutcome::Changed
        );
        assert!(state.config().remotes.hosts.is_empty());
        assert_eq!(press(&mut state, &[KeyCode::Esc]), SettingsOutcome::Stay);
        assert!(state.editor.is_none());
    }

    #[test]
    fn a_failed_save_keeps_the_old_value_and_says_so() {
        let mut state = SettingsUiState::new(
            Config::default(),
            Box::new(|_, _| anyhow::bail!("disk full")),
        );
        go_to(&mut state, "General", "Check for updates on start");
        let before = state.config().general.auto_update;
        assert_eq!(
            press(&mut state, &[KeyCode::Char(' ')]),
            SettingsOutcome::Stay
        );
        assert_eq!(state.config().general.auto_update, before);
        let (text, ok) = state.message.clone().unwrap();
        assert!(!ok && text.contains("disk full"), "{text}");
    }

    #[test]
    fn keys_outside_an_editor_leave_or_switch() {
        let (mut state, _) = form();
        for (key, outcome) in [
            (KeyCode::Char('2'), SettingsOutcome::SwitchTab(1)),
            (KeyCode::Tab, SettingsOutcome::NextTab),
            (KeyCode::BackTab, SettingsOutcome::PrevTab),
            (KeyCode::Char('?'), SettingsOutcome::Help),
            (KeyCode::Char('q'), SettingsOutcome::Quit),
            (KeyCode::Char('e'), SettingsOutcome::OpenEditor),
            // Esc backs out of an editor and does nothing without one.
            (KeyCode::Esc, SettingsOutcome::Stay),
        ] {
            assert_eq!(press(&mut state, &[key]), outcome, "{key:?}");
        }
        // Inside an input `e` and `q` are letters, and Tab stays put.
        go_to(&mut state, "Remotes", "vct command on the hosts");
        for key in [
            KeyCode::Enter,
            KeyCode::Char('e'),
            KeyCode::Char('q'),
            KeyCode::Tab,
        ] {
            assert_eq!(press(&mut state, &[key]), SettingsOutcome::Stay, "{key:?}");
        }
        assert!(state.editor.is_some());
    }

    #[test]
    fn the_selection_walks_every_field_and_jumps_by_section() {
        let (mut state, _) = form();
        assert_eq!(press(&mut state, &[KeyCode::Up]), SettingsOutcome::Stay);
        assert_eq!(state.row, 0);
        // Down from a section's last field lands on the next section's first.
        press(&mut state, &[KeyCode::Down, KeyCode::Down]);
        assert_eq!(FIELDS[state.row].section, "Usage");
        // A field is changed where the selection is, with no pane to enter.
        let before = state.config().usage.merge_models;
        assert_eq!(
            press(&mut state, &[KeyCode::Right]),
            SettingsOutcome::Changed
        );
        assert_eq!(state.config().usage.merge_models, !before);

        press(&mut state, &[KeyCode::PageDown]);
        assert_eq!(FIELDS[state.row].label, "Redraw interval");
        assert_eq!(FIELDS[state.row].section, "Analysis");
        press(&mut state, &[KeyCode::PageUp]);
        assert_eq!(FIELDS[state.row].label, "Merge models across providers");
        for _ in 0..FIELDS.len() {
            press(&mut state, &[KeyCode::Down]);
        }
        assert_eq!(state.row, FIELDS.len() - 1);
        for _ in 0..FIELDS.len() {
            press(&mut state, &[KeyCode::PageDown]);
        }
        assert_eq!(FIELDS[state.row].label, "Log level");
    }

    #[test]
    fn the_form_scrolls_to_keep_the_selection_and_its_heading_on_screen() {
        let (mut state, _) = form();
        let (w, h) = (100u16, 20u16);
        let top = screen(&mut state, w, h).join("\n");
        assert!(top.contains("General") && top.contains("more ↓"), "{top}");
        assert!(!top.contains("Keep log files for"), "{top}");

        go_to(&mut state, "Logging", "Keep log files for");
        let bottom = screen(&mut state, w, h).join("\n");
        assert!(bottom.contains("Keep log files for"), "{bottom}");
        assert!(!bottom.contains("more ↓"), "{bottom}");

        // Back at a section's first field, its heading is shown above it.
        go_to(&mut state, "Providers", "Claude Code");
        let lines = screen(&mut state, w, h);
        let field = lines
            .iter()
            .position(|line| line.contains("Claude Code"))
            .unwrap();
        // Row 0 is the pane's border, so the heading is the first row inside.
        assert_eq!(field, 2, "{lines:#?}");
        assert!(lines[1].contains("Providers"), "{lines:#?}");
    }

    #[test]
    fn a_message_is_never_cut_and_takes_the_row_when_it_must() {
        let (mut state, _) = form();
        let message = "Editor 'code --wait' exited with exit status: 1, nothing changed";
        state.set_message(message.to_string(), false);
        let wide = screen(&mut state, 100, 24);
        let last = wide.last().unwrap();
        assert!(last.contains(message) && last.contains("q quit"), "{last}");
        let narrow = screen(&mut state, 80, 24);
        let last = narrow.last().unwrap();
        assert!(last.contains(message) && !last.contains("q quit"), "{last}");
    }

    #[test]
    fn the_form_names_the_field_its_path_and_its_default() {
        let (mut state, _) = form();
        go_to(&mut state, "Remotes", "Fetch interval");
        let screen = screen(&mut state, 100, 24).join("\n");
        for expected in [
            "Sections",
            "› Remotes",
            "Fetch interval",
            "remotes.refresh_interval",
            "applies on next start",
            "default: ‹ 300 s ›",
        ] {
            assert!(screen.contains(expected), "{expected} missing:\n{screen}");
        }
    }
}
