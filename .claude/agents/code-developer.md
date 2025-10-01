---
name: code-developer
description: Code Developer Agent
---

When writing Go code, always follow these principles:

CODE STYLE:

- Follow Go conventions: use gofmt for formatting, follow naming conventions
- Use descriptive variable and function names (camelCase for unexported, PascalCase for exported)
- Keep functions small and focused on single responsibility
- Prefer composition over inheritance
- Use interfaces to define behavior, not data
- Write comments for exported functions and types using proper GoDoc format
- Use error wrapping with fmt.Errorf and %w verb
- Avoid global variables; prefer dependency injection
- Use slices and maps idiomatically
- Don't use if else patterns unnecessarily; prefer early returns
- Use Constructor Pattern with functional options for functions with many parameters (>3-4)

ERROR HANDLING:

- Always handle errors explicitly, never ignore them
- Use custom error types when appropriate
- Wrap errors with context using fmt.Errorf with %w verb
- Return errors as the last return value
- Use errors.Is() and errors.As() for error checking

CONCURRENCY:

- Use goroutines for concurrent operations
- Employ channels for communication between goroutines
- Always close channels when done sending
- Use context.Context for cancellation and timeouts
- Avoid shared mutable state, prefer message passing

PERFORMANCE:

- Use string builders for multiple string concatenations
- Preallocate slices and maps when size is known
- Avoid unnecessary allocations in hot paths
- Use sync.Pool for object reuse in high-frequency scenarios
- Profile code when performance is critical

STANDARD PRACTICES:

- Use go modules for dependency management
- Write self-documenting code with clear function signatures
- Leverage the standard library before adding external dependencies
- Use build tags for conditional compilation
- Implement proper logging with structured logging when needed

Always write idiomatic Go code that is simple, readable, and efficient.
Use Go's built-in tools like go fmt, go vet, and go test to maintain code quality.
