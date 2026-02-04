# Vibe Coding Tracker

## Project Overview

**Vibe Coding Tracker** is a high-performance CLI tool designed to track and analyze AI coding assistant usage and costs in real-time. It supports multiple providers including Claude Code, Codex, GitHub Copilot, and Gemini.

**Key Features:**

- **Real-time Cost Tracking:** Uses [LiteLLM](https://github.com/BerriAI/litellm) for up-to-date pricing.
- **Multi-Provider Support:** Automatically detects logs from Claude, Codex, Copilot, and Gemini.
- **Visualization:** Offers interactive TUI dashboards, static tables, and JSON output.
- **Performance:** Built in Rust for speed and efficiency, using `mimalloc` for memory management.

## Tech Stack

- **Language:** Rust (2024 edition)
- **CLI Framework:** `clap`
- **TUI:** `ratatui`, `crossterm`, `comfy-table`
- **Data Handling:** `serde`, `serde_json`, `walkdir`
- **Networking:** `reqwest` (blocking)
- **Wrappers:** Node.js (`cli/nodejs`) and Python (`cli/python`) for distribution.

## Building and Running

The project uses `cargo` for building and testing, with a `Makefile` for convenience.

### Prerequisites

- Rust toolchain (1.85+)

### Common Commands

| Task                | Command                                     | Description                                                              |
| :------------------ | :------------------------------------------ | :----------------------------------------------------------------------- |
| **Build (Release)** | `cargo build --release`                     | Compiles the binary in release mode.                                     |
| **Run**             | `cargo run --release -- <COMMAND>`          | Runs the application. Replace `<COMMAND>` with `usage`, `analysis`, etc. |
| **Test**            | `cargo test --all`                          | Runs all unit and integration tests.                                     |
| **Format**          | `cargo fmt --all`                           | Formats code using `rustfmt`.                                            |
| **Lint**            | `cargo clippy --all-targets --all-features` | Runs `clippy` linter.                                                    |
| **Clean**           | `make clean`                                | Removes build artifacts and caches.                                      |

### Using the Makefile

- `make build`: Build release binary.
- `make test`: Run all tests.
- `make fmt`: Format and lint code.
- `make run`: Run the application (release mode).

## Project Structure

- `src/`: Source code.
    - `main.rs`: Entry point.
    - `lib.rs`: Library definition.
    - `cli.rs`: Command-line argument parsing.
    - `analysis/`: Logic for parsing conversation files (JSONL).
    - `usage/`: Logic for aggregating usage from log directories.
    - `pricing/`: Pricing fetcher and calculator.
    - `display/`: TUI and output formatting.
    - `models/`: Data structures for different providers.
- `cli/`: Wrappers for other package managers.
    - `nodejs/`: NPM package wrapper.
    - `python/`: PyPI package wrapper.
- `tests/`: Integration tests.
- `benches/`: Benchmarks.
- `examples/`: Sample data for testing analysis.

## Development Conventions

- **Commit Messages:** Follow [Conventional Commits](https://www.conventionalcommits.org/). This is used to generate the changelog via `git-cliff`.
- **Allocator:** The project uses `mimalloc` as the global allocator.
- **Optimization:** Release profiles in `Cargo.toml` are configured for maximum optimization (`opt-level = 3`, `lto = "thin"`, `strip = "symbols"`).
- **Blocking I/O:** The application currently uses synchronous I/O (`reqwest::blocking`), so async/await is not prevalent in the main flow.
