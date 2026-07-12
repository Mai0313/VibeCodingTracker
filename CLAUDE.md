# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust CLI (`vibe_coding_tracker`, short alias `vct`) that scans on-disk session logs written by seven AI coding assistants — Claude Code, OpenAI Codex, GitHub Copilot CLI, and Gemini CLI (JSONL files), OpenCode (a SQLite database), Cursor (per-conversation SQLite chat stores for `analysis`, and its dashboard usage API for `usage`), plus Hermes (a SQLite `session_model_usage` table, `usage` only) — and aggregates them into two views:

- **`usage`** — per-model token counts and LiteLLM-priced cost
- **`analysis`** — per-model file-operation and tool-call metrics (read/write/edit lines, Bash/Edit/Read/Write/TodoWrite call counts)

Both subcommands support four output modes (interactive TUI / static table / plain text / JSON) and four time-range filters (`--daily` / `--weekly` / `--monthly` / `--all`). The interactive TUI is the default when no output flag is given.

Four auxiliary subcommands round out the CLI: `vct version` prints build/toolchain info (table default, plus `--json` / `--text`), `vct update` self-replaces the binary from the matching GitHub release asset (`--check` to inspect availability only, `--force` to skip the confirmation prompt), `vct fetch <provider>` prints a provider's raw quota/usage API response (`claude` / `codex` / `copilot` / `cursor`; `--json` default, plus `--text` / `--table`), and `vct config` shows/edits the persistent settings file (`path` / `show` / `edit` / `schema` / `migrate`; see **Persistent config** below).

## Common commands

The toolchain is pinned to `rust-toolchain.toml` (1.96.1, edition 2024). On this machine `cargo` is **not** on the default `PATH`; export it before invoking any cargo command:

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

OpenCode cost is a special case: `usage::resolve_model_cost` prices an OpenCode model from tokens only on an **exact** LiteLLM match (`ModelPricingMap::get_exact`); with no exact match it uses the stored assistant-message cost carried in `UsageData::stored_costs` rather than the normalized/substring/fuzzy fallback the other providers use. This keeps a novel model like `deepseek-v4-pro` from being mis-priced against a loosely-similar name. Cursor reuses this same stored-cost mechanism but with its own basis — `resolve_model_cost` takes a `CostSource` (`Litellm` / `OpenCodeStored` = exact-match-else-stored / `CursorStored` = the dashboard cost verbatim, never re-priced / `HermesStored` = same exact-match-else-stored basis as OpenCode). The providers' stored costs live in **separate** per-provider maps (`UsageData::stored_costs` is a `StoredCosts { opencode, cursor, hermes }`) so a legacy OpenCode session carrying a bare model name can't cross-contaminate a same-named Cursor or Hermes model.

Cursor is split across two boundaries and, like OpenCode, bypasses the detector / `parse_session_file_*` — `src/session/cursor.rs` owns it:

- **`analysis`** reads per-conversation blob stores at `~/.cursor/chats/*/*/store.db`. Each store is a content-addressed SQLite blob DAG: assistant turns live in **binary protobuf nodes** (`field 4` = the message JSON, `field 26` = timestamp, `field 5` = a running context-window gauge), while `Read` tool results live in standalone JSON blobs joined back by `toolCallId`. `read_cursor_analysis` decodes only those three fields (never the DAG topology, so it is robust to schema additions) and folds each turn's tool calls (`Write` / `StrReplace`→edit / `Read` / `Shell`→bash / `TodoWrite`) through `SessionParseState`. The conversation's model comes from `~/.cursor/ai-tracking/ai-code-tracking.db` (`conversationId -> model`, one model per conversation in practice), falling back to the store's hex-encoded `meta.lastUsedModel`.
- **`usage`** is a **local estimate**: `read_cursor_usage` reads each conversation's context gauge from the chat stores (`approximation_events` → `aggregate_events`), counts it as cache-read tokens, and prices it with LiteLLM (a deliberately-rough, input-only approximation, `$0` for models Cursor prices itself), so Cursor behaves like every other provider (all local-file based) and needs no network or credentials. It undercounts Cursor's real spend because much of it is billed under Cursor-internal model names LiteLLM can't price; the numbers are approximate on purpose. There is **no** dashboard-billing-API path here — the `get-filtered-usage-events` fetch (and its `~/.vct/cursor_usage_events.json` cache) was removed as too much surface for the value; the raw endpoint is documented in `examples/quota.md` if it is ever reintroduced. Both Cursor SQLite DBs are opened read-only with the same temp-copy/WAL fallback as OpenCode.

