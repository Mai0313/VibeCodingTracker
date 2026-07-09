# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust CLI (`vibe_coding_tracker`, short alias `vct`) that scans on-disk session logs written by six AI coding assistants — Claude Code, OpenAI Codex, GitHub Copilot CLI, and Gemini CLI (JSONL files), OpenCode (a SQLite database), plus Cursor (per-conversation SQLite chat stores for `analysis`, and its dashboard usage API for `usage`) — and aggregates them into two views:

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

OpenCode cost is a special case: `usage::resolve_model_cost` prices an OpenCode model from tokens only on an **exact** LiteLLM match (`ModelPricingMap::get_exact`); with no exact match it uses the stored assistant-message cost carried in `UsageData::stored_costs` rather than the normalized/substring/fuzzy fallback the other providers use. This keeps a novel model like `deepseek-v4-pro` from being mis-priced against a loosely-similar name. Cursor reuses this same stored-cost mechanism but with its own basis — `resolve_model_cost` takes a `CostSource` (`Litellm` / `OpenCodeStored` = exact-match-else-stored / `CursorStored` = the dashboard cost verbatim, never re-priced). The two providers' stored costs live in **separate** per-provider maps (`UsageData::stored_costs` is a `StoredCosts { opencode, cursor }`) so a legacy OpenCode session carrying a bare model name can't cross-contaminate a same-named Cursor model.

Cursor is split across two boundaries and, like OpenCode, bypasses the detector / `parse_session_file_*` — `src/session/cursor.rs` owns it:

- **`analysis`** reads per-conversation blob stores at `~/.cursor/chats/*/*/store.db`. Each store is a content-addressed SQLite blob DAG: assistant turns live in **binary protobuf nodes** (`field 4` = the message JSON, `field 26` = timestamp, `field 5` = a running context-window gauge), while `Read` tool results live in standalone JSON blobs joined back by `toolCallId`. `read_cursor_analysis` decodes only those three fields (never the DAG topology, so it is robust to schema additions) and folds each turn's tool calls (`Write` / `StrReplace`→edit / `Read` / `Shell`→bash / `TodoWrite`) through `SessionParseState`. The conversation's model comes from `~/.cursor/ai-tracking/ai-code-tracking.db` (`conversationId -> model`, one model per conversation in practice), falling back to the store's hex-encoded `meta.lastUsedModel`.
- **`usage`** does **not** come from local files — Cursor stores only the context gauge, not billing tokens. `read_cursor_usage` fetches real per-model tokens + cost from Cursor's individual dashboard API (`POST cursor.com/api/dashboard/get-filtered-usage-events`, `teamId:0`, `Origin: https://cursor.com`) reusing the quota panel's `WorkosCursorSessionToken` (via `quota::cursor::{read_cursor_session, cursor_ua}`), aggregates events per `(date, model)`, and caches them in `~/.vct/cursor_usage_events.json` (TTL `CURSOR_USAGE_CACHE_TTL_SECS`) so the TUI does not re-hit the endpoint every refresh. When the API is unreachable and no cache exists it falls back to a deliberately-rough, input-only **approximation** from the local context gauge (`$0` cost for models Cursor prices itself). Both Cursor SQLite DBs are opened read-only with the same temp-copy/WAL fallback as OpenCode.

### `ParseMode`

`src/session/state.rs` defines `ParseMode::Full` vs `ParseMode::UsageOnly`. The usage path uses `UsageOnly` to skip allocating the large `write_file_details` / `edit_file_details` bodies — this is a major part of why the TUI sits at ~30–50 MB RSS even on 200+ session directories. Preserve this distinction when adding new fields to `CodeAnalysisRecord`.

### Token accounting quirks

`src/utils/token_extractor.rs` (`extract_token_counts`) normalizes provider shapes into disjoint billable buckets. Two provider-specific subtleties:

