# Test Structure

This directory holds the project's integration test suite. Unit tests live next to the
code they cover, inside `#[cfg(test)] mod tests` blocks in `src/`. This split follows the
Rust Book's test organization guidance
([ch11-03](https://doc.rust-lang.org/book/ch11-03-test-organization.html)):

- **Unit tests** exercise individual functions in isolation and can reach private items.
  They belong with their module inside `src/`.
- **Integration tests** exercise the library through its public API — parsing real session
  files, driving the CLI, round-tripping the cache. They live here, in `tests/`.

## Layout

```
tests/
├── integration_tests.rs        # Cargo entry point (declares `mod integrations;`)
├── integrations/               # Shared test crate (one binary, several modules)
│   ├── mod.rs
│   ├── analysis_tests.rs       # Single-file + batch analysis across every provider
│   ├── cache_tests.rs          # LRU file cache + pricing cache round-trip behavior
│   ├── cli_tests.rs            # `vct` subcommands via `assert_cmd`
│   ├── parser_tests.rs         # Parser output vs. fixtures in `examples/`
│   ├── pricing_tests.rs        # Pricing fetch, match, and cost calculation
│   └── usage_tests.rs          # Usage aggregation / date grouping / output shapes
└── README.md                   # (this file)
```

Everything under `tests/integrations/` is pulled in by `integration_tests.rs`, so Cargo
compiles a single integration binary rather than one per file. Adding a new module means
creating `tests/integrations/<name>.rs` and declaring `pub mod <name>;` in
`integrations/mod.rs`.

## Running tests

```bash
# Everything: library unit tests + integration tests + doctests
cargo test --all

# Only the integration binary
cargo test --test integration_tests

# One integration module
cargo test --test integration_tests integrations::parser_tests

# One unit-test module (lives under src/ — test path mirrors the module path)
cargo test --lib analysis::detector
cargo test --lib utils::paths
cargo test --lib pricing::matching

# One test function by name (works across all binaries)
cargo test test_exact_match -- --nocapture

# Force sequential execution (debug flaky races between global-state tests)
cargo test -- --test-threads=1
```

## Writing new tests

- **Testing a single function or struct?** Add a `#[test]` inside (or next to) that
  module's `#[cfg(test)] mod tests` block. No new files, no new test crates.
  Private items are visible via `use super::*;`.
- **Testing end-to-end behavior through the public API?** Add the test to the appropriate
  module under `tests/integrations/`:
  - Analysis workflows → `analysis_tests.rs`
  - CLI subcommands → `cli_tests.rs`
  - Parsers (any provider) → `parser_tests.rs`
  - Pricing logic → `pricing_tests.rs`
  - Usage aggregation → `usage_tests.rs`
  - Caching (file parse cache, pricing cache) → `cache_tests.rs`
- Ignore environment-specific fields (`insightsVersion`, `machineId`, `user`,
  `gitRemoteUrl`) when comparing analyzer output against fixtures — `parser_tests.rs`
  has a `compare_json_ignore_fields` helper that does this.
- Tests that touch `global_cache()` must be `#[serial(global_cache)]` (see
  `serial_test`) so the singleton isn't clobbered by a parallel test.
- TUI rendering code is intentionally not covered by automated tests.
