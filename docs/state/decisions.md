# Architectural truths & gotchas

Durable facts that aren't obvious from the code. Add a bullet when a decision or gotcha would cost the next session time to rediscover. Big decisions also get an ADR in `docs/adr/`.

## Architecture

- **The server is a dumb encrypted relay.** It parses only `channel_id` from a voice packet (zero-copy) and forwards raw bytes to the other participants. It never holds the voice key, never decrypts, never persists content. This is a privacy guarantee, not an optimization — don't move content handling server-side.
- **One core, three processes.** `tc-client` is `[lib]` + `[bin]`; the desktop (`tc-desktop`) is a Tauri/Solid frontend over the same library, not a second implementation. Keep the core headless-usable — no TUI assumptions in voice/network logic. (ADR-0001.)
- **The terminal client stays — deliberately** (decided 2026-06-11, removal was considered and rejected). Voice chat over SSH/headless is the product's unique niche and the TUI doubles as a lightweight second client for reproducing bugs. Cost is low because the core is shared; only UX features need doing twice — new UX may ship desktop-first without a TUI counterpart, but the TUI binary keeps building, shipping, and working.
- **Two transports, two failure modes.** Control = TLS/TCP (reliable, ordered); voice = UDP (lossy, latency-first). Don't couple them; a control hiccup must not stall audio and vice versa.
- **Stateless / no DB.** Server state is in-memory (`server/state.rs`). User identity/settings/history live on the user's disk (`~/.config/tc/`). There is intentionally no account system.

## Voice hot path (allocation-sensitive)

- Steady-state target is **zero heap allocation per frame**. Achieved via: reusable codec buffers, in-place decrypt, slice-returning decode, zero-copy `VoicePacket` parse, reusable `BytesMut` for encode, `VecDeque` (not `Vec::drain`) for playback, and a lock-free `ringbuf` between decode and playback. A `Vec`/clone per frame is a regression — call it out in review.
- Network stats are **atomics**, not `Mutex` — the audio thread must not block.
- Relay peer cache is **`ArcSwap`** for lock-free reads on the UDP fan-out path.
- Batched UDP send: `sendmmsg` (Linux), `sendmsg_x` (macOS), sequential fallback elsewhere. `BatchSender` reuses sockaddr/iovec while the peer-set is stable.
- Adaptive jitter buffer keeps a **counter** instead of O(N) scans; sizing + Opus tier selection are driven by `config.rs` constants, not inline literals.

## Security gotchas

- **TOFU, not auto-trust.** A changed server cert fingerprint is *rejected* until the user runs `/trust`. Never widen this to silent acceptance.
- **Voice key never touches the server.** It's minted per channel and delivered inside the TLS control channel in `JoinedChannel`. Keep the relay key-blind.
- Rate limiting is **per-client AND per-source-IP** — the per-IP bucket exists specifically to stop one attacker multiplying the per-client cap across many connections. Don't drop it when refactoring `handle_message`.
- `MAX_PENDING_BUF` overflow must **disconnect the peer**, not grow the buffer — this is the OOM-DoS guard.

## Protocol / versioning

- `PROTOCOL_VERSION` (in `config.rs`) bumps **only** on a breaking wire change; the workspace `version` tracks every release. They are independent on purpose.
- A release bump must stay in sync across three files: root `Cargo.toml`, `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`. A drift here has shipped before (see commit history) — check all three.

## Gotchas / footguns

- **Legacy config migration is real code.** `client/settings.rs` intentionally references the old `~/.config/termicall/` directory to one-time-migrate it into `~/.config/tc/`. Do **not** "rename" these strings — they exist precisely because the old name shipped to users.
- Desktop HMR once double-subscribed Tauri event listeners (N× duplicate log lines after dev reloads) — guard listener registration against re-entry.
- Self-rename can race a channel join; the client re-sends `SetName` if the join wins the race.

## UDP registration & liveness (one-way-audio fix pack)

