# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [1.9.11] — 2026-06-11

### Fixed
- Desktop: phantom speaking indicator — the frontend was never told when the
  voice pipeline stopped, so the last speaker levels kept animating forever
  ("constant incoming stream" from a silent peer). The level pump now emits
  `voice_stopped` on the active→inactive transition, `left_channel` clears
  voice state, and a 1s frontend watchdog zeroes the meters if `voice_level`
  events stall.
- Input level meter froze at the last pre-mute RMS while muted; it now drops
  to zero.
- Desktop: the settings test tone now silences itself inside the audio
  callback after the requested duration — a wedged stream teardown can no
  longer leave an endless beep.

## [1.9.10] — 2026-06-11

### Fixed
- Voice receive path: one jitter buffer + Opus decoder **per sender** with PCM
  mixing into playback. The old shared buffer treated interleaved sequence
  streams from different senders as endless stream restarts and reset itself
  on nearly every packet — two people speaking at once silenced the channel
  for everyone.
- Echo guard: inbound voice packets carrying the client's own sender name are
  dropped before the speaking indicator and playback (stops feedback loops via
  a peer's loopback input device, e.g. "Stereo Mix").

## [1.8.0] — 2026-05-06

### Changed
- Desktop UI: full redesign — quiet sans-serif aesthetic, click-driven sidebar
  (servers / channels / DMs), grouped messages with day separators and
  timestamps, right-side `IN CALL` peer panel with voice bars and fingerprints,
  large `CONNECTED` strip with mute / mode / leave controls
- Identicon: spiral phyllotaxis pattern derived from fingerprint
- Sidebar voice-activity wave (animated vertical bars) under speaking channels
- Private channels marked with subtle diagonal stripes
- Tweaks panel in Settings: theme, accent, density, typeface

### Added
- ⌘K palette replaces the old slash-command bar — channels, DMs, servers,
  free-form commands
- `@`-mention popover in the channel composer opens a DM with the picked peer
- Public channels auto-load on connect; sidebar refreshes after disconnect
- Self-rename safety net: client re-sends `SetName` if a channel join races
  ahead of name registration
- `name_changed` event now updates participants, speaker map and self status

### Fixed
- HMR no longer double-subscribes Tauri event listeners (was producing N×
  duplicate log lines after dev reloads)

## [1.7.3] — 2026-03-15

### Fixed
- CI: install arm64 multiarch `libasound2-dev` for cross build
- Install script: only install client by default, server is opt-in

### Added
- ARM64 Linux (Raspbian) build support

## [1.7.0]

### Added
- Server stability hardening, structured logging
- TLS: auto-trust changed certificates, full error reporting
- Integration test suite for TLS and protocol exchange

### Fixed
- TLS: use ring crypto provider exclusively (removed aws-lc-rs)
- TLS: correct SNI hostname usage
- Linux: `sendmmsg` type cast and `BatchSender` Send impl

## [1.6.0]

### Changed
- Zero-allocation audio pipeline (lock-free ring buffers, reusable codec buffers)
- Atomic network stats (no more Mutex in voice hot path)
- Server: batch UDP send via `sendmmsg` on Linux

## [1.5.0]

### Added
- Public named channels (`/create <name>` creates `pub-<name>`)
- `/list` command to browse public channels
- Autocomplete preview on arrow key navigation

## [1.4.0]

### Added
- Volume control (`/config vol`, `/config gain`)
- Command shortcuts (`/m` for mute)

### Fixed
- Jitter buffer edge cases

## [1.3.0]

### Added
- Embedded web UI (browser control panel at `http://127.0.0.1:17300`)
- Protocol versioning (Hello/Welcome with version + protocol number)

## [1.2.0]

### Added
- TLS with TOFU certificate pinning
- Server-side rate limiting (token bucket)
- Voice quality optimizations (adaptive bitrate, FEC)
- Improved TUI (status bar, scroll, command history)

## [1.1.0]

### Fixed
- Windows: move UDP send from std::thread to tokio task