Hermes is **`usage`-only** and, like OpenCode, is a single SQLite database read directly (bypassing the detector / `parse_session_file_*`) — `src/session/hermes.rs` owns it. `read_hermes_usage` reads the pre-aggregated `session_model_usage` table at `state.db` under Hermes's home (`resolve_hermes_home` in `paths.rs` mirrors Hermes's own `get_hermes_home`: `$HERMES_HOME` wins, else `~/.hermes` on POSIX / `%LOCALAPPDATA%\hermes` on Windows), keying each row by the bare `model` column (so a model billed through several providers merges into one row) and mapping its token columns onto the same flat `CodeAnalysis` shape. Each session is also reconciled against the `sessions` aggregate — the positive residual (`sessions.<col>` minus the sum of its per-model rows) is attributed to the session's recorded model, mirroring Hermes's own insights view so partial or missing per-model rows aren't under-counted. Two quirks: `last_seen`/`first_seen` are REAL epoch **seconds** (not OpenCode's milliseconds), and `output_tokens` follows the OpenAI convention of **including** reasoning, so the reader subtracts `reasoning_tokens` back out to keep each token billed once. The row's cost is `actual_cost_usd` when known, else `estimated_cost_usd`, folded through the same `StoredCosts` path as OpenCode (`HermesStored`). There is no `analysis` reader (the table has no file-operation detail) and no quota panel (local DB, no remote API).

### `ParseMode`

`src/session/state.rs` defines `ParseMode::Full` vs `ParseMode::UsageOnly`. The usage path uses `UsageOnly` to skip allocating the large `write_file_details` / `edit_file_details` bodies — this is a major part of why the TUI sits at ~30–50 MB RSS even on 200+ session directories. Preserve this distinction when adding new fields to `CodeAnalysisRecord`.

### Token accounting quirks

`src/utils/token_extractor.rs` (`extract_token_counts`) normalizes provider shapes into disjoint billable buckets. Two provider-specific subtleties:

- **Codex reasoning is a subset of output.** Codex follows OpenAI's convention where `total_token_usage.output_tokens` (completion) already includes `reasoning_output_tokens`, and `total_tokens == input + output`. The Codex branch subtracts reasoning back out of `output_tokens` so each token is billed once, and uses the published `total_tokens` verbatim. Do **not** re-add reasoning to output or total here. (Gemini / Copilot report reasoning *disjoint* from output, so their flat branch keeps the buckets separate without subtracting.)
- **Claude `advisor_message` iterations are counted (for `usage` only).** Claude Code's top-level `usage` equals the sum of the `message`-type entries in `usage.iterations` and excludes any `advisor_message` iteration. `src/session/claude.rs` captures those advisor tokens in a **separate** `CodeAnalysisRecord::advisor_usage` map (keyed by the advisor's own model, `#[serde(skip)]`), so vct's Claude `usage` totals run **higher** than Claude Code's own `/cost`. They are kept out of `conversation_usage` on purpose: the `analysis` aggregator attributes a record's file-op / tool counts to every model in `conversation_usage`, and an advisor model never executes tools, so adding it there would mis-credit it with the main model's metrics.

### Provider directories

Resolved by `src/utils/paths.rs` (`resolve_paths`):

