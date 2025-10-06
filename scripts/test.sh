#!/bin/bash

make

./target/debug/vibe_coding_tracker analysis --path examples/test_conversation.jsonl --output examples/analysis_result.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_oai.jsonl --output examples/analysis_result_oai.json
./target/debug/vibe_coding_tracker analysis --path examples/test_conversation_gemini.json --output examples/analysis_result_gemini.json
