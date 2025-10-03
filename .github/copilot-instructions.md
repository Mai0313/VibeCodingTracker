# Rust Template - Developer Instructions

## Project Overview

This is a production-ready Rust implementation that translates the original Go codebase (`parser.go`) for parsing and analyzing JSONL log files from Claude Code and Codex. The project provides:

- Complete CLI implementation with analysis and usage statistics commands
- Modern Cargo workspace structure with modular design
- Comprehensive CI/CD pipeline with GitHub Actions
- Multi-platform cross-compilation support
- Docker containerization
- Automated testing, linting, and formatting
- Release management with GitHub Releases

### Translation from Go to Rust

This project is a complete Rust translation of the original Go implementation. All core functionality has been preserved and enhanced with Rust's type safety and performance benefits.

## Technical Architecture

### Project Structure
```
codex_usage/
├── src/
│   ├── lib.rs              # Library code
│   ├── main.rs             # Binary entry point
│   ├── cli.rs              # CLI argument parsing
│   ├── models/             # Data models
│   │   ├── mod.rs
│   │   ├── analysis.rs     # Analysis data structures
│   │   ├── usage.rs        # Usage data structures
│   │   ├── claude.rs       # Claude Code log models
│   │   └── codex.rs        # Codex log models
│   ├── analysis/           # Analysis functionality
│   │   ├── mod.rs
│   │   ├── analyzer.rs     # Main analyzer
│   │   ├── claude_analyzer.rs
│   │   ├── codex_analyzer.rs
│   │   └── detector.rs     # Extension type detection
│   ├── usage/              # Usage statistics
│   │   ├── mod.rs
│   │   ├── calculator.rs   # Usage calculation
│   │   └── display.rs      # Usage display formatting
│   └── utils/              # Utility functions
│       ├── mod.rs
│       ├── paths.rs        # Path handling
│       ├── time.rs         # Time parsing
│       ├── file.rs         # File I/O
│       └── git.rs          # Git operations
├── examples/               # Example JSONL files
├── tests/                  # Integration tests
├── docker/
│   └── Dockerfile          # Multi-stage build
├── .github/
│   ├── workflows/          # CI/CD pipelines
│   └── copilot-instructions.md
├── Makefile                # Build automation
└── Cargo.toml              # Dependencies and metadata
```

### Key Dependencies
- **CLI**: clap (v4.5) with derive macros for argument parsing
- **Serialization**: serde, serde_json for JSON handling
- **Error Handling**: anyhow, thiserror for robust error management
- **Time**: chrono for timestamp parsing
- **File System**: walkdir for directory traversal, home for path resolution
- **Regex**: regex for pattern matching in log parsing
- **Logging**: log, env_logger for debugging
- **Development**: clippy, rustfmt for code quality
- **CI/CD**: GitHub Actions with comprehensive workflow matrix

## Development Environment Setup

### Prerequisites
- Rust toolchain (via rustup)
- Cargo package manager
- Git
- Docker (optional, for container builds)
- Make (optional, for convenience commands)

### Installation
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and setup project
git clone <repository-url>
cd codex_usage
cargo build
```

### Development Commands
```bash
# Format and lint code
make fmt           # rustfmt + clippy
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings

# Testing
make test          # Run all tests with verbose output
cargo test --verbose

# Building
make build         # Debug build
make release       # Release build
cargo build --release

# Running
make run           # Run release binary
cargo run --release

# Packaging
make package       # Create .crate package
cargo package --locked --allow-dirty
```

## CLI Commands

### Analysis Command

Analyzes JSONL conversation files from Claude Code or Codex:

```bash
# Analyze and output to stdout
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl

# Analyze and save to file
./target/debug/codex_usage analysis --path examples/test_conversation.jsonl --output result.json
```

### Usage Command

Displays token usage statistics from Claude Code and Codex sessions:

```bash
# Display in table format
./target/debug/codex_usage usage

