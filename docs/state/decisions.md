# Architectural truths & gotchas

Durable facts that aren't obvious from the code. Add a bullet when a decision or gotcha would cost the next session time to rediscover. Big decisions also get an ADR in `docs/adr/`.

## Architecture

- **The server is a dumb encrypted relay.** It parses only `channel_id` from a voice packet (zero-copy) and forwards raw bytes to the other participants. It never holds the voice key, never decrypts, never persists content. This is a privacy guarantee, not an optimization — don't move content handling server-side.
- **One core, three processes.** `tc-client` is `[lib]` + `[bin]`; the desktop (`tc-desktop`) is a Tauri/Solid frontend over the same library, not a second implementation. Keep the core headless-usable — no TUI assumptions in voice/network logic. (ADR-0001.)
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
- **Keepalive = hello with token 0** (tokens are never 0). Sent after `UDP_KEEPALIVE_INTERVAL_SECS` of send-silence so NAT/firewall mappings keep the inbound path open; the relay counts (`udp_keepalives`) and drops it. Old servers treat it as an invalid hello — wire-compatible, no PROTOCOL_VERSION bump.
