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

## Playback timeline (onset-click class)

- **Onset click** (`kfx6og`, v1.12.0, client-only): every talkspurt started with an audible click, and the cause was a chain of compensations rather than one bug. The RMS gate clips speech onsets, so `VadPreroll` was added to compensate; when VAD opens it flushes `VAD_PREROLL_FRAMES + 1` packets in one loop iteration; the receive path decodes them immediately (`drain_streams_to_playback` runs after every push and the jitter buffer delays nothing), so `playback_buf` took 5760 samples in a single callback against a `MAX_PLAYBACK_BUF` of 4800 — with jitter still 0 at stream start, `playback_cap` is exactly that. The old `playback_buf.drain(..excess)` then spliced two unrelated points of the waveform together, once per phrase. Fix: `PlaybackQueue` (`client/audio.rs`) owns every edit the output callback makes to the sample timeline and ramps both over `PLAYBACK_FADE_MS` — a crossfade across the trim splice, and fade in/out at the silence↔data boundary when the queue runs dry (which also cleaned up phrase *endings*). Latency cap deliberately unchanged at 100 ms, so one pre-roll frame is still discarded, now inaudibly. Codec state was ruled out: encoder and decoder freeze on the same frame across a gate closure and resume consistently. Buffer arithmetic was lifted out of the cpal closure into a testable struct; the regression guard asserts the burst *still* overshoots the cap before measuring the step, so it can't pass by the trim quietly disappearing. Open follow-ups recorded as `4rg56x` (comfort noise instead of zero-fill) and `ta0vu0` (continuous DTX timeline instead of gate-and-flush — the structural exit from this whole chain, the way WebRTC/NetEq avoids the class).

## UX / protocol features

- Active-speaker indication (sender id in UDP packet → tracked on client → shown in TUI).
- Message-history scroll (PgUp/PgDn) in the TUI.
- XDG-compatible config path (`~/.config/tc/`) with one-time migration from legacy `~/.config/termicall/`.
- Version exchange (`Hello`/`Welcome` carry version + protocol; mismatch warnings).
- Embedded web UI (axum WS) — browser as UI only, audio stays in the terminal.
- Refactors: `tc-client` split into `[lib]` + `[bin]`; `handle_command`/`handle_message`/`start_voice` decomposed.

## Docs

CONTRIBUTING.md, CHANGELOG.md, full wire-protocol spec (`PROTOCOL.md`), cross-platform system-dependency notes, ARM64 Linux build support.

## Windows capture was 6 dB down: the downmix divided by silent channels (`4s219r`, v1.16.1, 2026-08-11)

Reported as "Windows picks up about half of what macOS does, and 200% gain
still isn't the same loudness or quality". Both halves have one cause.

macOS and Linux open the device on our terms — the fixed 1ch/48k config, so no
downmix happens at all. Windows cannot: WASAPI shared mode grants the mixer
format or nothing, so `resolve_stream_params` takes the device default whole,
and that is typically 2 channels. The capture callback then did
`chunk.iter().sum() / ch`, and a mono microphone delivered as stereo-with-a-
silent-second-channel — the common wiring — loses exactly half its amplitude.
The "quality" half follows: 200% gain buys the level back while amplifying the
noise floor and eating headroom.

`Downmix` now derives the divisor from the channels that carry signal: a slow
per-channel magnitude envelope, a channel counting while it is within
`DOWNMIX_ACTIVE_RATIO` (−30 dB) of the loudest, and no re-deciding below
`DOWNMIX_SILENCE_FLOOR` so a pause between words cannot flap the divisor
mid-sentence. A channel that does not count is also not summed, keeping its
noise floor out.

The property that made this safe to ship without a Windows machine: **when
every channel carries signal the arithmetic is identical to the average it
replaces**, so macOS, Linux and any true stereo input are untouched. Tests pin
both directions (silent second channel keeps full amplitude, true stereo still
averages, silence preserves the decision). Diagnosis on the user's side is one
log line — `input: 2ch …` at open, then `capture downmix: 1 of 2 channels carry
signal`.

Still unverified by us: nobody here has Windows. If that log line reads `1ch`,
the cause is elsewhere — start with the Windows per-device level and Microphone
Boost, which sit before we ever see the samples.
