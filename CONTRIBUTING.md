# Contributing to Vibe Coding Tracker

First off, thanks for taking the time to contribute!

All types of contributions are encouraged and valued. See the [Table of Contents](#table-of-contents) for different ways to help and details about how this project handles them. Please make sure to read the relevant section before making your contribution. It will make it a lot easier for us maintainers and smooth out the experience for all involved. The community looks forward to your contributions.

## Table of Contents

- [I Have a Question](#i-have-a-question)
- [I Want To Contribute](#i-want-to-contribute)
    - [Reporting Bugs](#reporting-bugs)
    - [Suggesting Enhancements](#suggesting-enhancements)
    - [Your First Code Contribution](#your-first-code-contribution)
    - [Development Guide](#development-guide)
        - [Prerequisites](#prerequisites)
        - [Project Layout](#project-layout)
        - [Building from Source](#building-from-source)
        - [Build Features](#build-features)
        - [Running Tests](#running-tests)
        - [Benchmarks](#benchmarks)
        - [Code Quality](#code-quality)
        - [Pre-commit Hooks](#pre-commit-hooks)
        - [Commit Convention](#commit-convention)
        - [Pull Requests](#pull-requests)
        - [Release & Packaging](#release--packaging)

## I Have a Question

Before you ask a question, it is best to search for existing [Issues](https://github.com/Mai0313/VibeCodingTracker/issues) that might help you. In case you have found a suitable issue and still need clarification, you can write your question in this issue. It is also advisable to search the internet for answers first.

If you then still feel the need to ask a question and need clarification, we recommend the following:

- Open an [Issue](https://github.com/Mai0313/VibeCodingTracker/issues/new).
- Provide as much context as you can about what you're running into.
- Provide project and platform versions (Rust toolchain, OS, Node.js, Python, etc.), depending on what seems relevant.

## I Want To Contribute

### Reporting Bugs

A good bug report shouldn't leave others needing to chase you up for more information. Therefore, we ask you to investigate carefully, collect information and describe the issue in detail in your report.

- Make sure that you are using the latest version (`vct update --check`).
- Determine if your bug is really a bug and not an error on your side, e.g. using incompatible environment components/versions (make sure that you have read the [documentation](README.md)).
- Check if other users have experienced (and potentially already solved) the same issue in the [bug tracker](https://github.com/Mai0313/VibeCodingTracker/issues).
- Include the `vct version --json` output, the exact command you ran, and any relevant JSONL snippet (scrubbed of secrets) so we can reproduce it.

### Suggesting Enhancements

This section guides you through submitting an enhancement suggestion, including completely new features and minor improvements to existing functionality.

- Make sure that you are using the latest version.
- Read the [documentation](README.md) carefully and find out if the functionality is already covered, maybe by an individual configuration.
- Perform a [search](https://github.com/Mai0313/VibeCodingTracker/issues) to see if the enhancement has already been suggested. If it has, add a comment to the existing issue instead of opening a new one.
- Find out whether your idea fits with the scope and aims of the project. It's up to you to make a strong case to convince the project's developers of the merits of this feature.

### Your First Code Contribution

Unsure where to begin contributing? You can start by looking through `good first issue` and `help wanted` issues.

### Development Guide

#### Prerequisites

- [Rust toolchain](https://rustup.rs/) **1.85 or higher** — this project targets the **Rust 2024 edition**. Update with `rustup update` if needed.
- `rustfmt` and `clippy` components (installed by default with `rustup`).
- Optional: [`pre-commit`](https://pre-commit.com/), [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) for coverage, and Docker if you plan to touch the image build.

> [!NOTE]
> `build.rs` embeds the git describe output into the binary as the version string. Building outside a git worktree falls back to the `Cargo.toml` version.

#### Project Layout

```
.
├── benches/          # Criterion benchmarks (pricing, parsing, aggregation)
├── cli/              # npm and PyPI wrapper packages (nodejs/, python/)
├── docker/           # Multi-stage Dockerfile (builder → ubuntu:24.04 prod)
├── examples/         # Sample session files and analysis outputs
├── src/
│   ├── analysis/     # JSONL parsers (claude / codex / copilot / gemini) + detector
│   ├── cache/        # LRU + pricing cache
│   ├── cli.rs        # clap definitions (commands, flags, TimeRange enum)
│   ├── display/      # TUI dashboards, static tables, plain-text renderers (usage rows sorted by cost ascending)
│   ├── pricing/      # LiteLLM fetch, fuzzy model matching, cost calculation
│   ├── update/       # Self-update via GitHub releases (archive extraction)
│   ├── usage/        # Per-provider token aggregation
│   └── utils/        # Path resolution, directory walking, time helpers
└── tests/            # Integration suite + per-module unit tests
```

#### Building from Source

```bash
# 1. Clone the repository
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker

# 2. Debug build (fast iteration)
cargo build

# 3. Release build (recommended for benchmarking / dogfooding)
cargo build --release --locked

# 4. Binary location
./target/release/vibe_coding_tracker --help

# 5. Optional: create a short alias
# Linux/macOS:
sudo ln -sf "$(pwd)/target/release/vibe_coding_tracker" /usr/local/bin/vct

# Or install to user directory (make sure ~/.local/bin is in PATH):
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct
```

Two release profiles are defined in `Cargo.toml`:

- `release` — thin LTO, good default for local release builds.
- `dist` — fat LTO, single codegen unit; used for distribution artifacts. Invoke via `cargo build --profile dist --locked`.

#### Build Features

`Cargo.toml` exposes a small set of optional features; the defaults are tuned for long-running TUI sessions.

- **System allocator (default)** — the build links against glibc's `malloc`. Combined with the `mallopt` tuning applied at startup (see `src/utils/heap.rs`) and the per-refresh `malloc_trim(0)` call, this keeps `usage` / `analysis` TUI RSS roughly flat (~30–50 MB) even over hours of refreshes. Use this for anything you plan to leave open.
- **`mimalloc` (opt-in)** — enable with `cargo build --release --features mimalloc`. Links Microsoft's mimalloc as the global allocator. Startup / one-shot commands (`vct usage --json`, `vct analysis --path file.jsonl`) are slightly faster, but mimalloc's lazy purge retains freed pages — on a 219-session directory the TUI RSS was ~11× higher than the default build in our measurements. Prefer this only for scripted, short-lived invocations.

On Linux/glibc the main binary also calls `mallopt(M_ARENA_MAX, 2)` + `mallopt(M_TRIM_THRESHOLD, 128 KiB)` at start. These cap the number of per-thread allocator arenas (so Rayon workers can't multiply arena-side fragmentation across cores) and pin the trim threshold. The calls are no-ops on other platforms / allocators.

Common Makefile shortcuts (`make help` to list all):

| Target          | Description                                                     |
| --------------- | --------------------------------------------------------------- |
| `make build`    | Debug build (`cargo build`)                                     |
| `make release`  | Locked release build (`cargo build --release --locked`)         |
| `make package`  | `cargo package --locked --allow-dirty`                          |
| `make test`     | Run the full `cargo test --all` suite                           |
| `make fmt`      | `cargo fmt --all` + `cargo clippy --all-targets --all-features` |
| `make coverage` | Install & run `cargo-llvm-cov` for workspace coverage           |
| `make clean`    | Remove build artifacts and prune git objects                    |

#### Running Tests

The tests are organized into three tiers. See [`tests/README.md`](tests/README.md) for the full breakdown.

```bash
# Everything (library + integration + per-module unit tests)
cargo test --all

# Integration tests only (end-to-end, no TUI)
cargo test --test integration_tests

# A specific unit test file
cargo test --test test_detector
cargo test --test test_pricing_matching

# Run a single test by name
cargo test test_exact_match -- --nocapture

# Run sequentially (useful when debugging flaky parallel tests)
cargo test -- --test-threads=1
```

Before opening a PR, please ensure `cargo test --all` passes locally.

#### Benchmarks

Performance-sensitive code paths (pricing lookup, JSONL parsing, aggregation) have Criterion benchmarks in `benches/benchmarks.rs`:

```bash
cargo bench
# Reports are written to target/criterion/*/report/index.html
```

When optimizing, include before/after numbers in the PR description.

#### Code Quality

We use `rustfmt` and `clippy` to ensure code quality. The CI (`.github/workflows/code-quality-check.yml`) runs both with `-D warnings`, so please run them locally before submitting:

```bash
# Format your code
cargo fmt --all

# Check formatting without modifying files (same as CI)
cargo fmt --all -- --check

# Run linting checks (warnings are errors in CI)
cargo clippy --all-targets --all-features -- -D warnings
```

#### Pre-commit Hooks

The repository ships a `.pre-commit-config.yaml` covering whitespace/EOL fixes, JSON/YAML/TOML linting, `mdformat` for Markdown, `gitleaks` for secret scanning, and `shellcheck`. Install once:

```bash
# Install the pre-commit tool itself
pipx install pre-commit            # or: uv tool install pre-commit

# Install the git hooks into .git/hooks/
pre-commit install --install-hooks

# Run against all files (what CI does)
pre-commit run --all-files
```

#### Commit Convention

All commit messages must be written in **English** and follow the [Conventional Commits](https://www.conventionalcommits.org/) specification. The `semantic-pull-request` workflow enforces this on PR titles, and `git-cliff` consumes these prefixes to generate release notes.

Accepted types (see `Cargo.toml` → `package.metadata.git-cliff.git.commit_parsers` for the full list):

- `feat:` — a new feature
- `fix:` — a bug fix
- `docs:` — documentation-only changes
- `perf:` — performance improvement
- `refactor:` — code restructuring without behavior change
- `style:` — formatting / whitespace
- `test:` — adding or adjusting tests
- `chore:` / `ci:` — tooling, dependencies, CI

Example:

```
feat(usage): add --weekly time range filter

Aggregate sessions whose modified date falls within the current ISO week.
```

#### Pull Requests

1. Fork the repository and create a topic branch (`feat/...`, `fix/...`, `docs/...`).
2. Make focused commits following the convention above.
3. Ensure `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all` pass.
4. Update the relevant README files (`README.md`, `README.zh-CN.md`, `README.zh-TW.md`) when behavior or flags change — all three languages should stay in sync.
5. Open the PR against `main`. The title must satisfy the semantic-pull-request check.

#### Release & Packaging

- **Crates.io**: `cargo package --locked --allow-dirty` locally; publishing is automated via `.github/workflows/build_release.yml`.
- **npm / PyPI**: wrapper packages live under `cli/nodejs` and `cli/python`. They download the matching GitHub release binary at install time.
- **Docker**: `docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .` produces an `ubuntu:24.04`-based image that runs the release binary as `ENTRYPOINT`.
