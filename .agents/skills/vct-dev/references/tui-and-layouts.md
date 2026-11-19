# TUI, Palette, and Layout Plugins

## Interactive app and background workers

- `display::app::run_app` (`src/tui/src/display/app.rs`) is the only interactive entry point (started by bare `vct` or `usage`/`analysis`/`quota` without a format flag; off a TTY, bare `vct` prints help and exits 2 before dispatch).
- Usage and Analysis MUST share one `RefreshWorker` holding one `SessionLedger`, created lazily on the first visit to either data tab (together with `remote::spawn_sync_loop`). Two workers with separate ledgers would re-parse cold and let an analysis save restamp the usage half unclassified. Only the active tab refreshes; allow at most one active scan plus one coalesced pending job, schedule the next automatic scan from completion time (never a fixed tick that piles up), and keep last-known-good data on failure without retrying until the next timer or `r`.
- `QuotaRuntime` is created lazily on the first visit to the Quota tab so opening on Usage/Analysis polls no quota endpoint.
- Usage refreshes build merged and unmerged rows together so toggling `Merge models across providers` in Settings never rescans; switching `general.layout` swaps the active `LayoutView` in place without rescanning. The ledger saves when dirty at most every `LEDGER_SAVE_INTERVAL` (60 s; first save immediate).
- `TerminalSession` is the cleanup boundary: setup rolls back partial terminal changes on error, and `Drop` attempts raw-mode disable, mouse-capture disable, alternate-screen leave, cursor restore, and active-flag clear even if an earlier cleanup step fails. `TerminalSession::suspend` (used by `e` on Settings for `$EDITOR`) leaves alternate screen and rebuilds a fresh `Terminal` rather than calling `Terminal::clear()` (which queries cursor position and fails on terminals that do not answer). On return, a broken `config.toml` MUST neither be loaded nor overwritten (`after_editor` + save guard).
- Changing providers or `[usage.quota]` in Settings drops `QuotaRuntime` so it restarts on the next visit to the Quota tab — never restart per key press (each restart polls every worker and spawns `agy`).
- Quota shutdown guards signal workers on exit without joining threads that may be blocked in HTTP timeouts.

## Input and keybinding invariants

- Universal rules across every tab and layout (never break):
    - `q` is the only key that quits.
    - `Esc` only closes what is open (help modal, details modal, Provider Usage panel, Settings editor) and never quits.
    - `Tab` switches tabs (when no modal is open).
    - Sort (`←`/`→`) and time range (`[`/`]`) step along the drawn order and stop at ends; keep sort enum variants in column order.
    - Any new key on a tab or layout MUST have an entry in `LayoutView::help_rows` (or `help_rows` in `app.rs`).
- Outside `Tab::Settings` (with help modal closed), `app.rs` forwards only `Close`, `Open`, `Space`, `TogglePane`, and `Page(_)` to `LayoutView::on_key`; all other keys stay app-global.
- On `Tab::Settings`, an open modal editor MUST intercept mouse clicks before tab navigation so clicking outside dismisses the editor instead of switching tabs behind it.

## Palette (`src/tui/src/display/common/theme.rs`)

- Every color comes from `theme()` by role — never hardcode a `Color` in a view. Provider colors appear only as a `●` dot via `provider_tui_color`, never as borders or text.
- One accent color; green/yellow/red carry state only; borders use one border grey. `dim` and `border` blend terminal `fg`/`bg`, while `selected_bg` blends `bg` with accent (`flash_bg` blends `bg` with green).
- `theme::probe` queries OSC 10/11 once inside `setup_terminal` BEFORE `EnableMouseCapture` or input reads, only on Unix when stdin is a TTY and `COLORTERM` is `truecolor` or `24bit`, ending with a DA1 query (`\x1b[c`) so unread terminal replies never leak as keypresses. Otherwise (and in all tests) it falls back to `Theme::ANSI`.
- Use `Theme::title` on block titles so they do not inherit border grey.

## Compile-time layout plugins (`src/tui/src/layouts/`)

- `src/tui/build.rs` auto-discovers every `<id>/mod.rs` or `<id>.rs` under `src/tui/src/layouts/` into `$OUT_DIR/layouts_registry.rs` (`classic` first, rest alphabetical; forward-slash `#[path = "..."]` for Windows safety).
- **Adding a layout**: drop `<id>/mod.rs` (or `<id>.rs`) exporting `pub(crate) static LAYOUT: <Type> = <Type>;` implementing `pub(crate) trait TuiLayout` (`meta().id == "<id>"`). Do NOT edit `vct-core`, `settings.rs`, or `app.rs` (`LAYOUT_IDS` feeds `Settings > General > Layout` automatically). Note: using `pub static` with a `pub(crate)` type triggers `private_interfaces` under `-D warnings`.
- **Removing a layout**: delete its file/folder. `[general].layout` is a `String` (default `"classic"`), and `layouts::resolve` falls back to `"classic"` (else `LAYOUTS[0]`), so removed layout ids in existing configs never break startup.
- **Formatting trap**: `cargo fmt --all` does NOT follow `#[path = "..."]` modules inside `$OUT_DIR`. Always run `make fmt` (or explicit `rustfmt --edition 2024` over `src/tui/src/layouts/`), mirrored in `.github/workflows/code-quality-check.yml`.
- **Bottom-row OSC 8 hyperlink**: place the 1-row `create_controls_with_status` footer on `y = f.area().bottom() - 1` so `overlay_repo_hyperlink` attaches the link to `Star on GitHub`.
- **Interactive Analysis**: no interactive layout draws a bottom `Providers` table on Analysis (per-provider attribution lives inside each model's Inspector/Drawer/Details/card chips; static `analysis --table` keeps `build_analysis_provider_rows`).

## Degradation and sizing invariants

- **Never drop information silently**: truncated Provider Usage lines show `+N more → p`; truncated modals, side panes, strips, or cards show `+N` / `+{n} ↓` / `+N more`; dropped table columns show `−N cols` in the title; hidden Quota cards are counted in the Quota footer.
- `provider_digest` drops providers in **ascending `total_cost`** order (ties broken from the tail) so the costliest provider always survives on narrow widths.
- **Build quota cards for the actual cell width**: pass the real cell width from `fit_cards`, never `max_w`. Keep the login hint directly under the plan line so short cells never drop the only actionable line.
- **Columns drop by priority, never by squeezing model names**: `fit_columns` drops highest `drop_rank` first against a floored model-name column; slack past the comfortable name width becomes the Usage `Share` bar column.
- `grid_cells` MUST return exactly `fits` non-zero cells because `fit_cards` zips them with `collect_cards`.
