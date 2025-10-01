---
argument-hint: [folderpath]
description: Update test files to reflect recent code changes
tools: Bash, Edit, Glob, Grep, MultiEdit, NotebookEdit, NotebookRead, Read, Task, TodoWrite, WebFetch, WebSearch, Write
model: anthropic.claude-sonnet-4-5-20250929-v1:0
---

## Your task

Update all test files to reflect recent code changes under $1 (default to current directory if not specified).

Steps:

1. Identify all test files in the specified directory
2. Analyze recent code changes that may affect tests
3. Update test cases, assertions, and mocks as needed
4. Ensure tests follow project testing conventions and best practices
5. Verify tests run successfully after updates

Consider:

- Unit tests, integration tests, and end-to-end tests
- Test naming conventions and structure
- Code coverage for new or modified code
- Edge cases and error scenarios
- Test data and fixtures
