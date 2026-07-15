#!/bin/bash

set -euo pipefail

make

tmp_dir=$(mktemp -d tests/fixtures/sessions/.analysis-results.XXXXXX)
trap 'rm -rf "$tmp_dir"' EXIT

generate_result() {
    local input=$1
    local output=$2
    local temporary
    temporary="$tmp_dir/$(basename "$output")"

    ./target/debug/vibe_coding_tracker analysis "$input" > "$temporary"
    mv "$temporary" "$output"
}

generate_result tests/fixtures/sessions/claude_code.jsonl tests/fixtures/sessions/claude_code.expected.json
generate_result tests/fixtures/sessions/codex.jsonl tests/fixtures/sessions/codex.expected.json
generate_result tests/fixtures/sessions/gemini.jsonl tests/fixtures/sessions/gemini.expected.json
generate_result tests/fixtures/sessions/copilot.jsonl tests/fixtures/sessions/copilot.expected.json