- **Codex reasoning is a subset of output.** Codex follows OpenAI's convention where `total_token_usage.output_tokens` (completion) already includes `reasoning_output_tokens`, and `total_tokens == input + output`. The Codex branch subtracts reasoning back out of `output_tokens` so each token is billed once, and uses the published `total_tokens` verbatim. Do **not** re-add reasoning to output or total here. (Gemini / Copilot report reasoning *disjoint* from output, so their flat branch keeps the buckets separate without subtracting.)
- **Claude `advisor_message` iterations are counted (for `usage` only).** Claude Code's top-level `usage` equals the sum of the `message`-type entries in `usage.iterations` and excludes any `advisor_message` iteration. `src/session/claude.rs` captures those advisor tokens in a **separate** `CodeAnalysisRecord::advisor_usage` map (keyed by the advisor's own model, `#[serde(skip)]`), so vct's Claude `usage` totals run **higher** than Claude Code's own `/cost`. They are kept out of `conversation_usage` on purpose: the `analysis` aggregator attributes a record's file-op / tool counts to every model in `conversation_usage`, and an advisor model never executes tools, so adding it there would mis-credit it with the main model's metrics.

### Provider directories

Resolved by `src/utils/paths.rs` (`resolve_paths`):

| Provider      | Source path                                                                                                                                                      |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Claude Code   | `~/.claude/projects/**/*.jsonl` (recursive — includes subagents)                                                                                                 |
| Codex         | `~/.codex/sessions/**/*.jsonl`                                                                                                                                   |
| Copilot CLI   | `~/.copilot/session-state/<sessionId>/events.jsonl` (depth-bounded walk via `COPILOT_SESSION_MAX_DEPTH` to skip per-session snapshot subtrees)                   |
| Gemini CLI    | `~/.gemini/tmp/<project_hash>/chats/*.jsonl`                                                                                                                     |
| OpenCode      | `~/.local/share/opencode/opencode.db` (SQLite database, read via `rusqlite`; honors `$XDG_DATA_HOME`)                                                            |
| Cursor        | `~/.cursor/chats/*/*/store.db` (SQLite chat stores, `analysis`) + `~/.cursor/ai-tracking/ai-code-tracking.db` (model attribution); `usage` via the dashboard API |
| Pricing cache | `~/.vct/model_pricing_YYYY-MM-DD.json`                                                                                                                           |

### Quota panels (`src/quota/`)

The `usage` TUI shows live remaining quota for **Claude / Codex / Copilot / Cursor**, each fetched over HTTP on its own background thread (`provider::spawn_quota_worker`, per-provider cadence via its `refresh_secs` arg — Codex ~10s, Claude/Copilot/Cursor ~60s to stay under stricter endpoint rate limits; the stale-drop threshold scales with the cadence), seeded from a `~/.vct/<provider>_usage.json` cache. There is **no** `statusline` subcommand — the old `vct statusline ingest` mechanism was removed; Claude quota now comes from `GET https://api.anthropic.com/api/oauth/usage` (sent with the `anthropic-beta: oauth-2025-04-20` header to unlock the richer `limits` / `spend` fields — per-model weekly cap + credit balance). The panel's Plan line reads `rateLimitTier` (fallback `subscriptionType`) from the credentials file, prettified (`default_claude_max_20x` -> `max 20x`). The per-model `weekly_scoped` window is **volatile** (e.g. Fable is subscription-only, time-limited), so the whole `limits` array is parsed leniently (`de_lenient_limits` skips malformed entries; scalar windows tolerate null) and the scoped row is only rendered when both the window and its model label resolve — an absent/broken scope silently drops the row, never erroring the response.

