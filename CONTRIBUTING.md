# Contributing to tc

## Prerequisites

- Rust toolchain (stable)
- **Linux**: `sudo apt-get install libasound2-dev` (ALSA headers for audio)
- **macOS / Windows**: no extra dependencies

## Build

```sh
cargo build --release
```

Binaries are placed in `target/release/`:
- `tc-client` — terminal client
- `tc-server` — relay server

## Project structure

```
crates/
  shared/    # Wire protocol, config constants, TCP framing
  server/    # TLS+TCP control, UDP voice relay, rate limiting
  client/    # TUI, audio capture/playback, voice pipeline, web UI
```

## Run tests

```sh
cargo test --workspace
```

Tests include unit tests across all crates and an end-to-end integration test that spins up a real server with two clients exchanging chat and voice.

## Development server

```sh
# Terminal 1: start server (auto-generates TLS cert)
cargo run --release --bin tc-server

# Terminal 2: connect client
cargo run --release --bin tc-client
```

Server listens on TCP :7100 and UDP :7101 by default. Override with `--tcp-port` and `--udp-port`.

## Code style

- `cargo fmt` before committing
- `cargo clippy --workspace` should pass without warnings
- Comments only for non-obvious logic
- Follow existing patterns: `anyhow::Result`, `tracing` for logging

## Wire protocol

See [PROTOCOL.md](PROTOCOL.md) for the full wire protocol specification.
