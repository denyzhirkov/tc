# `tc` — Stack & Contracts

Source of truth for tooling decisions, wire contracts, and the security/resilience baseline. `CLAUDE.md` summarizes; this file is the detail. `PROTOCOL.md` (repo root) is the canonical wire-format spec — this file references it, never contradicts it.

---

## 1. Tech decisions

| Layer | Choice | Why / constraint |
|---|---|---|
| Language (core) | Rust 2021, workspace | One core, three processes. |
| Async runtime | `tokio` (full) | Single runtime everywhere. |
| Control serialization | `bincode` over length-prefixed frames | Compact, schema-from-types. |
| Control transport | TCP + `tokio-rustls` (TLS 1.3, `ring`) | TOFU pinning; no CA infra. |
| Voice transport | UDP, custom binary | Latency over reliability. |
| Voice crypto | `chacha20poly1305` (XChaCha20-Poly1305) | AEAD, 24-byte nonce, per-channel key. |
| Identity | `ed25519-dalek` | Per-user keypair, fingerprint shown in UI. |
| Codec | `audiopus` (Opus) | 48 kHz mono, 20 ms frames. HD-voice HIGH tier (64 kbit/s → fullband), `signal=Voice`. |
| Noise suppression | `nnnoiseless` (RNNoise port) | Opt-in mic denoise before encode (`Denoiser`, 480-sample frames). Pure Rust, no C/build deps. |
| Audio I/O | `cpal` | CoreAudio / WASAPI / ALSA. |
| Resampling | `rubato` (sinc) + linear fallback | Quality on device-rate mismatch. |
| Ring buffers | `ringbuf` | Lock-free thread handoff on hot path. |
| TUI | `ratatui` + `crossterm` | Event-driven redraw (dirty flag). |
| Web UI | `axum` (ws) embedded in client | Browser is UI-only; audio stays in the terminal. |
| Desktop shell | Tauri 2 + SolidJS + TS + Tailwind v3 (pnpm) | Reuses `tc-client`; thin command/event bridge. |
| CLI args | `clap` (derive) | — |
| Config files | `toml` | `~/.config/tc/`. |

**Before adding a dependency:** confirm nothing in the workspace already covers it, add it to `[workspace.dependencies]` in the root `Cargo.toml`, and note the new failure mode if it's on a request/voice path.

---

## 2. Crate / app contracts

- `tc-shared` — the only place wire types and tunables live. Anything in `protocol.rs` is a published contract between processes; anything in `config.rs` is a tunable knob. No business logic.
- `tc-client` is `[lib]` + `[bin]`. The library surface is what `tc-desktop` consumes — keep it usable headless (no TUI assumptions leaking into core voice/network logic).
- `tc-server` depends only on `tc-shared`. It must never depend on `tc-client`.
- `apps/desktop/src-tauri` depends on `tc-shared` + `tc-client`. Tauri commands (`commands.rs`, `audio_cmd.rs`, `settings_cmd.rs`, `voice_actor.rs`, …) are thin adapters; logic stays in the core. Server push → Tauri events (`events.rs`).

---

## 3. Wire protocol (summary — full spec in `PROTOCOL.md`)

| Channel | Transport | Default port | Encoding | Crypto |
|---|---|---|---|---|
| Control | TCP | 7100 | length-prefixed bincode | TLS 1.3 |
| Voice | UDP | 7101 | custom binary | XChaCha20-Poly1305 |
| Web UI | HTTP/WS | 17300 | JSON | localhost only |

- Frame: `[u32 BE length][bincode payload]`, max 64 KB (`MAX_FRAME_SIZE`).
- Voice packet: `[u32 seq][u8 channel_id_len][channel_id][encrypted opus]`. `seq == 0` ⇒ UDP hello (12 bytes: seq + `u64` token).
- Handshake: `Welcome` (server) → `Hello` (client), both carry `version` + `protocol`. Then `JoinChannel` → `JoinedChannel { participants, udp_token, voice_key }` → UDP hello (ACK'd, retried).
- `PROTOCOL_VERSION` (currently **3**, `config.rs`) bumps only on a breaking wire change. Mismatch ⇒ hard warning; version-only mismatch ⇒ soft note.

---

## 4. Security baseline (non-negotiable)

- **Transport.** TLS 1.3 only on control (`ring` provider, TLS 1.2 floor for compat). TOFU: server cert SHA-256 fingerprint pinned in config; a changed fingerprint is rejected until the user runs `/trust`. Never auto-accept silently.
- **Voice E2E.** XChaCha20-Poly1305 with a per-channel 32-byte key delivered in `JoinedChannel` over the TLS channel. The relay never receives the key and never decrypts. Nonce (24 B) prepended, tag (16 B) appended.
- **Identity.** Ed25519 keypair per user (`client/identity.rs`), persisted in the OS config dir. Server maps pubkey → display name; fingerprints surface in both UIs.
- **Rate limiting** (`server/rate_limit.rs`, sized in `config.rs`): per-client token buckets (commands, channel creation) *and* a per-source-IP bucket sized higher (NAT headroom) to defend against multi-connection abuse. Idle per-IP entries expire (`IP_LIMITER_IDLE_TTL_SECS`).
- **DoS caps.** `MAX_FRAME_SIZE` rejects oversized frames; `MAX_PENDING_BUF` disconnects a peer whose unparsed buffer overflows. `MAX_UDP_PACKET` bounds voice packets.
- **No content persistence.** Server keeps no chat history, no recordings, no content logs. Local text history (desktop) is per `(server, channel)` on the user's disk only.
- **Logs carry no content.** No chat text, no voice, no keys in `tracing` output.

---

## 5. Resilience — failure modes

| Path | Failure mode |
|---|---|
| Control connection drops | Auto-reconnect, exponential backoff, auto-rejoin last channel, `reconnecting` status shown. |
| UDP hello lost | Retried with backoff until ACK (`UDP_HELLO_RETRIES` / `_INTERVAL_MS`); voice not assumed live until ACK. |
| Voice packet lost | Opus PLC fills the gap; adaptive jitter buffer absorbs reorder; excess dropped to cap latency. |
| Slow/abusive peer | Bounded TCP queues apply backpressure; pending-buffer overflow disconnects that peer only. |
| Server shutdown | SIGTERM/SIGINT → broadcast shutdown error → clean exit; clients reconnect. |
| Poor network | Adaptive quality steps Opus bitrate/complexity down by measured loss (`LOSS_THRESH_*`, `BITRATE_*`). |
| Empty channels | Periodic maintenance sweep (`MAINTENANCE_INTERVAL_SECS`) cleans them up. |

---

## 6. Build & release

- Build: `cargo build --release` (LTO fat, 1 codegen unit, stripped). Linux needs `libasound2-dev`.
- Test: `cargo test --workspace` (unit + e2e two-client integration). `cargo fmt` + `cargo clippy --workspace` clean.
- Desktop: `pnpm -C apps/desktop build` (UI), `pnpm -C apps/desktop tauri build` (app).
- Load: `crates/loadtest` against a local server before merging server hot-path changes.
- CI: `.github/workflows/desktop-ci.yml`, `release.yml` (unified CLI + desktop matrix mac/win/linux → GitHub Releases).
- **Version bump touches three files:** root `Cargo.toml`, `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`. Bump `PROTOCOL_VERSION` separately and only on wire breaks.