**Copilot** (`src/quota/copilot.rs`) reads the long-lived `gho_` token from `~/.copilot/config.json` (which is **JSONC** — `strip_jsonc_comments` removes `//` / `/* */` comments in a string-aware scan so a `//` inside a `"https://github.com:login"` key survives) and calls `GET https://api.github.com/copilot_internal/user` with `Authorization: token <gho_>`. It **impersonates the Copilot CLI** (the client the token belongs to): `User-Agent: GitHubCopilotCLI/<version>` (version via the shared `detect_cli_version("copilot", "copilot_version.json", …)`) + `Copilot-Integration-Id: copilot-cli`. The panel maps `premium_interactions` into the gauge (`used_percent = 100 - percent_remaining`, dropping the zero-entitlement placeholder) plus `chat`/`completions` unlimited flags. **No token refresh** (the `gho_` token is long-lived); a `401`/`403` surfaces a `run: copilot login` hint.

**Cursor** (`src/quota/cursor.rs`) reads the WorkOS session JWT from `~/.config/cursor/auth.json` (`cursor_dir` honors `$XDG_CONFIG_HOME`), base64url-decodes the payload for `sub` (userID after the final `|`) and `exp`, synthesizes the `WorkosCursorSessionToken=<uid>%3A%3A<accessToken>` cookie, and calls `GET https://cursor.com/api/usage-summary`, impersonating the Cursor CLI via a `User-Agent: cursor-agent/<version>` (version via the shared `detect_cli_version("cursor-agent", "cursor_version.json", …)`). It maps `totalPercentUsed` / `autoPercentUsed` / `apiPercentUsed` into three gauges (reset = `billingCycleEnd`) plus on-demand spend (cents→USD). Refresh is **reactive**: the file is re-read each tick and the token used while its JWT `exp` is in the future (the official Cursor client keeps it fresh); **never written back**. Expiry or a `401`/`403` surfaces a `run: cursor-agent login` hint.

- **Credential files read:** Claude `~/.claude/.credentials.json` (`claudeAiOauth`, written back on refresh), Codex `~/.codex/auth.json` (`tokens`, written back on refresh), Copilot `~/.copilot/config.json` (`copilotTokens`, read-only), Cursor `~/.config/cursor/auth.json` (`accessToken`, read-only). On macOS Claude stores its credentials in the Keychain (Claude panel absent there); Cursor's `~/.config/cursor` path is Linux-oriented.
- **Token refresh (Claude / Codex only)** lives in `src/quota/refresh.rs` (shared primitives) + each fetcher. It only fires when a token is near expiry (Claude `expiresAt` ms) or an API call returns 401 (Codex is reactive-only — `auth.json` has no expiry). A refreshed access token is cached in memory so the worker reuses it instead of refreshing every tick; the new token is also written back **atomically, preserving every other field** (`update_json_file_in_place` mutates a whole `serde_json::Value`, never a narrow struct) in that CLI's exact timestamp format. Both providers' refresh tokens **rotate** (persist the new one) — a re-check of the file mtime just before write aborts the write if a concurrent official CLI rotated first. Copilot and Cursor have no driveable refresh, so they skip this path entirely.
- **Backoff:** a refresh failure arms a per-provider cooldown (`RefreshCooldown`, 5 min) keyed on the credential file mtime, so a revoked token cannot spin the token endpoint; a mtime change (re-login) retries immediately. Persistent failure surfaces a `run: <provider> auth login` hint (`needs_login` is a snapshot field independent of `QuotaSource`, so Codex keeps showing session-fallback data alongside the hint).
- Panels are **TUI-only** and appear only for a provider whose credentials exist. With four providers, the band layout is responsive (`arrange_band` / `split_band` in `src/display/usage/interactive.rs`): the slimmed Provider Usage table (Provider / Tokens / Cost — **Active Days column dropped**) shares the row only when the terminal is wide enough, otherwise it folds out and the panels take the full width, wrapping to a 2×2 grid at narrow widths. `src/display/common/table.rs::main_layout` is **shared with the analysis view** and must not change; only the band height fed to it grows.

