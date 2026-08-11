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

## Automatic capture level (`9ezeql`, v1.17.0, 2026-08-11)

Follow-on from `4s219r`. The Windows downmix fix was real but not the whole
story: the owner's fifine stayed quiet even at the 200% manual ceiling. The
sentence that settled it — "this same microphone works fine in Discord and
Telemost" — is decisive, because both are WebRTC with AGC enabled by default.
Nothing was wrong with the microphone, with Windows, or with the downmix. We
simply had no automatic level at all, and a manual gain cannot substitute for
one: it multiplies the noise floor by exactly the same factor as the voice,
which is why 200% sounded loud-but-wrong rather than loud.

`client/agc.rs` targets `AGC_TARGET_RMS` with the gain clamped to
[−12 dB, +18 dB]. Two properties are the whole design:

- **It learns only from speech.** Frames under `AGC_FLOOR_RMS` do not move the
  gain. An AGC that adapts in a pause is a machine for winding up room noise
  between sentences — the artefact everyone recognises as bad automatic gain.
- **Down fast, up slow** (`AGC_ATTACK` 0.35 vs `AGC_RELEASE` 0.03 per 20 ms
  frame). Coming down is urgent — the next syllable would clip. Going up is
  cosmetic and takes about a second, so it is inaudible.

A soft limiter (knee at `AGC_KNEE`, asymptotic to 1.0) is part of the feature,
not a refinement: any gain worth having exceeds the headroom of a loud syllable,
and those peaks would clip inside Opus. Placed after denoise (level measured on
cleaned speech, not on noise) and before the RMS meter (so "my voice" shows what
is actually sent).

**Default on**, reversing the plan stated when the work was scoped. Every
comparable app levels the microphone itself, so an off-by-default switch would
have left a quiet microphone quiet for everyone who never found it. That default
came with a trap worth remembering: the settings writer elided `false` as `None`
for the other flags, and with an on-by-default field an absent value reads as
"on" — saving "off" would have undone itself at the next start. It is written
explicitly now.

One deliberate asymmetry: the sound check runs with the user's real AGC setting
while still forcing denoise off. "How loud am I" is the question that check is
asked, and answering it through a path the call does not use is the kind of lie
it exists to catch.

## A client was never told its own name (`zw8tqe`, v1.18.0, 2026-08-11)

Reported as "the other person has no buttons in the room". The cause was four
levels away from buttons: `server/state.rs` names every registering client
`user-N`, and nothing ever sent that name back — `Welcome` carries version and
protocol only. A user who had not set a name locally therefore held `None` while
the server and every peer called them `user-10`.

Everything in the room keys off `me()`, so one gap broke four things at once:
the acts row (`anchor()` finds no standing name that is ours) — the visible
symptom; the "you" tail; the input-peak branch of `levelOf`, so their own name
never moved with their own voice; and `Falling`, which resolves "you: …" through
`me()` and so never found a node to drop their own words from. Worth
remembering as a shape: **a single unknown identity can look like four unrelated
UI bugs.**

Fix without a protocol bump: right after `Welcome`, the server sends that client
`NameChanged { old_name: "", new_name }`. Not a new variant — this *is* a name
change, from nothing to `user-N`, and an empty `old_name` marks it as
self-identification. Clients adopt it only when they have no name, so a chosen
name still wins and its `SetName` round-trip is untouched. Old client + new
server = one cosmetic log line; new client + old server = today's behaviour. The
three e2e tests had encoded the old handshake and now assert the name arrives.

Deployment note that matters more than the code: **this half is in the server**.
A client update alone changes nothing.

Same release: `invite_url` stopped stripping the default port. Omitting it was
correct for exactly one reader — our own `deeplink.rs`, which fills the default
back in — and wrong for a person, a chat client, or a paste into `/server`. Bare
IPv6 literals are bracketed on the way out, which the old "does it contain a
colon" test could never have got right.

**A link the app could not read back (v1.18.1).** Pasting `tc://host:7100/pub-room`
into `/server` died with "invalid port value": the string reached the socket
verbatim and `ToSocketAddrs` splits on the *last* colon, so the port it parsed
was `7100/pub-room`. The app tripped over the exact string it hands out when you
click your own room — one release after that click was added. Parsing now lives
beside the builder in `tc_shared::invite` and serves the desktop `connect`
command, `/server` in the TUI, and the OS deep-link handler, which had been
carrying a second parser on the `url` crate (now dropped as a dependency). A
pasted link that names a room also joins it. **Rule of thumb this leaves
behind: anything the app puts on a clipboard has to be accepted back in every
field where its plain form is accepted.**
