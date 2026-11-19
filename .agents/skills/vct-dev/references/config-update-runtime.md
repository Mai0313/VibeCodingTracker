# Config, Logging, Self-Update, and Build Version

## Persistent config and schema (`src/core/src/config/`, `src/core/src/models/filter.rs`)

- Typed structs in `src/core/src/config/mod.rs` (plus `TimeRange` in `src/core/src/models/filter.rs`) are the single source of truth for both `vct.schema.json` and the commented default `~/.vct/config.toml` (`default_document`).
- `schemars` uses `inline_subschemas = true` so `annotate_table` walks `properties.<section>` directly instead of `$ref`/`$defs`. Editing any `///` doc on a schema type, field, or variant changes `vct.schema.json` and fails `committed_schema_matches_generated` — regenerate `vct.schema.json` via `vct config schema` whenever schema docs change.
- Reads are infallible (missing/malformed -> `Config::default()`); writes go through `toml_edit` (`save_setting`) to preserve user comments and unknown keys.
- Adding a setting: add a `#[serde(default)]` field with a `///` doc comment, thread it through the explicit config seams (never re-read `config.toml` per refresh tick), and add a row to `FIELDS` in `src/tui/src/display/settings.rs` if user-editable in TUI.
- Legacy upgrade uses two layers: idempotent structural rewrite (`migrate_document`) plus read-time `migrate_legacy` backstop for inline tables. **Never replace `migrate_legacy` with `#[serde(alias)]`**: serde rejects files containing both old and new names as duplicate fields, which infallible loading turns into a silent full-config reset.
- When a provider gains a quota panel: bump `CONFIG_VERSION` and add a `(version, panel)` row to `PANELS_ADDED_BY_VERSION`. Preserve the three edge cases in `backfill_new_quota_panels`: empty `panels = []` stamps `CONFIG_VERSION` without appending (keeps band off); inline `usage`/`quota` tables skip both append and stamp (retiring an upgrade that did not run would lose it forever); an inline `[general]` table bails outright.
- `performance.scan_threads` resolution: positive config value → positive `RAYON_NUM_THREADS` → auto `clamp(ceil(parallelism / 2), 2, 8)`, capped at `available_parallelism` and ≥ 1 (`0` = auto).

## Diagnostic logging (`src/core/src/logging/`, `src/tui/src/display/common/tui.rs`)

- Custom `log::Log` writes exclusively to `~/.vct/logs/vct-YYYY-MM-DD.log` (UTC date, unbuffered single `write_all` behind a `Mutex` so records survive `panic = "abort"`). Never log to stdout/stderr (TUI safety).
- File creation is lazy on first log record so commands that log nothing never create `~/.vct`.
- `main.rs` calls `logging::init()` (level `warn`) once before dispatch, installs `vct_tui::display::common::tui::ensure_terminal_panic_hook()` separately (core `logging` has no TUI dep), and applies `[logging]` (`logging::apply`) only in branches that call `config::load()`.
- The panic hook restores the terminal when `IN_TUI` is set AND either the panicking thread owns the TUI or the build aborts (`panic = "abort"` in `release`/`dist`), chaining to the previous hook only when `!tui_active || restore_terminal`.
- Parser schema drift logs once per source/fingerprint (total count + first concrete reason), never per record. Interactive failures stay file-only; non-interactive partial failures emit one stderr summary; the two non-TUI pricing warnings both `eprintln!` and log.

## Self-update and release packaging (`src/core/src/update/`)

- `platform.rs` hardcodes `-gnu` on Linux (no musl build in release matrix). Unix stages a sibling binary, validates `--version`, and atomically renames; Windows spawns a hidden PowerShell helper (`InstallationDisposition::Deferred`) that waits for parent PID *before* taking `.vct-update.lock` and strips `\\?\<drive>:` prefixes for PowerShell 5.1.
- Background auto-update (`update::maybe_auto_update`) runs only in release builds (`!cfg!(debug_assertions)`) when `[general].auto_update` is enabled and `VCT_OFFLINE` is unset. TUI runs it detached; one-shot commands (except `update`/`config`) run it concurrently and join before exit.
- Startup check throttle lives in `~/.vct/version/vct.json` (separate from `config.toml`, never folded into it). `record_check_attempt_in` MUST stamp `last_checked_at` *before* lock acquisition and network calls so contended locks or unwritable install dirs are throttled to once per UTC day instead of warning on every command. `.vct-update.lock` beside the binary (`lock.rs`) is strictly non-blocking: background auto-update skips when contended, while explicit `vct update` errors with `Another vct update is already running`.
- Tests in `update` and `remote` that spawn executables or take locks MUST share `#[serial(update_spawn)]` (prevents Unix `ETXTBSY` / inherited `flock` flakes).
- Release workflow builds `cargo build --profile dist --locked --target <target>` -> `target/<target>/dist/vibe_coding_tracker`, publishes crates in order `vct-core` → `vct-tui` → `vct-cli` (`vct-test-support` stays unpublished), and bundles prebuilt binaries into wrapper packages (`cli/nodejs/binaries/<platform>/` and `cli/python/src/vibe_coding_tracker/binaries/<platform>/`; neither builds from source via `cargo`).

## Build version (`src/core/build.rs`, `src/cli/src/main.rs`)

- `BUILD_VERSION` is assembled git-describe-style (fallback `Cargo.toml` outside a git worktree).
- `src/cli/src/main.rs` short-circuits top-level `--version` / `-V` via `std::env::args_os().nth(1)` *before* `Cli::parse()` so `vct --version` coexists with `vct version` without triggering dispatch or `config::load()`. Preserve this branch.
