# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust CLI (`vibe_coding_tracker`, short alias `vct`) that scans on-disk JSONL session logs written by four AI coding assistants — Claude Code, OpenAI Codex, GitHub Copilot CLI, and Gemini CLI — and aggregates them into two views:

- **`usage`** — per-model token counts and LiteLLM-priced cost
- **`analysis`** — per-model file-operation and tool-call metrics (read/write/edit lines, Bash/Edit/Read/Write/TodoWrite call counts)

Both subcommands support four output modes (interactive TUI / static table / plain text / JSON) and four time-range filters (`--daily` / `--weekly` / `--monthly` / `--all`). The interactive TUI is the default when no output flag is given.

## Common commands

The toolchain is pinned to `rust-toolchain.toml` (1.95.0, edition 2024). On this machine `cargo` is **not** on the default `PATH`; export it before invoking any cargo command:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

| Command                                     | What it does                                                                          |
| ------------------------------------------- | ------------------------------------------------------------------------------------- |
| `cargo build`                               | Debug build                                                                           |
| `cargo build --release --locked`            | Release build (used for benchmarking / dogfooding)                                    |
| `cargo build --profile dist --locked`       | Distribution build (fat LTO, single codegen unit) — used by release artifacts         |
| `cargo build --release --features mimalloc` | Opt-in mimalloc allocator (faster one-shot, ~10× higher RSS in TUI loops)             |
| `make fmt`                                  | `cargo fmt --all` + `cargo clippy --fix` + clippy with `-D warnings`                  |
| `cargo test --all`                          | Full test suite (lib unit tests + every integration test binary)                      |
| `cargo test --test <name>`                  | A single integration crate (one file under `tests/` = one binary)                     |
| `cargo test --lib <module::path>`           | Unit tests in a specific src module (e.g. `--lib pricing::matching`)                  |
| `cargo test <test_name> -- --nocapture`     | A single test by name across all binaries                                             |
| `cargo bench`                               | Criterion benchmarks (`benches/benchmarks.rs`); reports in `target/criterion`         |
| `uvx pre-commit run --all-files`            | Run all pre-commit hooks (whitespace, JSON/YAML/TOML, mdformat, gitleaks, shellcheck) |
| `uvx pre-commit install --install-hooks`    | Install the git hooks once after cloning                                              |

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

Two entry points:

- `parse_session_file_typed(path)` — content-based auto-detection. **Only** for the CLI single-file path (`vct analysis --path ...`).
- `parse_session_file_as(path, provider, mode)` — caller already knows the provider from the source directory. Use this from every directory walker — it eliminates the "metadata sentinel mis-classifies a Claude session as Codex" bug class.

`classify_records()` is the streaming-friendly variant that returns `None` on indeterminate records and lets callers keep peeking until a marker arrives — there is **no fixed peek window**, which is critical because a Claude metadata preamble (`permission-mode`, `file-history-snapshot`, …) can be arbitrarily long.

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

## Conventions

- **Commit messages** are English-only and follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` / `perf:` / `refactor:` / `style:` / `test:` / `chore:` / `ci:`). The `semantic-pull-request` workflow enforces this on PR titles, and `git-cliff` consumes them for release notes.
- When CLI behavior or flags change, update **all three** READMEs (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) in the same PR — they must stay in sync.
- The wrapper packages under `cli/nodejs/` and `cli/python/` download the matching GitHub release binary at install time; they're not built from source via `cargo`.
- Test layout follows [Rust Book ch11-03](https://doc.rust-lang.org/book/ch11-03-test-organization.html): unit tests inline in `src/<module>/*.rs` inside `#[cfg(test)] mod tests`; each file under `tests/` compiles to its own binary.
- Sample fixtures and golden outputs for all four providers live in `examples/`. The `tests/parser.rs` integration test compares actual analyzer output to the golden JSON while ignoring environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`).