# Display in JSON format
./target/debug/codex_usage usage --json
```

### Version Command

Displays version information:

```bash
./target/debug/codex_usage version
```

## Go to Rust Function Mapping

For developers familiar with the original Go implementation, here's the mapping between Go functions and their Rust equivalents:

| Go Function | Rust Implementation | Module | Description |
|-------------|---------------------|--------|-------------|
| `analyzeConversations` | `analyze_claude_conversations` | `analysis::claude_analyzer` | Analyzes Claude Code conversation logs |
| `analyzeCodexConversations` | `analyze_codex_conversations` | `analysis::codex_analyzer` | Analyzes Codex conversation logs |
| `CalculateUsageFromJSONL` | `calculate_usage_from_jsonl` | `usage::calculator` | Calculates usage statistics from a single JSONL file |
| `GetUsageFromDirectories` | `get_usage_from_directories` | `usage::calculator` | Aggregates usage statistics from multiple session directories |
| `ReadJSONL` | `read_jsonl` | `utils::file` | Reads and parses JSONL files |
| `parseISOTimestamp` | `parse_iso_timestamp` | `utils::time` | Parses ISO timestamp strings to Unix milliseconds |
| `getGitRemoteOriginURL` | `get_git_remote_url` | `utils::git` | Extracts Git remote origin URL from repository |
| `detectExtensionType` | `detect_extension_type` | `analysis::detector` | Auto-detects whether logs are from Claude Code or Codex |
| `processClaudeUsageData` | `process_claude_usage` | `analysis::claude_analyzer` | Processes Claude usage statistics |
| `processCodexUsageData` | `process_codex_usage` | `analysis::codex_analyzer` | Processes Codex usage statistics |
| `displayStaticTable` | `display_usage_table` | `usage::display` | Displays usage statistics in table format |

### Type Mappings

| Go Type | Rust Type | Notes |
|---------|-----------|-------|
| `map[string]interface{}` | `HashMap<String, Value>` | JSON value maps using serde_json::Value |
| `[]map[string]interface{}` | `Vec<Value>` | Array of JSON objects |
| `string` | `String` | Owned strings |
| `int64` | `i64` | 64-bit integers |
| `time.Time` | `i64` (Unix millis) | Timestamps stored as milliseconds |
| `error` | `Result<T, anyhow::Error>` | Error handling using Result type |

## Implementation Highlights

### Features Preserved from Go

1. **Analysis Functionality**: Complete analysis of both Claude Code and Codex logs
2. **Usage Calculation**: Token usage statistics with date-based aggregation
3. **Path Handling**: Automatic path resolution and normalization
4. **Git Integration**: Extraction of Git remote URLs from repositories
5. **Format Detection**: Automatic detection of log source (Claude Code vs Codex)

### Rust-Specific Enhancements

1. **Type Safety**: Strong typing eliminates entire classes of runtime errors
2. **Error Handling**: Comprehensive error handling with `anyhow` and `thiserror`
3. **Performance**: Zero-cost abstractions and optimized release builds
4. **Memory Safety**: No null pointers, guaranteed memory safety without garbage collection
5. **Modular Design**: Clean separation of concerns with module system

### Development Workflow
- **After every edit**: Run `cargo build` to confirm compilation is successful before proceeding
- **Before committing**: Ensure code passes all quality checks (fmt, clippy, test)
- **Before pushing**: Run full test suite to catch any integration issues

## Build and Release Process

### Local Development Build
```bash
cargo build --release --locked
```

### Cross-Platform Builds
The CI/CD pipeline supports building for multiple target architectures:

**Supported Targets:**
- `x86_64-unknown-linux-gnu` - Linux x86_64 (glibc)
- `x86_64-unknown-linux-musl` - Linux x86_64 (musl, static)
- `aarch64-unknown-linux-gnu` - Linux ARM64 (glibc)
- `aarch64-unknown-linux-musl` - Linux ARM64 (musl, static)
- `x86_64-apple-darwin` - macOS Intel
- `aarch64-apple-darwin` - macOS Apple Silicon
- `x86_64-pc-windows-msvc` - Windows x86_64
- `aarch64-pc-windows-msvc` - Windows ARM64

### Release Process
1. **Create Release Tag**: `git tag -a v1.0.0 -m "Release v1.0.0"`
2. **Push Tag**: `git push origin v1.0.0`
3. **CI/CD Triggers**: `build_release.yml` workflow automatically:
   - Builds binaries for all supported platforms
   - Creates compressed archives (.tar.gz for Unix, .zip for Windows)
   - Uploads assets to GitHub Release

### Asset Naming Convention
- Unix platforms: `{binary-name}-v{version}-{target}.tar.gz`
- Windows: `{binary-name}-v{version}-{target}.zip`

Example: `codex_usage-v1.0.0-x86_64-unknown-linux-gnu.tar.gz`

## CI/CD Workflows

### Core Workflows
1. **test.yml**: Comprehensive testing with coverage reports
2. **code-quality-check.yml**: Code formatting and linting validation
3. **build_package.yml**: Cargo package building and optional crates.io publishing
4. **build_image.yml**: Docker image building and pushing to GHCR
5. **build_release.yml**: Cross-platform binary releases

### Automation Features
- **Auto-labeling**: PRs labeled based on file changes and branch names
- **Security scanning**: Multi-layer security analysis (secrets, vulnerabilities, code quality)
- **Release drafting**: Automated changelog generation
- **Semantic PR validation**: Enforces conventional commit format
- **Dependency updates**: Weekly automated dependency updates via Dependabot

## Code Quality Standards

### Rust Code Guidelines
- Use `rustfmt` for consistent formatting
- Enable all clippy warnings as errors (`-D warnings`)
- Follow Rust API guidelines
- Write comprehensive documentation comments
- Include unit tests for all public functions

### Commit Conventions
Follow Conventional Commits format:
```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`

### Pull Request Requirements
- All CI checks must pass
- Code review required
- Conventional commit title format
- Updated documentation if needed
- Tests added for new features

## Testing

This project includes a comprehensive test suite with 50+ tests covering all core functionality.

### Test Structure

**Unit Tests** (`tests/test_*.rs`):
- `test_utils_time.rs` - Time parsing and timestamp handling (6 tests)
- `test_utils_paths.rs` - Path resolution and user/machine ID (4 tests)
- `test_utils_file.rs` - File I/O, JSONL reading, JSON saving (10 tests)
- `test_utils_git.rs` - Git remote URL extraction (4 tests)
- `test_models.rs` - Data model serialization/deserialization (12 tests)
- `test_analysis_detector.rs` - Extension type detection (6 tests)

**Integration Tests** (`tests/test_integration_*.rs`):
- `test_integration_analysis.rs` - Complete analysis workflows (7 tests)
- `test_integration_usage.rs` - Usage statistics calculation (6 tests)

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test module
cargo test test_utils_time

# Run with output显示
cargo test -- --nocapture

# Verbose mode
cargo test --verbose

# Run single test
cargo test test_parse_iso_timestamp
```

