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
- `server/udp.rs` — **behavioral `BatchSender`** fan-out over loopback: exercises the real per-OS send path (`sendmmsg`/`sendmsg_x`/`try_send_to`) + cached-layout reuse. The only test that actually runs the platform syscall, so it must run on each OS.
- `server/state.rs` — UDP registration incl. canonical-IP (v4-mapped IPv6) match
- `client/voice.rs` — re-hello loop, jitter buffer, seq wrap
- `client/network.rs` — frame parsing
- `client/main.rs` — input sanitize + conversions

**E2E** (`server/tests/e2e.rs`): spins up a real `tc-server` and connects two clients that exchange chat and voice over the live TLS/UDP path. Parametrized by bind IP — runs over **both `127.0.0.1` and `::1`** (IPv6 skips gracefully if the host lacks v6 loopback). The IPv6 run guards the dual-stack relay path where the one-way-audio bug lived. Load-bearing — keep green when touching protocol, framing, or the voice handshake.

## Cross-platform CI

`cargo test --workspace --exclude tc-desktop` runs on **ubuntu + macOS + windows** — not just Linux. This is what catches platform regressions in code that only *compiles* on a foreign OS:
- `Release` workflow: `test` matrix (3 OS) gates the build/release jobs.
- `Desktop CI` (push/PR): `check` (ubuntu, frontend + full workspace incl. `tc-desktop`) + `test-core` (macOS + windows core crates).
- Both matrix jobs set `CMAKE_POLICY_VERSION_MINIMUM=3.5` — Opus (`audiopus_sys`) vendors a CMake build the runners' newer CMake otherwise rejects.

Not CI-covered (manual only): cpal audio capture/playback (no devices on runners), live NAT/firewall timeouts, real Windows `WSAECONNRESET`.

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
