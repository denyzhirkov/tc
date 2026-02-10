# Minimal Terminal Voice Chat (Rust)

## Project Overview

This project is a **minimal terminal-based voice and text chat application written in Rust**.

Primary use case:
- quick voice communication during gaming,
- extremely simple channel-based communication,
- zero unnecessary features.

This is NOT a Discord/Slack/Teamspeak replacement.

The goal is:
→ minimalism  
→ low latency voice communication  
→ reliability  
→ simple terminal UX  

If unsure — choose the simplest working solution.

---

## Core User Flow

1. User runs client in terminal.
2. Creates a channel:

   /create

3. Receives a short channel ID.
4. Shares ID with friends.
5. Friends join via:

   /connect <channel_id>

Once connected:
- voice chat works immediately,
- optional text chat works in the same channel.

No persistent identity or history required.

---

## Architecture Requirements

### Networking model

Use a **central relay server**.

DO NOT implement P2P or NAT traversal.

Architecture:

- TCP → control channel (commands, channel management).
- UDP → voice transport.
- Server relays audio packets to all channel participants.

Prioritize simplicity and reliability.

---

## Audio Requirements

- Codec: Opus.
- Low latency configuration.
- Cross-platform audio capture/playback.

Required basics:

- push-to-talk support,
- simple jitter buffer,
- packet sequence handling,
- minimal latency buffering.

Avoid advanced DSP unless explicitly requested.

---

## Terminal Interface Requirements

Terminal-first UX:

Commands:

/create
/connect <id>
/mute
/quit

Optional:

- speaking indicator,
- participant list,
- simple text chat.

No GUI.

---

## Technical Stack (Preferred)

- Rust stable toolchain.
- Tokio async runtime.
- CPAL for audio I/O.
- Opus bindings (audiopus/opus).
- Ratatui + Crossterm for terminal UI.

Dependencies should stay minimal.

---

## Non-Goals (Do NOT Add)

Unless explicitly instructed:

- user accounts,
- authentication systems,
- persistent chat history,
- moderation/roles,
- bots or automation,
- video/screen sharing,
- mobile apps,
- distributed infrastructure,
- complex UI,
- Discord-like feature expansion.

Keep it minimal.

---

## Engineering Principles

- Simplicity over completeness.
- Working MVP over theoretical perfection.
- Readable, maintainable Rust code.
- Avoid premature optimization.
- Avoid over-engineering.

If complexity increases:
→ reconsider simpler solution first.

---

## When Unsure

1. Ask clarification questions.
2. Do not invent features.
3. Stick to the minimalist voice-chat goal.
4. Prefer practical implementation over idealized architecture.

This project intentionally values "primitive but working".
