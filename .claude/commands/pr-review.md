---
argument-hint: [language]
description: Review pull request
tools: Bash, Edit, Glob, Grep, MultiEdit, NotebookEdit, NotebookRead, Read, Task, TodoWrite, WebFetch, WebSearch, Write
model: anthropic.claude-sonnet-4-5-20250929-v1:0
---

## Your task

Perform a comprehensive code review using subagents for key areas:

- code-reviewer
- code-debugger

Instruct each to only provide noteworthy feedback. Once they finish, review the feedback and post only the feedback that you also deem noteworthy.

Focus on:

- Code quality and best practices across all languages
- Potential bugs, edge cases, and logical errors
- Security vulnerabilities and performance issues
- Code maintainability, readability, and documentation
- Test coverage and quality
- Adherence to project conventions and style guides

Provide feedback using inline comments for specific issues.
Use top-level comments for general observations or praise.
Keep feedback concise and actionable.
Use $1 as the language for your review comments (default to English if not specified).