**Antigravity (investigated, deferred — not shipped):** a full Antigravity quota implementation was built and then removed. Blocker: its Google OAuth client id/secret live only inside the Antigravity **IDE** language-server binary (`~/.antigravity-ide-server/.../language_server_linux_x64`), not in `~/.gemini/antigravity-cli`, so there is no clean way to obtain them without either committing the (public installed-app) secret or extracting it from that binary at runtime — plus Antigravity is still beta. Endpoints/shapes are captured in the memory note `project_antigravity_quota`.

### Pricing (`src/pricing/`)

1. Daily pricing fetched from LiteLLM (`https://github.com/BerriAI/litellm/raw/.../model_prices_and_context_window.json`)
2. Cached as `~/.vct/model_pricing_YYYY-MM-DD.json`. The cache stores the **filtered raw upstream JSON**, not the derived `ModelPricing` shape — so future versions can read tiered/flex/batch pricing without re-fetching.
3. Lookup priority: exact → normalized (strip version suffix) → substring → Jaro-Winkler fuzzy (≥0.7 threshold) → $0.00 fallback
4. `ModelPricingMap` precomputes normalized + lowercase indices and uses `Rc<str>` keys to avoid cloning. There is also a small in-process LRU (`MATCH_CACHE`) for repeated lookups during a TUI refresh.
5. Cost is not token-only: Claude `server_tool_use.web_search_requests` is billed **per query** at `ModelPricing::web_search_cost_per_query` (derived by `parse_litellm_entry` from LiteLLM's nested `search_context_cost_per_query`, a flat $0.01 for Anthropic). `resolve_model_cost` adds it on top of the token cost; it is 0 for every non-Claude model. `web_fetch_requests` is **not** separately billed (its fetched content already counts as input tokens).

### Memory tuning (Linux glibc only)

`src/main.rs` calls `tune_system_allocator()` **before** any allocation. It applies `mallopt(M_ARENA_MAX, 2)` + `mallopt(M_TRIM_THRESHOLD, 128 KiB)` to stop Rayon worker arenas from multiplying retention across cores. Each TUI refresh ends with `release_freed_heap()` (`malloc_trim(0)`) to hand free pages back to the kernel. **Do not remove these calls** — without them the TUI grows ~6 MB per 10 s refresh on long sessions. Both are no-ops on non-Linux/glibc.

### File cache (`src/cache/`)

Global singleton `GLOBAL_FILE_CACHE` (capacity = 5 in `constants::capacity::FILE_CACHE_SIZE`) keyed by `PathBuf`, holding `Arc<CodeAnalysis>` — **typed** form, not `serde_json::Value`, because `to_value` deep-clones every string. Invalidation is by mtime. TUI keeps cache size small to bound RSS; bump deliberately if you change the displayed-sessions horizon.

### Build version (`build.rs`)

`BUILD_VERSION` is generated from `git describe` (latest tag + commits since + short SHA + `-dirty` suffix when applicable). Outside a git worktree it falls back to `Cargo.toml`. `BUILD_RUST_VERSION` and `BUILD_CARGO_VERSION` come from `rustc --version` / `cargo --version` at build time. All three are exposed via `vct version`.

`src/main.rs` also short-circuits the top-level `--version` / `-V` flag *before* `Cli::parse()`, by inspecting `std::env::args_os().nth(1)` and printing `VERSION` directly. This keeps the conventional `vct --version` flag working in parallel with the `vct version` subcommand — preserve this branch if you touch the entry point.

### Self-update (`src/update/`)

`vct update` resolves the current host's `(os, arch, libc)` tuple via `platform.rs`, fetches the matching asset from the latest GitHub Releases tag, extracts the archive (zip on Windows, tar.gz elsewhere) via `archive.rs`, then atomically replaces the running binary. `mod.rs` exposes `check_update()` (no-op probe) and `update_interactive(force)` (the path `--force` skips the confirmation prompt). `extract_semver_version()` strips the `git describe` suffix so the freshness comparison only looks at the SemVer tag.

Every update check records the result to `~/.vct/version.json` via `version_cache::record_version_check` (`SelfVersion { latest_version, last_checked_at, dismissed_version }`) as groundwork for a future auto-update prompt. It preserves any existing `dismissed_version` and stamps `last_checked_at` with `now_rfc3339_utc_nanos()` (RFC3339 UTC nanoseconds, e.g. `2026-07-07T05:34:50.563606999Z`) — the same stamp the per-CLI version caches (`{claude,codex,copilot,cursor}_version.json`, written by `detect_cli_version`) now use for their `last_checked_at` field. All four version caches refresh once per UTC day.

## Conventions

- **Commit messages** are English-only and follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` / `perf:` / `refactor:` / `style:` / `test:` / `chore:` / `ci:`). The `semantic-pull-request` workflow enforces this on PR titles, and `git-cliff` consumes them for release notes.
- When CLI behavior or flags change, update **all three** READMEs (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) in the same PR — they must stay in sync.
- The wrapper packages under `cli/nodejs/` and `cli/python/` download the matching GitHub release binary at install time; they're not built from source via `cargo`.
- Test layout follows [Rust Book ch11-03](https://doc.rust-lang.org/book/ch11-03-test-organization.html): unit tests inline in `src/<module>/*.rs` inside `#[cfg(test)] mod tests`; each file under `tests/` compiles to its own binary, and `tests/common/mod.rs` is the shared helper module (`TempHome`, `fixture`). The integration-test split is one-file-per-subsystem:
    - `tests/cli.rs` — `assert_cmd`-driven checks of the built binary, in two groups: a zero-env, zero-network group (version / help / `analysis --path` / flag conflicts) and per-child-HOME smoke tests (`usage` / `analysis` batch against an isolated temp HOME seeded with fixtures plus an offline pricing cache)
    - `tests/parser.rs` — golden-output comparison against `examples/analysis_result_*.json`, ignoring environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`)
    - `tests/analysis.rs` — `aggregate_sessions_by_model_from_paths` rollup logic over a `TempHome`
    - `tests/usage.rs` — `get_usage_from_paths` aggregation over a `TempHome`
    - `tests/pricing.rs` — `ModelPricingMap` lookup priority + tiered pricing math, plus `fetch_model_pricing_with` fetch/cache against an `httpmock` server
    - `tests/cache.rs` — LRU file cache + pricing cache invalidation
    - `tests/http_mock.rs` — HTTP-layer tests of the public quota fetchers (`call_wham`, `refresh_codex`) against an `httpmock` server
- **Tests are hermetic: no real external API, no machine-file reads, no ambient env control.** Isolation comes from dependency injection, not `HOME`/`VCT_OFFLINE` mutation: the `*_from_paths` / `resolve_paths_from_home` / `fetch_model_pricing_with` / cache `*_in` seams take an explicit temp dir (via `TempHome` in `tests/common`), and every network call is pointed at a local `httpmock` server through the injected endpoint parameters. The 401 → refresh → retry loop and each provider's send layer are covered by inline `#[cfg(test)]` tests in their source files (which can reach crate-private items). `VCT_OFFLINE` / `network_disabled()` remain a **production** offline feature but no test depends on them, so `cargo test` passes fully offline **without** any env var — the same way CI runs it. The only env used anywhere is a per-child `HOME` on the handful of `assert_cmd` smoke tests (there is no other way to isolate a separate binary's home). Keep tests self-contained (e.g. `clear_pricing_cache()` before asserting on the global match-cache).
- Sample fixtures and golden outputs for the four JSONL providers live in `examples/` (one `test_conversation_<provider>.jsonl` plus one `analysis_result_<provider>.json` per provider). OpenCode has no JSONL fixture; its SQLite reader is covered by inline unit tests in `src/session/opencode.rs` that build a temp database.
