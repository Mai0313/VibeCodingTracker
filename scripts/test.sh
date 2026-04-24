#!/bin/bash

make

./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_claude_code.jsonl --output examples/analysis_result_claude_code.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_codex.jsonl --output examples/analysis_result_codex.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_gemini.json --output examples/analysis_result_gemini.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_copilot.json --output examples/analysis_result_copilot.json
