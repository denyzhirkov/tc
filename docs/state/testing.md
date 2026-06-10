# Testing

## Automated

```sh
cargo test --workspace        # unit (co-located) + e2e integration
cargo fmt --check
cargo clippy --workspace      # must be warning-clean
```

Unit coverage of note:
- `client/codec.rs` — Opus encode/decode + PLC for lost packets
- `shared`/framing — length-prefixed frame parse, `BytesMut` pending buffer
- `server/tcp.rs` — `handle_message` routing
- `server/udp.rs` — batch sender
- `client/network.rs` — frame parsing
- `client/main.rs` — input sanitize + conversions

**E2E:** spins up a real `tc-server` and connects two clients that exchange chat and voice over the live TLS/UDP path. This is the load-bearing integration test — keep it green when touching protocol, framing, or the voice handshake.

## Desktop

```sh
pnpm -C apps/desktop build         # tsc + vite (UI typechecks/builds)
pnpm -C apps/desktop tauri build   # full app bundle
```

No automated UI tests yet — desktop changes need a manual smoke (see below).

## Load

`crates/loadtest` is a synthetic-client harness. Run it against a local server before merging server hot-path or rate-limit changes:

```sh
cargo run --release --bin tc-server &
cargo run --release --bin tc-loadtest -- <args>   # see crate --help
```

## Manual smoke (release-worthy changes)

1. `cargo run --release --bin tc-server` (auto-generates TLS cert).
2. `cargo run --release --bin tc-client` → web UI at `http://127.0.0.1:17300`.
3. `/create`, `/connect <id>` from a second client, confirm: voice both ways, mute, chat, peer join/leave, active-speaker indication.
4. Kill the server mid-call → confirm auto-reconnect + auto-rejoin.
5. New/changed cert → confirm TOFU prompts for `/trust`.
6. Desktop: launch the Tauri app, repeat 3–5, check tray/notifications/hotkeys and `tc://` deeplink.
