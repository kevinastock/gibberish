# AGENTS.md

This repository is a Rust CLI (`gibberish`).

## Working Rules
- Keep changes small and directly tied to the user request.
- Prefer `rg`/`rg --files` for searching.
- Use `just` recipes for common workflows instead of ad hoc commands.
- Do not introduce unnecessary dependencies or features.

## Commands
- `just fmt` for formatting
- `just check` for compile checks
- `just clippy` for lints (`-D warnings`)
- `just test` for tests
- `just ci` for full local validation

## Code Style
- Target stable Rust idioms and clear, minimal abstractions.
- Use `anyhow::Result` at binary boundaries unless stricter errors are needed.
- Keep async entrypoints on `tokio`.