### Test Coverage

- **Total Tests**: 50+
- **Unit Tests**: 40+
- **Integration Tests**: 13
- **Module Coverage**: utils, models, analysis, usage
- **All Tests Pass**: ✅

### Test Dependencies

- `tempfile` (3.15) - Temporary file handling for tests

## Cross-Platform Considerations

### Binary Naming
- Unix systems: Binary name matches Cargo package name
- Windows: Binary includes `.exe` extension
- CI/CD handles platform-specific naming automatically

### Archive Creation
- **Unix platforms**: `.tar.gz` archives containing the binary
- **Windows**: `.zip` archives containing the `.exe` file
- Archives exclude debug symbols and unnecessary files

### Platform-Specific Dependencies
- Linux MUSL targets require `musl-dev` package in Alpine containers for static linking (includes crti.o and other linking libraries)
- macOS builds work on both Intel and Apple Silicon
- Windows builds use MSVC toolchain

## Troubleshooting

### Common Build Issues

**MUSL builds failing on Ubuntu:**
```bash
sudo apt install -y musl-tools pkg-config
```

**Cross-compilation locally:**
Install cross-compilation tools or use zig as linker.

**Permission issues:**
Ensure CI has appropriate permissions for releases and package publishing.

### Performance Optimization
- Use release builds for production
- Enable link-time optimization (LTO) in Cargo.toml for smaller binaries
- Consider stripping debug symbols for distribution

## Security Considerations

### CI/CD Security
- Use GitHub's built-in secret scanning
- Rotate tokens and keys regularly
- Limit workflow permissions to minimum required
- Use Dependabot for automated security updates

### Code Security
- Run clippy with security lints enabled
- Use safe Rust practices
- Audit dependencies regularly
- Follow Rust security advisories

## Deployment

### Docker Deployment
```bash
# Build production image
docker build -f docker/Dockerfile --target prod -t your-app:latest .

# Run container
docker run --rm your-app:latest
```

### Binary Distribution
Download platform-specific binaries from GitHub Releases and deploy directly to target systems.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make changes following code standards
4. Add tests for new functionality
5. Ensure all CI checks pass
6. Submit a pull request with conventional commit format

## Additional Resources

- [Rust Documentation](https://doc.rust-lang.org/)
- [Cargo Book](https://doc.rust-lang.org/cargo/)
- [GitHub Actions Documentation](https://docs.github.com/en/actions)
- [Conventional Commits](https://conventionalcommits.org/)
