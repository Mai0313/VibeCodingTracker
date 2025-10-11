# Integration Tests

This directory contains comprehensive integration tests for Vibe Coding Tracker, covering all functionality except TUI components.

## Test Coverage

### 1. Parser Tests (`parser_tests.rs`)

Tests the parsing functionality for all supported AI assistant formats:

- **Claude Code Parser**: Validates parsing of Claude Code session files (`.jsonl`)
- **Codex Parser**: Validates parsing of Codex/OpenAI session files (`.jsonl`)
- **Copilot Parser**: Validates parsing of GitHub Copilot CLI session files (`.json`)
- **Gemini Parser**: Validates parsing of Gemini session files (`.json`)

Each parser test compares the actual output against expected results from example files, ignoring environment-specific fields (`insightsVersion`, `machineId`, `user`, `gitRemoteUrl`).

**Example Test Files**:

- `examples/test_conversation.jsonl` → `examples/analysis_result.json`
- `examples/test_conversation_oai.jsonl` → `examples/analysis_result_oai.json`
- `examples/test_conversation_copilot.json` → `examples/analysis_result_copilot.json`
- `examples/test_conversation_gemini.json` → `examples/analysis_result_gemini.json`

### 2. Usage Tests (`usage_tests.rs`)

Tests the usage calculation and aggregation logic:

- Usage data structure and serialization
- Date-based aggregation
- Cost calculation accuracy
- Multiple models handling
- JSON output format validation
- Date sorting and formatting

### 3. Analysis Tests (`analysis_tests.rs`)

Tests both single-file and batch analysis operations:

- Single file analysis for all formats (Claude, Codex, Copilot, Gemini)
- Conversation usage extraction
- Tool call counts tracking
- File operations tracking (edit, read, write)
- Batch analysis with sorting
- Provider-grouped analysis
- Edge cases (empty files, invalid JSON)

### 4. Cache Tests (`cache_tests.rs`)

Tests the LRU file parsing cache and pricing cache:

- Basic cache operations
- Cache hit/miss behavior
- Cache invalidation on file modification
- Cache statistics and memory estimation
- Concurrent cache access
- LRU eviction behavior
- Arc-based data sharing

### 5. Pricing Tests (`pricing_tests.rs`)

Tests the pricing system functionality:

- Model pricing fetch and caching
- Exact, normalized, substring, and fuzzy matching
- Cost calculation with various token configurations
- Pricing data serialization
- Cache expiration (24-hour TTL)
- Edge cases (empty strings, special characters)

### 6. CLI Tests (`cli_tests.rs`)

Tests command-line interface operations:

- Version command (text, JSON output)
- Help commands for all subcommands
- Analysis command (single file, batch, provider-grouped)
- Usage command (JSON, text, table output)
- Update check command
- Output file creation
- Error handling (invalid commands, nonexistent files)
- Unicode and spaces in file paths

## Running Tests

### Run All Integration Tests

```bash
cargo test --test integration_tests
```

### Run With Single Thread (Sequential)

```bash
cargo test --test integration_tests -- --test-threads=1
```

### Run Specific Test Module

```bash
cargo test --test integration_tests integrations::parser_tests
cargo test --test integration_tests integrations::usage_tests
cargo test --test integration_tests integrations::analysis_tests
cargo test --test integration_tests integrations::cache_tests
cargo test --test integration_tests integrations::pricing_tests
cargo test --test integration_tests integrations::cli_tests
```

### Run Specific Test

```bash
cargo test --test integration_tests test_claude_code_parser
```

### Run With Verbose Output

```bash
cargo test --test integration_tests -- --nocapture
```

## Test Statistics

- **Total Tests**: 95
- **Parser Tests**: 8 (4 parsers + 4 helper tests)
- **Usage Tests**: 9
- **Analysis Tests**: 17
- **Cache Tests**: 17
- **Pricing Tests**: 21
- **CLI Tests**: 23

## Test Design Principles

1. **No TUI Tests**: Interactive terminal UI components are excluded from integration tests
2. **Environment Independence**: Tests ignore environment-specific fields that vary between systems
3. **Graceful Degradation**: Tests handle missing example files and empty directories gracefully
4. **Parallel Safety**: Tests can run concurrently without interference (cache tests verify this)
5. **Comprehensive Coverage**: Tests cover success paths, error paths, and edge cases

## Adding New Tests

When adding new tests to this suite:

1. Choose the appropriate test file based on functionality
2. Follow existing naming conventions (`test_<functionality>_<scenario>`)
3. Add documentation comments explaining what the test verifies
4. Ensure tests clean up after themselves (temporary files, cache state)
5. Handle missing dependencies gracefully (skip tests if example files don't exist)

## CI/CD Integration

These tests are designed to run in CI/CD pipelines:

- All tests pass on clean checkout
- No network dependencies (except pricing tests, which handle failures gracefully)
- Minimal system state requirements
- Fast execution (typically < 10 seconds total)
