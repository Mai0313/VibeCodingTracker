# Test Structure

Tests for the Vibe Coding Tracker project are split into two tiers following the standard Rust convention.

## Unit Tests (inline with code, under `src/`)

Per-module unit tests live in `#[cfg(test)] mod tests` blocks inside the source file they cover. Keeping them co-located lets the tests reach private items directly and keeps the test layout mirroring the source tree.

Examples of where to find them:

| Module                         | Tests inside                                                           |
| ------------------------------ | ---------------------------------------------------------------------- |
| `src/analysis/detector.rs`     | Provider-format auto-detection (ClaudeCode / Codex / Copilot / Gemini) |
| `src/analysis/common_state.rs` | Shared analyzer state / path normalization / record conversion         |
| `src/cache/mod.rs`             | Global file-parse cache singleton behaviour                            |
| `src/cache/file_cache.rs`      | LRU eviction + entry invalidation                                      |
| `src/models/analysis.rs`       | `CodeAnalysis*` (de)serialization + camel/PascalCase round-trips       |
| `src/pricing/matching.rs`      | Model-name matching (exact / normalized / substring / fuzzy)           |
| `src/pricing/cache.rs`         | LiteLLM payload parsing + `ModelPricing` round-trips                   |
| `src/update/mod.rs`            | `extract_semver_version` parsing                                       |
| `src/utils/*.rs`               | file I/O, timestamps, paths, directory walking, git remote lookup      |

Run them all with:

```bash
cargo test --lib
```

Target a single module:

```bash
cargo test --lib pricing::matching
cargo test --lib utils::directory
```

## Integration Tests (`tests/integrations/`)

End-to-end tests that drive the public API / CLI live under `tests/integrations/` and share `tests/integration_tests.rs` as the single entry point binary. See [`integrations/README.md`](integrations/README.md) for the per-file breakdown (parser / usage / analysis / cache / pricing / cli tests).

Run them with:

```bash
cargo test --test integration_tests
```

## Everything

```bash
cargo test --all
```

## Design Principles

1. **Unit tests stay beside their module.** The `tests/test_*.rs` layout is reserved for end-to-end coverage.
2. **No TUI tests.** Interactive terminal UI is excluded from automated tests.
3. **Environment independence.** Tests ignore environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`) and handle missing fixture files gracefully.
4. **Parallel-safe.** Tests that touch shared global state (`global_cache`, pricing LRU) use `#[serial(...)]` so they are not affected by parallel execution order.

## Adding New Tests

- **Unit tests**: add them to the `#[cfg(test)] mod tests` block inside the source file under test. Create the block if the file does not have one yet.
- **Integration tests**: pick the appropriate file under `tests/integrations/` (analysis / cache / cli / parser / pricing / usage) and add the test there.
