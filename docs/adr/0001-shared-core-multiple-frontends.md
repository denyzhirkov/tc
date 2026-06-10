# ADR-0001 — Shared `tc-client` core, multiple frontends

**Status:** Accepted

## Context

`tc` ships two clients — a terminal app (ratatui TUI) and a desktop app (Tauri + Solid) — plus a relay server. The voice pipeline (capture, Opus, crypto, jitter, network), the protocol client, TLS/TOFU, identity, and settings are non-trivial and identical across both clients. Reimplementing them per frontend would double the surface for bugs and protocol drift.

## Decision

`tc-client` is built as both `[lib]` and `[bin]`. The library is the **single core**: voice, network, TLS, identity, settings — usable headless, with no TUI assumptions. The TUI binary and the desktop crate (`apps/desktop/src-tauri`, which depends on `tc-shared` + `tc-client`) are thin presentation layers. Tauri commands and events are adapters over the library, not a second implementation. `tc-server` depends only on `tc-shared`.

## Consequences

- Protocol and voice logic live in exactly one place; a wire change is made once.
- The core must stay frontend-agnostic — no `ratatui`/Tauri types or terminal assumptions leaking into voice/network modules. This is an ongoing constraint, enforced in review.
- A future third frontend (mobile, headless bot) is a new adapter, not a rewrite.
- Slight indirection cost in the desktop bridge (command/event marshalling) — accepted.
