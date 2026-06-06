# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust CLI (`vibe_coding_tracker`, short alias `vct`) that scans on-disk session logs written by five AI coding assistants — Claude Code, OpenAI Codex, GitHub Copilot CLI, and Gemini CLI (JSONL files), plus OpenCode (a SQLite database) — and aggregates them into two views:

- **`usage`** — per-model token counts and LiteLLM-priced cost
- **`analysis`** — per-model file-operation and tool-call metrics (read/write/edit lines, Bash/Edit/Read/Write/TodoWrite call counts)

Both subcommands support four output modes (interactive TUI / static table / plain text / JSON) and four time-range filters (`--daily` / `--weekly` / `--monthly` / `--all`). The interactive TUI is the default when no output flag is given.

Two auxiliary subcommands round out the CLI: `vct version` prints build/toolchain info (table default, plus `--json` / `--text`), and `vct update` self-replaces the binary from the matching GitHub release asset (`--check` to inspect availability only, `--force` to skip the confirmation prompt).

## Common commands

The toolchain is pinned to `rust-toolchain.toml` (1.95.0, edition 2024). On this machine `cargo` is **not** on the default `PATH`; export it before invoking any cargo command:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Standard `cargo build` / `cargo test` / `cargo bench` work as usual. Project-specific entries:

| Command                                     | What it does                                                                          |
| ------------------------------------------- | ------------------------------------------------------------------------------------- |
| `cargo build --profile dist --locked`       | Distribution build (fat LTO, single codegen unit) — used by release artifacts         |
| `cargo build --release --features mimalloc` | Opt-in mimalloc allocator (faster one-shot, ~10× higher RSS in TUI loops)             |
| `make fmt`                                  | `cargo fmt --all` + `cargo clippy --fix` + clippy with `-D warnings`                  |
| `uvx pre-commit run --all-files`            | Run all pre-commit hooks (whitespace, JSON/YAML/TOML, mdformat, gitleaks, shellcheck) |
| `uvx pre-commit install --install-hooks`    | Install the git hooks once after cloning                                              |

Criterion benchmarks live at `benches/benchmarks.rs`; reports land in `target/criterion`.

**Before every commit / PR**, always run `make fmt` and `uvx pre-commit run -a`. CI runs both with `-D warnings`, and the pre-commit hooks gate-keep the repo (gitleaks, mdformat, etc.).

## Architecture

### Two-stage pipeline

```
JSONL session file
        │
        ▼
src/session/        ← provider detection + per-provider parsers → CodeAnalysis
        │
        ▼
src/analysis/       ← roll up CodeAnalysis records → AggregatedAnalysisRow
src/usage/          ← roll up CodeAnalysis records → UsageResult / PerProviderUsage
        │
        ▼
src/display/        ← TUI / table / text / JSON renderers
```

`src/session/` owns the "raw bytes → typed `CodeAnalysis`" boundary so both `analysis` and `usage` consume the same parsed shape. Do **not** add direct file parsing to `src/usage/` or `src/analysis/`; route everything through `src/session/parser.rs`.

### Provider classification

`src/session/detector.rs` distinguishes the four providers by JSONL markers:

- **Gemini** — first line is a session-meta record with `sessionId` + `projectHash` and *no* `messages` array
- **Copilot CLI** — first line is `type == "session.start"` with `data.producer` starting with `"copilot"`
- **Claude Code** — any record carrying a `parentUuid` field
- **Codex** — any record whose `type` is one of `session_meta` / `turn_context` / `event_msg` / `response_item`, or default fallback when no other marker is found

Four parser entry points live in `src/session/parser.rs`:

