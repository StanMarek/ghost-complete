# Ghost Complete

A terminal-native autocomplete engine using PTY proxying, built in Rust.

Personal tool, built for Ghostty. Inspired by Fig (RIP).

See [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) for the full design.

## Quick Start

```bash
cargo build --release
./target/release/ghost-complete        # wraps your default shell
./target/release/ghost-complete -- /bin/zsh  # specify a shell
```

## License

MIT