| Provider      | Source path                                                                                                                                                  |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Claude Code   | `~/.claude/projects/**/*.jsonl` (recursive — includes subagents)                                                                                             |
| Codex         | `~/.codex/sessions/**/*.jsonl`                                                                                                                               |
| Copilot CLI   | `~/.copilot/session-state/<sessionId>/events.jsonl` (depth-bounded walk via `COPILOT_SESSION_MAX_DEPTH` to skip per-session snapshot subtrees)               |
| Gemini CLI    | `~/.gemini/tmp/<project_hash>/chats/*.jsonl`                                                                                                                 |
| OpenCode      | `~/.local/share/opencode/opencode.db` (SQLite database, read via `rusqlite`; honors `$XDG_DATA_HOME`)                                                        |
| Cursor        | `~/.cursor/chats/*/*/store.db` (SQLite chat stores, `analysis` + a local `usage` estimate) + `~/.cursor/ai-tracking/ai-code-tracking.db` (model attribution) |
| Hermes        | `$HERMES_HOME/state.db` (default `~/.hermes`, or `%LOCALAPPDATA%\hermes` on Windows; SQLite `session_model_usage` table, read via `rusqlite`; `usage` only)  |
| Pricing cache | `~/.vct/model_pricing_YYYY-MM-DD.json`                                                                                                                       |
| User settings | `~/.vct/config.toml`                                                                                                                                         |

### Quota panels (`src/quota/`)

The `usage` TUI shows live remaining quota for **Claude / Codex / Copilot / Cursor**, each fetched over HTTP on its own background thread (`provider::spawn_quota_worker`, **one shared poll cadence for every provider** from `config.usage.quota.refresh_interval`, default 60s), seeded from a `~/.vct/<provider>_usage.json` cache. A transient failure — a network error or a `429` rate limit (`QuotaOutcome::Transient`) — keeps the last-known-good snapshot **indefinitely**: the panel keeps showing it with a growing staleness marker until the next success overwrites it, and never blanks out (there is **no** stale-drop). There is **no** `statusline` subcommand — the old `vct statusline ingest` mechanism was removed; Claude quota now comes from `GET https://api.anthropic.com/api/oauth/usage` (sent with the `anthropic-beta: oauth-2025-04-20` header to unlock the richer `limits` / `spend` fields — per-model weekly cap + credit balance). The panel's Plan line reads `rateLimitTier` (fallback `subscriptionType`) from the credentials file, prettified (`default_claude_max_20x` -> `max 20x`). The per-model `weekly_scoped` window is **volatile** (e.g. Fable is subscription-only, time-limited), so the whole `limits` array is parsed leniently (`de_lenient_limits` skips malformed entries; scalar windows tolerate null) and the scoped row is only rendered when both the window and its model label resolve — an absent/broken scope silently drops the row, never erroring the response.

**Copilot** (`src/quota/copilot.rs`) reads the long-lived `gho_` token from `~/.copilot/config.json` (which is **JSONC** — `strip_jsonc_comments` removes `//` / `/* */` comments in a string-aware scan so a `//` inside a `"https://github.com:login"` key survives) and calls `GET https://api.github.com/copilot_internal/user` (host derived per account via `copilot_api_url` — a GHE data-residency login hits `api.<host>` instead) with `Authorization: token <gho_>`. It **impersonates the Copilot CLI** (the client the token belongs to): `User-Agent: GitHubCopilotCLI/<version>` (version via the shared `detect_cli_version("copilot", "copilot_version.json", …)`) + `Copilot-Integration-Id: copilot-cli`. The panel maps only `premium_interactions` into the gauge (`used_percent = 100 - percent_remaining`, dropping the zero-entitlement placeholder); the response's `chat`/`completions` flags are not read. **No token refresh** (the `gho_` token is long-lived); a `401`/`403` surfaces a `run: copilot login` hint.

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

`BUILD_VERSION` is assembled git-describe-style from separate git commands (latest tag + commits since + short SHA + `-dirty` suffix when applicable). Outside a git worktree it falls back to `Cargo.toml`. `BUILD_RUST_VERSION` and `BUILD_CARGO_VERSION` come from `rustc --version` / `cargo --version` at build time. All three are exposed via `vct version`.

