---
argument-hint: [command-name]
description: Generate a new command for Claude Code CLI
tools: Bash, Edit, Glob, Grep, MultiEdit, NotebookEdit, NotebookRead, Read, Task, TodoWrite, WebFetch, WebSearch, Write
model: anthropic.claude-sonnet-4-5-20250929-v1:0
---

## Your task

Your task is to create a new command for the Claude Code CLI tool.

- Use $1 as the command name, if $1 is not provided, you can decide a name for it.
- Create a markdown file in the `.claude/commands/<command-name>.md`.
- You are allowed to visit those resources for reference:
  - For Slash Commands:
    - `./.claude/commands`
    - [Slash Commands](https://docs.claude.com/en/docs/claude-code/slash-commands)
  - For Subagents:
    - `./.claude/agents`
    - [Subagents](https://docs.claude.com/en/docs/claude-code/sub-agents)
- Use `Subagents` only if the command is complex and requires multiple steps or expertise.
- Ensure the command has a clear description, lists relevant tools, and specifies the model to be used.