- **UDP voice target derives from the live TCP peer IP** (`ServerConnection::peer_addr()`), never from re-resolving the server string. The server requires hello-IP == TCP-IP; a hostname resolving differently per family (TCP v6, UDP v4 — `localhost` on Windows!) used to cause a permanent reject → one-way audio. Don't reintroduce a second resolve.
- **Hello tokens are idempotent and survive mismatches.** The token is kept after successful registration (TTL refreshed) so duplicate hellos get re-ACKed (lost-ACK recovery), and an IP-mismatched hello does not burn it (64-bit token is unguessable; burning enabled a spoof-DoS). A re-hello from a new source port replaces the stale addr in the channel set (NAT rebind).
- **Registration failure must be loud.** `start_voice` keeps running on handshake timeout but exposes `is_registered()`; UIs show NO-RX and a background re-hello loop (backoff `UDP_REHELLO_BASE_MS`→`UDP_REHELLO_MAX_MS`) keeps trying until ACK or first inbound packet. Never silently "proceed anyway".
- **Keepalive = re-hello with the real token** (since v1.9.12; was token-0 in 1.9.8–1.9.11). Sent after `UDP_KEEPALIVE_INTERVAL_SECS` of send-silence. Token-0 only refreshed the NAT mapping but **not the registration** — when a NAT rebound the external port, the relay kept sending voice to the stale address and the client went permanently deaf (proven in the field: the app's socket had one hello ACK and zero voice while a fresh listener on the same NAT received everything). A real-token re-hello refreshes both; the extra ACKs are ignored by the receive path (seq==0 never parses as voice). The server still accepts token-0 from old clients (counts + drops).
- **The server never expires the token of a live in-channel session** (`cleanup_empty_channels` retains it; only orphaned tokens age out after 30s). Without this, re-hello self-healing dies 30 seconds after join.

## Receive path: per-sender streams (concurrent-speakers fix)

- **One jitter buffer + one Opus decoder per sender, keyed by the name in the decrypted packet.** Sequence counters of different senders are independent (each starts at 1), so a single shared buffer treated any interleaving of two streams as endless "stream restarts" and reset itself on nearly every packet — two concurrent speakers (or any echo/loopback) silenced playback entirely. `spawn_recv_task` now keeps `HashMap<name, SenderStream>`; capped at `MAX_SENDER_STREAMS`, pruned after `SENDER_STREAM_TTL_SECS` without a packet. Don't collapse this back to a single buffer.
- **Mixing, not concatenation.** `drain_streams_to_playback` pops one frame per sender per round, sums PCM into a reusable mix buffer (clamped to ±1.0 only when >1 contributor) and pushes the mixed frame — single-talker rounds are bit-identical to the old path and stay zero-alloc.
- **The jitter buffer must resync on sustained forward gaps, not just backward jumps.** A loss burst longer than the ring (15 packets = 300 ms — e.g. the deaf window while UDP registration re-heals) used to wedge it forever: every later packet was "too far ahead", playback went permanently silent while the speaking indicator (updated before the push) kept animating. After `JITTER_RESYNC_AFTER` consecutive out-of-window packets the buffer resets onto the new position; a single stray packet still can't reset a healthy stream. Note: the seq header is outside the AEAD (unauthenticated) — fixing that is a protocol change, tracked separately.
- **Echo guard on receive:** packets whose sender name equals our own are dropped before the speaking map and playback. The server already excludes the sender by socket addr, but a loopback through a peer (mic capturing speakers, "Stereo Mix" input) or any future relay echo otherwise plays your own voice back. Keying is by display name — a peer sharing our name gets dropped too; the wire format has no per-sender id (known limitation, needs a protocol change to fix properly).

- **`cargo test` runs on ubuntu + macOS + windows**, not just Linux. The UDP batch sender has three `#[cfg]` send paths (`sendmmsg`/`sendmsg_x`/sequential `try_send_to`) and only the OS-matching one compiles per platform — a Linux-only test job would never *execute* the macOS/Windows paths. The behavioral `BatchSender` test in `server/udp.rs` and the IPv6 (`::1`) e2e are the regression guards; they only have teeth because the matrix runs them on real runners.
- **`CMAKE_POLICY_VERSION_MINIMUM=3.5` is required on any job that compiles `tc-client`/`tc-server` on macOS/Windows** (Opus → `audiopus_sys` vendors an old-CMake build). The build/release jobs already set it; test jobs must too or they fail at compile before running a single test.
- **Don't assert delivery from a freshly-bound tokio `UdpSocket` without `writable().await` first.** On Windows the non-blocking `try_send_to` fallback returns `WouldBlock` until the socket is registered writable and silently drops the datagram (fine for lossy voice in production, flaky for a test). The relay's recv socket is always ready by send time, so production is unaffected.

## Desktop voice UI state (phantom-indicator class)

- **The frontend voice state is event-driven and must be told about every transition, including "stopped".** `level_pump` emits `voice_level` only while the voice handle is active; it must emit `voice_stopped` on the active→inactive flip (and `left_channel` clears voice state too) — otherwise the Solid store freezes at the last snapshot and speaker waves animate forever ("constant stream from a peer" that no packet sniffer can see). A 1s frontend watchdog additionally zeroes the voice UI if `voice_level` events stall (backend pump stuck/dead).
- **Published atomics must be reset on the path that stops publishing them.** The capture thread skips frames while muted — it has to store `input_peak = 0` first, or the self/sidebar level meter freezes at the last pre-mute RMS.
- **The settings test tone silences itself inside the audio callback** after the requested duration (frame countdown → zeros); the `sleep + drop(stream)` remains, but a wedged CoreAudio dispose can no longer leave an infinite beep.

## tc:// deeplink hardening (invite pack)

- **`tauri-plugin-single-instance` (feature `deep-link`) must stay the first plugin** in the builder. On Windows/Linux a tc:// click spawns a second process; the plugin forwards argv to the running instance and re-triggers `on_open_url` itself — our callback only surfaces the window. Re-ordering plugins silently breaks deep links on those platforms.
- **Cold-start invites go through `deeplink::Gate`**, not a bare `emit`. `on_open_url` fires before the webview subscribes; the Gate buffers the invite until the frontend calls `take_pending_invite` (which also flips `frontend_ready`). An emit-only path loses the very link that launched the app.
- **Invites to servers not in the registry require explicit confirmation** (`InviteConfirm` modal). Auto-connecting would let any web page drive-by-connect the client — leaking IP + Ed25519 pubkey and TOFU-pinning an attacker cert. Known-registry servers connect without friction.
- **Invite URL building lives in `tc_shared::invite_url`** (single owner of the default-port-omission rule, next to `TCP_PORT`); parsing stays desktop-side in `deeplink.rs` (needs the `url` crate, and only the desktop registers the OS scheme). `/invite` in both clients uses the shared builder.
- **No artificial delay between connect and join:** the `connect` command resolves only after the TLS session is up and `c.conn` is set, so a `join_channel` straight after is ordered on the same stream. The old 400 ms `setTimeout` was guarding nothing — don't bring it back.