- `parse_session_file(path) -> serde_json::Value` — untyped JSON wrapper used by `vct analysis --path` in `main.rs`. Returns the same shape as the golden fixtures under `examples/`.
- `parse_session_file_typed(path) -> CodeAnalysis` — typed, content-based auto-detection. **Only** for the CLI single-file path when no provider is known up front.
- `parse_session_file_typed_with_mode(path, mode)` — same as above but lets the caller choose `ParseMode::Full` vs `UsageOnly`.
- `parse_session_file_as(path, provider, mode)` — caller already knows the provider from the source directory. Use this from every directory walker — it eliminates the "metadata sentinel mis-classifies a Claude session as Codex" bug class. `provider` is the `Provider` enum defined in `src/models/provider.rs`.

`classify_records()` is the streaming-friendly variant that returns `None` on indeterminate records and lets callers keep peeking until a marker arrives — there is **no fixed peek window**, which is critical because a Claude metadata preamble (`permission-mode`, `file-history-snapshot`, …) can be arbitrarily long.

OpenCode does **not** flow through the detector or `parse_session_file_*`: it lives in a single SQLite database, so `src/session/opencode.rs` reads it directly (`read_opencode_usage` from assistant messages with a legacy `session` fallback, `read_opencode_analysis` from `message` + `part`) and produces the same `CodeAnalysis` shape, which the `usage` / `analysis` aggregators fold in alongside the file-based providers. The DB is opened read-only (with a temp-copy fallback) so the user's database is never mutated.

OpenCode cost is a special case: `usage::resolve_model_cost` prices an OpenCode model from tokens only on an **exact** LiteLLM match (`ModelPricingMap::get_exact`); with no exact match it uses the stored assistant-message cost carried in `UsageData::opencode_costs` rather than the normalized/substring/fuzzy fallback the other providers use. This keeps a novel model like `deepseek-v4-pro` from being mis-priced against a loosely-similar name.

### `ParseMode`

`src/session/state.rs` defines `ParseMode::Full` vs `ParseMode::UsageOnly`. The usage path uses `UsageOnly` to skip allocating the large `write_file_details` / `edit_file_details` bodies — this is a major part of why the TUI sits at ~30–50 MB RSS even on 200+ session directories. Preserve this distinction when adding new fields to `CodeAnalysisRecord`.

### Provider directories

Resolved by `src/utils/paths.rs` (`resolve_paths`):

| Provider      | Source path                                                                                                                                    |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Claude Code   | `~/.claude/projects/**/*.jsonl` (recursive — includes subagents)                                                                               |
| Codex         | `~/.codex/sessions/**/*.jsonl`                                                                                                                 |
| Copilot CLI   | `~/.copilot/session-state/<sessionId>/events.jsonl` (depth-bounded walk via `COPILOT_SESSION_MAX_DEPTH` to skip per-session snapshot subtrees) |
| Gemini CLI    | `~/.gemini/tmp/<project_hash>/chats/*.jsonl`                                                                                                   |
| OpenCode      | `~/.local/share/opencode/opencode.db` (SQLite database, read via `rusqlite`; honors `$XDG_DATA_HOME`)                                          |
| Pricing cache | `~/.vibe_coding_tracker/model_pricing_YYYY-MM-DD.json`                                                                                         |

### Pricing (`src/pricing/`)

1. Daily pricing fetched from LiteLLM (`https://github.com/BerriAI/litellm/raw/.../model_prices_and_context_window.json`)
2. Cached as `~/.vibe_coding_tracker/model_pricing_YYYY-MM-DD.json`. The cache stores the **filtered raw upstream JSON**, not the derived `ModelPricing` shape — so future versions can read tiered/flex/batch pricing without re-fetching.
3. Lookup priority: exact → normalized (strip version suffix) → substring → Jaro-Winkler fuzzy (≥0.7 threshold) → $0.00 fallback
4. `ModelPricingMap` precomputes normalized + lowercase indices and uses `Rc<str>` keys to avoid cloning. There is also a small in-process LRU (`MATCH_CACHE`) for repeated lookups during a TUI refresh.

### Memory tuning (Linux glibc only)

