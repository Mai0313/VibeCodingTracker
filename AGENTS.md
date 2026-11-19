# AGENTS.md

## Project Overview

Rust Cargo workspace (`vibe_coding_tracker`, alias `vct`) that scans local (and optional ssh-fetched remote) session logs from ten AI coding assistants (Claude Code, Codex, Copilot CLI, Gemini CLI, Grok CLI, DeepSeek Harness, Antigravity, OpenCode, Cursor, Hermes) and aggregates them into `usage` (tokens + LiteLLM-priced cost) and `analysis` (file ops + tool calls), plus `quota`, `ledger export`, `config`, `update`, and `version`.

Strict one-way crate boundary under `src/`: `vct-core` → `vct-tui` → `vct-cli` (plus dev-only unpublished `vct-test-support`):

- `src/core/` (`vct-core`): zero terminal or CLI dependencies (no `ratatui`, `crossterm`, `comfy-table`, or `clap`), keeping core reusable for a future GUI.
- `src/tui/` (`vct-tui`): `display` + compile-time auto-discovered `layouts/` plugins (`src/tui/build.rs`) and Criterion benchmarks (`src/tui/benches/benchmarks.rs`).
- `src/cli/` (`vct-cli`): `vibe_coding_tracker` binary (`src/cli/src/cli.rs`, `src/cli/src/main.rs`).

Pipeline boundaries:

- Route all file parsing through `src/core/src/session/parser.rs` (`raw bytes -> CodeAnalysis`); never add direct file parsing inside `src/core/src/usage/` or `src/core/src/analysis/`.
- `src/core/src/scan/` owns the shared `ScanDiagnostics`, `CompactSink`, and ledger-backed scanners; `ledger` MUST NOT import from `usage` or `analysis`.
- Keep priced roll-ups and summary builders (`usage::scan_usage_priced`, `build_usage_summary`, `convert_to_analysis_rows`, `usage::price_usage_data`) in `vct-core`, never in `vct-tui` or `vct-cli`.
- `GLOBAL_FILE_CACHE` is a library-only compatibility API; never reconnect it to the CLI/TUI hot path.

## Common commands

Toolchain is pinned in `rust-toolchain.toml` (edition 2024). On this machine `cargo` is **not** on the default `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Non-obvious commands:

| Command                                                | What it does                                                                                                                                          |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo build --profile dist --locked`                  | Distribution build (fat LTO, single codegen unit) used by release artifacts                                                                           |
| `cargo build --release -p vct-cli --features mimalloc` | Opt-in mimalloc allocator (faster one-shot, ~10× higher RSS in TUI loops)                                                                             |
| `make fmt`                                             | `cargo clippy --fix` + `cargo fmt --all` + `rustfmt` on `src/tui/src/layouts` (`cargo fmt` skips `$OUT_DIR` `#[path]` plugins) + clippy `-D warnings` |
| `cargo doc --no-deps --workspace --all-features`       | Rustdoc check (`.cargo/config.toml` denies all rustdoc warnings); also run with `--document-private-items`                                            |
| `uvx pre-commit run -a`                                | Repo hooks (whitespace, JSON/YAML/TOML, mdformat, gitleaks, shellcheck, SKILL.md description check)                                                   |

**Before every commit / PR:** run `make fmt`, `uvx pre-commit run -a`, and both `cargo doc --no-deps --workspace --all-features` passes (with and without `--document-private-items`, since each checks intra-doc links the other cannot see).

**Before calling a code change done:** run the built debug binary against the real session directories (writing any output outside the repo), verifying exit 0 on: `version` (table / `--json` / `--text`); `usage` and `analysis` (`--json` / `--text` / `--table` × `--daily` / `--weekly` / `--monthly` / `--all`); `usage --table --merge-providers`; `analysis tests/fixtures/sessions/claude_code.jsonl`; `quota <provider>` for each logged-in provider; `config path` / `show` / `schema`; `ledger export`. Then launch the TUI under a real PTY as bare `vct`, `quota`, `usage`, `analysis`, and the latter two's `--daily` / `--weekly` / `--monthly` variants: wait ~1.5 s, send `q` (quits even while loading), and expect exit 0 plus `\x1b[?1049h`; visit every tab (`Tab`) in one run. Use a throwaway `HOME` for any run pressing keys on `Settings` (changes persist immediately to `~/.vct/config.toml`). Grep output for `thread '.*' panicked`, `RUST_BACKTRACE`, or `stack backtrace` — never bare `panic`, which session content can contain.

## Subsystem rules (`vct-dev` skill)

Before planning, editing, or reviewing code in any subsystem below, load `.agents/skills/vct-dev/SKILL.md` and read the matching reference file:

| Area                                                                                                                                                               | Reference                                                    |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------ |
| Providers, parsers, `ParseMode`, token accounting, directory discovery (`session/`, `scan/`, `usage/`, `analysis/`, `utils/`)                                      | `.agents/skills/vct-dev/references/providers-and-parsers.md` |
| Interactive app, keybindings, OSC 10/11 palette, layout plugins (`src/tui/`)                                                                                       | `.agents/skills/vct-dev/references/tui-and-layouts.md`       |
| Quota panels, token refresh, provider API quirks, Antigravity `agy` rules (`quota/`, `models/quota.rs`)                                                            | `.agents/skills/vct-dev/references/quota.md`                 |
| LiteLLM pricing cache, lookup tie-breaks, per-request context tiers (`pricing/`)                                                                                   | `.agents/skills/vct-dev/references/pricing.md`               |
| Session ledger continuity/concurrency and ssh remote ledgers (`ledger/`, `remote/`, `cache/`)                                                                      | `.agents/skills/vct-dev/references/ledger-and-remotes.md`    |
| Persistent config/schema/migrations, file logging, self-update, build version, wrapper packages (`config/`, `logging/`, `update/`, `build.rs`, `src/cli/`, `cli/`) | `.agents/skills/vct-dev/references/config-update-runtime.md` |

## Repo-wide invariants and conventions

- **Public repository confidentiality:** never name a company-internal product, codename, machine type, or sign-in system anywhere in code, comments, test names, docs, commit messages, PR titles/bodies, issues, or release notes. Refer to an Antigravity build other than `agy` CLI or Desktop as "another build" or "a build with its own sign-in"; literal paths may appear only where the code must inspect them.
- **Never open a user's SQLite database read-write:** OpenCode, Cursor, Hermes, and Antigravity all read databases exclusively through `src/core/src/session/sqlite.rs`.
- **Process lifecycle and `~/.vct` boundaries:**
    - Never configure Rayon's global thread pool; use `scan::build_scan_pool`.
    - Keep `tune_system_allocator()` before the first allocation in `src/cli/src/main.rs` and `release_freed_heap()` after every successful TUI refresh (Linux glibc; prevents ~6 MB per 10 s refresh RSS growth).
    - Keep the top-level `--version` / `-V` short-circuit before `Cli::parse()`, `remove_legacy_cache_files()` after `Cli::parse()`, and `utils::finish_atomic_writes()` at the end of `main()` (prevents detached workers from leaving partial temp files).
    - Preserve "settings-free commands never create `~/.vct`": `config::load()` runs lazily only in `usage` / `analysis`-batch / `open_app`; `spawn_cli_auto_update` and `ledger export` use `config::load_read_only()`; `FileLogger` opens lazily on the first record.
- **Docs, comments, and commits:**
    - Conventional Commits in English (`semantic-pull-request` enforces PR titles; `git-cliff` generates release notes).
    - CLI behavior or flag changes MUST update all three READMEs (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) in the same PR. Workspace, build, test, lint, or PR rule changes MUST update `.github/CONTRIBUTING.md`.
    - `src/cli/src/cli.rs` `///` comments are user-facing `--help` text; do not strip them for restating field names. `JsonSchema` `///` comments in `src/core/src/config/mod.rs` and `src/core/src/models/filter.rs` drive `vct.schema.json` and generated `config.toml` comments — regenerate `vct.schema.json` (`vct config schema`) if changed.
    - Put a file path in a code comment only when the comment is an explicit pointer (`see docs/quota.md`), never as decoration.
- **Hermetic tests and fixtures:**
    - No real network, no machine-file reads, and no in-process `HOME` / `VCT_OFFLINE` mutation: inject paths via `TempHome` (`vct-test-support`), `*_from_paths`, `*_in`, and local `httpmock` servers; call `clear_pricing_cache()` before cache-generation assertions. CLI version probing stays opt-in (`quota::enable_cli_version_detection()` only in `main.rs`).
    - Every `assert_cmd` child in `src/cli/tests/cli.rs` that reaches dispatch (including `vct version`) MUST use the isolated `child_cmd` helper (per-child `HOME`, `USERPROFILE`, `HERMES_HOME`, `GROK_HOME`, `DSH_HOME`, cleared XDG vars, `VCT_OFFLINE=1`). Only pre-dispatch checks (`--help`, invalid flags, `--version` / `-V`) may inherit the ambient environment. Two intentional exceptions remove `VCT_OFFLINE` on top of `child_cmd`: `dev_build_skips_auto_update_before_network_or_cache_writes` (proving debug builds skip auto-update before network/cache I/O) and `online_cmd` (remote-ledger ssh stand-in tests, which seed today's pricing cache and set a dead proxy `127.0.0.1:9` so no real network call can succeed).
    - Test fixtures live under `tests/fixtures/` via `vct_test_support::fixture()` / `fixture_str()`, **never** `examples/` (Cargo reserves `examples/`). Antigravity's fixture nests under `tests/fixtures/sessions/antigravity/agy/` so global `.gitignore` rules matching `antigravity/brain/` do not hide it.