`src/main.rs` also short-circuits the top-level `--version` / `-V` flag *before* `Cli::parse()`, by inspecting `std::env::args_os().nth(1)` and printing `VERSION` directly. This keeps the conventional `vct --version` flag working in parallel with the `vct version` subcommand — preserve this branch if you touch the entry point.

### Self-update (`src/update/`)

`vct update` resolves the current host's `(os, arch, libc)` tuple via `platform.rs`, fetches the matching asset from the latest GitHub Releases tag, extracts the archive (zip on Windows, tar.gz elsewhere) via `archive.rs`, then atomically replaces the running binary. `mod.rs` exposes `check_update()` (no-op probe) and `update_interactive(force)` (the path `--force` skips the confirmation prompt). `extract_semver_version()` strips the `git describe` suffix so the freshness comparison only looks at the SemVer tag.

Every update check records the result to `~/.vct/version.json` via `version_cache::record_version_check` (`SelfVersion { latest_version, last_checked_at, dismissed_version }`) as groundwork for a future auto-update prompt. It preserves any existing `dismissed_version` and stamps `last_checked_at` with `now_rfc3339_utc_nanos()` (RFC3339 UTC nanoseconds, e.g. `2026-07-07T05:34:50.563606999Z`) — the same stamp the per-CLI version caches (`{claude,codex,copilot,cursor}_version.json`, written by `detect_cli_version`) now use for their `last_checked_at` field. All four version caches refresh once per UTC day. This `version.json` record is **separate** from `config.toml` (below) and is not folded into it.

### Persistent config (`src/config/`)

`config::load()` reads `~/.vct/config.toml` into a typed `Config` (`general` / `usage` / `analysis` / `providers` sections, with quota settings nested under `[usage.quota]`), **generating a commented default from the typed structs on first run** (no hand-maintained template). Reads are infallible (missing/malformed → `Config::default`); writes go through `toml_edit` so hand-added comments and unknown keys survive (`save_merge_models` is the live-write path for the `m` toggle, and guards against a hand-edited non-table `[usage]` before indexing). `main.rs` loads it lazily — only inside the `usage` and `analysis`-batch branches, so settings-free commands (`version`, `fetch`, `analysis --path`) never read or create `~/.vct/config.toml` — and threads the values down: `default_time_range` via `resolve_time_range_with_default` (an explicit period flag always wins), `providers` (per-provider include toggles) into the `*_with` aggregation entry points (`get_usage_from_directories_with` / `aggregate_sessions_by_model_with`, whose bare wrappers default to all-providers), and `usage.quota.panels` (which quota panels to show) / `usage.refresh_interval` (TUI redraw cadence) / `usage.quota.refresh_interval` (the one shared quota-poll cadence) into the two interactive display functions. A disabled provider skips its whole aggregation block (no scan, no API). The typed structs are the **single source of truth**: `schemars` derives both `vct.schema.json` (committed at the repo root, referenced by a `#:schema` directive on the generated file's first line, and printed/regenerated via `vct config schema`) and the commented default (`default_document` serializes `Config::default()` and injects each field's doc-comment as a `#` comment), guarded by drift tests (`committed_schema_matches_generated` + `generated_template_parses_to_expected_defaults`). Adding a setting: add a field with `#[serde(default)]` + a `///` doc-comment (which becomes both the schema `description` and the config comment) and thread it through the `_with` seam rather than re-reading the file per refresh tick. Legacy files are upgraded through **two layers**. On load (and via `vct config migrate`, which shares the same pass), `migrate_document` rewrites a standard-`[header]`-table file **in place** — prepends the `#:schema` directive, renames `refresh_interval_secs` → `refresh_interval` (dropping the stale key if both coexist), and moves a top-level `[usage].quota_panels` into the nested `[usage.quota]` (filling in the default quota `refresh_interval`), refreshing the schema comment only on the keys it touches. It is idempotent (returns whether it changed, so `load_in` rewrites at most once) and never overwrites malformed TOML. A read-time `migrate_legacy` shim then backstops any residual legacy form the structural pass deliberately skips (an inline `usage = { ... }` table), so the in-memory `Config` is correct even when the file was not rewritten. This replaced a serde `alias`, which would make serde reject a mid-upgrade file carrying both the old and new name as a duplicate field, which the infallible read then turns into a silent full-config reset.

