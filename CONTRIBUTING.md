# Contributing to Ghost Complete

Thanks for your interest in contributing! Here's how to get started.

## Prerequisites

- **Rust 1.75+** (install via [rustup](https://rustup.rs))
- **macOS** (the PTY proxy uses macOS-specific APIs)
- **Ghostty** (for manual testing)

## Building & Testing

```bash
cargo build                           # Debug build
cargo build --release                 # Release build
cargo test                            # Run all workspace tests
cargo test -p gc-pty                  # Run tests for a single crate
cargo clippy --all-targets            # Lint (must pass with no warnings)
cargo fmt --check                     # Check formatting
cargo fmt                             # Auto-format
cargo bench                           # Run all Criterion benchmarks
cargo bench -p gc-suggest             # Run suggest benchmarks only
```

## Running locally

```bash
cargo run                             # Wraps your default shell
cargo run -- /bin/zsh                 # Specify a shell
cargo run -- --log-level debug        # Enable debug logging
```

## Architecture

The project is a Rust workspace with 7 crates under `crates/`. See [docs/IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) for the full architecture.

The short version: Ghost Complete is a PTY proxy. Keystrokes flow in through stdin, get intercepted for popup navigation, or forwarded to the shell. Shell output flows through a VT parser for state tracking, then to the terminal. Suggestions are computed on trigger conditions and rendered as ANSI popups.

## Writing Completion Specs

Completion specs are Fig-compatible JSON files in `specs/`. Specs can define static subcommands/options and dynamic generators (shell commands with transform pipelines). See [docs/COMPLETION_SPEC.md](docs/COMPLETION_SPEC.md) for the full format including generators, transforms, and caching.

To validate specs:

```bash
ghost-complete validate-specs
```

## Running Benchmarks

Criterion benchmarks exist for `gc-suggest` and `gc-parser`:

```bash
cargo bench                                  # Run all benchmarks
cargo bench -p gc-suggest -- fuzzy_ranking   # Run specific group
cargo bench -- --save-baseline before        # Save baseline
cargo bench -- --baseline before             # Compare against baseline
```

Reports are generated at `target/criterion/report/index.html`.

## Commit Conventions

- Use imperative mood: "fix popup drift" not "fixed popup drift"
- Keep the first line under 72 characters
- Reference issue numbers where applicable

## Pull Request Process

1. Fork the repo and create a branch from `master`
2. Make your changes
3. Ensure `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` all pass
4. Open a PR against `master`
5. Fill out the PR template

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be respectful.