`src/main.rs` calls `tune_system_allocator()` **before** any allocation. It applies `mallopt(M_ARENA_MAX, 2)` + `mallopt(M_TRIM_THRESHOLD, 128 KiB)` to stop Rayon worker arenas from multiplying retention across cores. Each TUI refresh ends with `release_freed_heap()` (`malloc_trim(0)`) to hand free pages back to the kernel. **Do not remove these calls** — without them the TUI grows ~6 MB per 10 s refresh on long sessions. Both are no-ops on non-Linux/glibc.

### File cache (`src/cache/`)

Global singleton `GLOBAL_FILE_CACHE` (capacity = 5 in `constants::capacity::FILE_CACHE_SIZE`) keyed by `PathBuf`, holding `Arc<CodeAnalysis>` — **typed** form, not `serde_json::Value`, because `to_value` deep-clones every string. Invalidation is by mtime. TUI keeps cache size small to bound RSS; bump deliberately if you change the displayed-sessions horizon.

### Build version (`build.rs`)

`BUILD_VERSION` is generated from `git describe` (latest tag + commits since + short SHA + `-dirty` suffix when applicable). Outside a git worktree it falls back to `Cargo.toml`. `BUILD_RUST_VERSION` and `BUILD_CARGO_VERSION` come from `rustc --version` / `cargo --version` at build time. All three are exposed via `vct version`.

`src/main.rs` also short-circuits the top-level `--version` / `-V` flag *before* `Cli::parse()`, by inspecting `std::env::args_os().nth(1)` and printing `VERSION` directly. This keeps the conventional `vct --version` flag working in parallel with the `vct version` subcommand — preserve this branch if you touch the entry point.

### Self-update (`src/update/`)

`vct update` resolves the current host's `(os, arch, libc)` tuple via `platform.rs`, fetches the matching asset from the latest GitHub Releases tag, extracts the archive (zip on Windows, tar.gz elsewhere) via `archive.rs`, then atomically replaces the running binary. `mod.rs` exposes `check_update()` (no-op probe) and `update_interactive(force)` (the path `--force` skips the confirmation prompt). `extract_semver_version()` strips the `git describe` suffix so the freshness comparison only looks at the SemVer tag.

## Conventions

- **Commit messages** are English-only and follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` / `perf:` / `refactor:` / `style:` / `test:` / `chore:` / `ci:`). The `semantic-pull-request` workflow enforces this on PR titles, and `git-cliff` consumes them for release notes.
- When CLI behavior or flags change, update **all three** READMEs (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) in the same PR — they must stay in sync.
- The wrapper packages under `cli/nodejs/` and `cli/python/` download the matching GitHub release binary at install time; they're not built from source via `cargo`.
- Test layout follows [Rust Book ch11-03](https://doc.rust-lang.org/book/ch11-03-test-organization.html): unit tests inline in `src/<module>/*.rs` inside `#[cfg(test)] mod tests`; each file under `tests/` compiles to its own binary. The integration-test split is one-file-per-subsystem:
    - `tests/cli.rs` — `assert_cmd`-driven end-to-end checks of the built binary (subcommand wiring, flag conflicts, exit codes)
    - `tests/parser.rs` — golden-output comparison against `examples/analysis_result_*.json`, ignoring environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`)
    - `tests/analysis.rs` — `aggregate_sessions_by_model` rollup logic
    - `tests/usage.rs` — `get_usage_from_directories` aggregation
    - `tests/pricing.rs` — `ModelPricingMap` lookup priority + tiered pricing math
    - `tests/cache.rs` — LRU file cache + pricing cache invalidation
- Sample fixtures and golden outputs for the four JSONL providers live in `examples/` (one `test_conversation_<provider>.jsonl` plus one `analysis_result_<provider>.json` per provider). OpenCode has no JSONL fixture; its SQLite reader is covered by inline unit tests in `src/session/opencode.rs` that build a temp database.
