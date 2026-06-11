# Closed history — CLI core (relay, TUI, protocol)

History of the terminal-client + server arc. Newest milestones at the bottom of each section. Source of record is `tsk`; this is the human-readable narrative.

## Foundations

Project scaffold, workspace, and the shared protocol/types. Server TCP control channel and UDP voice relay; client TCP connect + commands, CPAL audio capture/playback, Opus encode/decode, UDP voice send/receive, and the ratatui TUI. Closed with an end-to-end integration test (two clients exchanging chat + voice against a real server).

## Hardening pass (correctness & DoS)

- TCP frame-size limit to stop an OOM DoS (`MAX_FRAME_SIZE`) and bounded pending buffer (`MAX_PENDING_BUF`).
- Channel-ID collision fix; UDP hello race condition fixed with server ACK + retransmit/backoff (EPIC 14).
- Sequence wrap-around handling; Opus PLC for lost packets.
- Voice-pipeline resource-leak fixes; audio-path allocation/latency reductions; server-side optimizations.

## Security epics

- **EPIC 1 — TLS TOFU.** SHA-256 fingerprint of the server cert, trusted-fingerprint storage in `tc.toml`, TOFU verifier in `CustomVerifier`, `/trust` command to accept a changed cert.
- **EPIC 2 — TCP pending-buffer cap.** `MAX_PENDING_BUF` enforced in both `server/tcp.rs` reader loop and `client/network.rs` reader task.
- **EPIC 3 — Rate limiting.** Token-bucket `RateLimiter`, config in `config.rs`, integrated into `handle_message`. Later extended to a per-source-IP bucket with idle TTL.

## Reliability epics

- **EPIC 4 — Auto-reconnect.** Exponential backoff, auto-rejoin last channel, `reconnecting` status in the TUI.
- **EPIC 16 — Graceful shutdown.** SIGTERM/SIGINT via `tokio::signal`, broadcast "server shutting down" before exit.
- **EPIC 17 — Bounded client channels.** Replaced unbounded channels (outgoing commands + incoming `ServerMessage`) with bounded + backpressure.
- Concurrent-speakers fix (`oyvhmx`, v1.9.10): receive path moved to per-sender streams — `HashMap<name, SenderStream{JitterBuffer, OpusDecoder}>` with PCM mixing (one frame per sender per round, clamp when >1 contributor). Previously a single shared jitter buffer served all senders: independent seq counters of two senders looked like endless "stream restarts" → reset on every packet → two concurrent speakers silenced the whole channel. Plus an echo guard: a packet carrying our own name is dropped before the speaking map and playback (guards against a loopback through a peer's input device). Cap `MAX_SENDER_STREAMS`, TTL `SENDER_STREAM_TTL_SECS` — both in `config.rs`.

## Performance arc (zero-alloc voice + server throughput)

- Adaptive jitter buffer (ring-buffer based, counter instead of O(N) scans) and adaptive playback-buffer cap.
- Zero-allocation voice hot path: reusable Opus encode/decode buffers, in-place decrypt, zero-copy `VoicePacket::decode`, eliminated per-frame `packet_buf.clone()`, reusable encryption buffer, reusable resampler output, `VecDeque` playback pop, reusable `BytesMut` for encode.
- Lock-free `ringbuf` for decode→playback (last allocation removed); `Mutex<NetworkStats>` → atomics.
- `BytesMut` for TCP framing (EPIC 8) instead of `Vec` drain.
- Event-driven TUI with a dirty flag instead of 50 ms polling (EPIC 9).
- Quality sinc resampling via `rubato`, linear fallback retained (EPIC 10).
- Server relay: parallel `send_to` via `FuturesUnordered`, then `sendmmsg` batch (Linux) and `sendmsg_x` (macOS); peer cache → `ArcSwap` (lock-free reads); `list_public_channels`/`cleanup_empty_channels` reduced from O(N×M) to O(N+M); `broadcast_channel`/`broadcast_to_channel` merged; UDP-token TTL.

## UX / protocol features

- Active-speaker indication (sender id in UDP packet → tracked on client → shown in TUI).
- Message-history scroll (PgUp/PgDn) in the TUI.
- XDG-compatible config path (`~/.config/tc/`) with one-time migration from legacy `~/.config/termicall/`.
- Version exchange (`Hello`/`Welcome` carry version + protocol; mismatch warnings).
- Embedded web UI (axum WS) — browser as UI only, audio stays in the terminal.
- Refactors: `tc-client` split into `[lib]` + `[bin]`; `handle_command`/`handle_message`/`start_voice` decomposed.

## Docs

CONTRIBUTING.md, CHANGELOG.md, full wire-protocol spec (`PROTOCOL.md`), cross-platform system-dependency notes, ARM64 Linux build support.
