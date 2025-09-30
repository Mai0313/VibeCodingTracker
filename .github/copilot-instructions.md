# Rust Template - Developer Instructions

## Project Overview

This is a production-ready Rust project template designed to bootstrap new Rust applications quickly. The template provides:

- Modern Cargo workspace structure
- Comprehensive CI/CD pipeline with GitHub Actions
- Multi-platform cross-compilation support
- Docker containerization
- Automated testing, linting, and formatting
- Release management with GitHub Releases

## Technical Architecture

### Project Structure
```
codex_usage/
├── src/
│   ├── lib.rs          # Library code
│   └── main.rs         # Binary entry point
├── tests/              # Integration tests
├── docker/
│   └── Dockerfile      # Multi-stage build
├── .github/
│   ├── workflows/      # CI/CD pipelines
│   └── copilot-instructions.md
├── Makefile            # Build automation
└── Cargo.toml          # Dependencies and metadata
```

### Key Dependencies
- **Runtime**: Standard Rust libraries
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
make build-release # Release build
cargo build --release

# Running
make run           # Run release binary
cargo run --release

# Packaging
make package       # Create .crate package
cargo package --locked --allow-dirty
```

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
- Linux MUSL targets require `musl-tools` for static linking
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