## Conventions

- **Commit messages** are English-only and follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` / `perf:` / `refactor:` / `style:` / `test:` / `chore:` / `ci:`). The `semantic-pull-request` workflow enforces this on PR titles, and `git-cliff` consumes them for release notes.
- When CLI behavior or flags change, update **all three** READMEs (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) in the same PR — they must stay in sync.
- The wrapper packages under `cli/nodejs/` and `cli/python/` ship the release binaries bundled in a `binaries/<platform>/` directory (populated by the release build) and just spawn the matching one; they're not built from source via `cargo`.
- Test layout follows [Rust Book ch11-03](https://doc.rust-lang.org/book/ch11-03-test-organization.html): unit tests inline in `src/<module>/*.rs` inside `#[cfg(test)] mod tests`; each file under `tests/` compiles to its own binary, and `tests/common/mod.rs` is the shared helper module (`TempHome`, `fixture`). The integration-test split is one-file-per-subsystem:
    - `tests/cli.rs` — `assert_cmd`-driven checks of the built binary, in two groups: a zero-env, zero-network group (version / help / `analysis --path` / flag conflicts / `vct config path`+`show`) and per-child-HOME smoke tests (`usage` / `analysis` batch against an isolated temp HOME seeded with fixtures plus an offline pricing cache)
    - `tests/config.rs` — `config::load_in` / `save_merge_models_in` over a `TempHome`'s `~/.vct`: first-run generated-file creation, comment-preserving writes, legacy `quota_panels` migration, `default_time_range` precedence, and that an existing `version.json` is left untouched (not folded into `config.toml`)
    - `tests/parser.rs` — golden-output comparison against `examples/analysis_result_*.json`, ignoring environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`)
    - `tests/analysis.rs` — `aggregate_sessions_by_model_from_paths` rollup logic over a `TempHome`
    - `tests/usage.rs` — `get_usage_from_paths` aggregation over a `TempHome`
    - `tests/pricing.rs` — `ModelPricingMap` lookup priority + tiered pricing math, plus `fetch_model_pricing_with` fetch/cache against an `httpmock` server
    - `tests/cache.rs` — LRU file cache + pricing cache invalidation
    - `tests/http_mock.rs` — HTTP-layer tests of the public quota fetchers (`call_wham`, `refresh_codex`) against an `httpmock` server
    - `tests/quota.rs` — Codex session-log quota fallback (`latest_session_rate_limits_in`) over a `TempHome` seeded with `codex_session_rate_limits.jsonl`
- **Tests are hermetic: no real external API, no machine-file reads, no ambient env control.** Isolation comes from dependency injection, not `HOME`/`VCT_OFFLINE` mutation: the `*_from_paths` / `resolve_paths_from_home` / `fetch_model_pricing_with` / cache `*_in` seams take an explicit temp dir (via `TempHome` in `tests/common`), and every network call is pointed at a local `httpmock` server through the injected endpoint parameters. The 401 → refresh → retry loop and each provider's send layer are covered by inline `#[cfg(test)]` tests in their source files (which can reach crate-private items). `VCT_OFFLINE` / `network_disabled()` remain a **production** offline feature but no test depends on them, so `cargo test` passes fully offline **without** any env var — the same way CI runs it. The only env used anywhere is a per-child `HOME` on the handful of `assert_cmd` smoke tests (there is no other way to isolate a separate binary's home). Keep tests self-contained (e.g. `clear_pricing_cache()` before asserting on the global match-cache).
- Sample fixtures and golden outputs for the four JSONL providers live in `examples/` (one `test_conversation_<provider>.jsonl` plus one `analysis_result_<provider>.json` per provider). OpenCode and Hermes have no JSONL fixture; their SQLite readers are covered by inline unit tests in `src/session/opencode.rs` / `src/session/hermes.rs` that build a temp database.
