# Test Structure

This directory contains all tests for the Vibe Coding Tracker project.

## Test Organization

### Integration Tests

**Entry Point:** `integration_tests.rs` (imports `integrations/` module)

**Directory:** `integrations/`

These tests verify end-to-end functionality of the application, excluding TUI components. They test the complete workflow from parsing session files to generating usage reports.

- `analysis_tests.rs` - Single-file and batch analysis operations
- `cache_tests.rs` - LRU file parsing cache and pricing cache
- `cli_tests.rs` - Command-line interface operations
- `parser_tests.rs` - Parsing functionality for all AI assistant formats
- `pricing_tests.rs` - Pricing system functionality
- `usage_tests.rs` - Usage calculation and aggregation logic

**Run all integration tests:**

```bash
cargo test --test integration_tests
```

### Unit Tests

These tests verify individual functions and modules in isolation.

#### Analysis Module Tests

- `test_detector.rs` - AI provider format detection (`src/analysis/detector.rs`)
- `test_common_state.rs` - Common analysis state shared by all analyzers (`src/analysis/common_state.rs`)

#### Utils Module Tests

- `test_utils_file.rs` - File reading and line counting utilities (`src/utils/file.rs`)
- `test_utils_time.rs` - Timestamp parsing utilities (`src/utils/time.rs`)
- `test_utils_paths.rs` - Path resolution and machine identification (`src/utils/paths.rs`)
- `test_utils_directory.rs` - Directory traversal and file filtering (`src/utils/directory.rs`)
- `test_utils_git.rs` - Git remote URL detection (`src/utils/git.rs`)

#### Pricing Module Tests

- `test_pricing_matching.rs` - Model pricing matching logic (`src/pricing/matching.rs`)
- `test_pricing_cache.rs` - Model pricing data structures and serialization (`src/pricing/cache.rs`)

#### Cache Module Tests

- `test_cache_global.rs` - Global cache singleton operations (`src/cache/mod.rs`)

#### Models Module Tests

- `test_models_serialization.rs` - Model structures serialization/deserialization (`src/models/`)

#### Update Module Tests

- `test_update_version.rs` - Version extraction and comparison logic (`src/update/mod.rs`)

**Run all unit tests:**

```bash
cargo test --tests --lib
```

**Run specific unit test:**

```bash
cargo test --test test_detector
cargo test --test test_common_state
cargo test --test test_utils_file
# ... etc
```

## Test Statistics

### Integration Tests

- **Total Tests:** 95
- **Coverage:** All functionality except TUI components

### Unit Tests

- **Total Tests:** 202 (12 test files)
- **Coverage:** Individual functions in core modules

### Library Tests

- **Total Tests:** 26 (in source code)

### Total

- **All Tests:** 323 tests across 15 test suites ✅

## Running Tests

### Run All Tests

```bash
cargo test
```

### Run Tests Quietly

```bash
cargo test --quiet
```

### Run Tests with Output

```bash
cargo test -- --nocapture
```

### Run Tests Sequentially

```bash
cargo test -- --test-threads=1
```

### Run Specific Test Module

```bash
cargo test --test integration_tests integrations::parser_tests
cargo test --test test_detector
```

### Run Specific Test

```bash
cargo test test_claude_code_parser
cargo test test_exact_match
```

## Test Design Principles

1. **Separation of Concerns**: Integration tests verify end-to-end workflows, while unit tests verify individual functions
2. **Independence**: Each test should be able to run independently without relying on other tests
3. **Clarity**: Test names should clearly describe what is being tested
4. **Coverage**: Every public function should have at least one test
5. **No TUI Tests**: Interactive terminal UI components are excluded from automated tests

## Adding New Tests

### For Integration Tests

Add tests to the appropriate file in `tests/integrations/`:

- Analysis operations → `analysis_tests.rs`
- CLI commands → `cli_tests.rs`
- Parsing logic → `parser_tests.rs`
- etc.

### For Unit Tests

Create a new test file or add to existing ones:

1. Name the file `test_<module_name>.rs`
2. Place it in the `tests/` directory
3. Import necessary items from `vibe_coding_tracker::<module>`
4. Write tests for individual functions
5. Run `cargo test --test test_<module_name>` to verify

## CI/CD Integration

All tests are designed to run in CI/CD pipelines:

- No manual interaction required
- Graceful handling of missing test data
- Fast execution (typically < 10 seconds for all tests)
- Parallel-safe (tests don't interfere with each other)
