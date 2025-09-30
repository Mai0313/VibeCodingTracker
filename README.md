<center>

# CodexUsage — Codex & Claude Code Telemetry Parser

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](LICENSE)

</center>

CodexUsage parses JSONL event logs from Codex and Claude Code, producing aggregated CodeAnalysis JSON plus optional debug artifacts.

Other Languages: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## What It Does

- Parse JSONL logs (one event per line) from Codex and Claude Code
- Detect source automatically and normalize paths across sessions
- Aggregate read/write/edit/command usage, tool call counts, and conversation token usage
- Output a single CodeAnalysis record (JSON) and optional debug files

Scope focuses on data extraction, summarization, and file handling. Transport of results (e.g., SendAnalysisData) is out of scope.

## Quick Start

Prerequisites: Rust toolchain (rustup), Docker optional

```bash
make fmt            # rustfmt + clippy
make test           # cargo test (verbose)
make build          # cargo build
make build-release  # cargo build --release
make run            # run the release binary
make package        # build .crate package
```

## Docker

```bash
docker build -f docker/Dockerfile --target prod -t ghcr.io/<owner>/<repo>:latest .
docker run --rm ghcr.io/<owner>/<repo>:latest
```

Binary image tag (current placeholder):
```bash
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .
docker run --rm codex_usage:latest
```

## Naming Placeholders

- Crate/binary: `codex_usage`
- Repository links: `https://github.com/<owner>/codex-usage` placeholders
- CI workflows may assume a specific binary name; we will align them after the final project name is chosen

## License

MIT — see `LICENSE`.
