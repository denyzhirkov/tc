# ADR-0002 — Relay never decrypts voice (end-to-end encryption)

**Status:** Accepted

## Context

`tc` is a hosted relay: clients connect to a server that forwards voice between channel participants. The privacy promise is "no accounts, no recording, nobody listening." A naive relay that decrypts and re-encrypts (or just handles plaintext) would make the server a single point of compromise for every conversation and a tempting target for lawful-intercept or operator abuse.

## Decision

Voice is **end-to-end encrypted between clients** with XChaCha20-Poly1305 using a per-channel 32-byte key. The key is minted per channel and delivered to clients inside the TLS control channel (`JoinedChannel.voice_key`); it is **never sent to or stored by the relay**. The server parses only the `channel_id` prefix of a UDP voice packet (zero-copy) and forwards the still-encrypted bytes to the other participants. The server keeps no chat history, no recordings, and no content logs.

## Consequences

- A compromised or malicious relay cannot read voice — it only sees ciphertext, channel membership, and traffic timing/volume (acknowledged metadata leakage).
- The relay is cheap and dumb: no codec, no crypto on the hot path beyond byte-shuffling → high fan-out throughput (`sendmmsg`/`sendmsg_x` batching).
- Key management is the clients' problem. Today the key is static for a channel's lifetime; **forward secrecy requires key rotation** (planned: EPIC 15 — `KeyRotation` message + periodic server-driven rotation + dual-key transition). Until then, a leaked channel key exposes that channel's past traffic if recorded.
- Any feature that would need server-side access to content (server-side search, moderation of voice, transcription) is **out of scope by design** — it would break this ADR and must be raised as a new decision, not slipped in.
