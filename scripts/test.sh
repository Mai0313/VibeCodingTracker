#!/bin/bash

set -euo pipefail

make

tmp_dir=$(mktemp -d examples/.analysis-results.XXXXXX)
trap 'rm -rf "$tmp_dir"' EXIT

generate_result() {
    local input=$1
    local output=$2
    local temporary
    temporary="$tmp_dir/$(basename "$output")"

    ./target/debug/vibe_coding_tracker analysis "$input" > "$temporary"
    mv "$temporary" "$output"
}

generate_result examples/test_conversation_claude_code.jsonl examples/analysis_result_claude_code.json
generate_result examples/test_conversation_codex.jsonl examples/analysis_result_codex.json
generate_result examples/test_conversation_gemini.jsonl examples/analysis_result_gemini.json
generate_result examples/test_conversation_copilot.jsonl examples/analysis_result_copilot.json
