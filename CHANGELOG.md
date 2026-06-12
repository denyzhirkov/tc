# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [1.9.16] — 2026-06-12

### Fixed
- Phantom speaking wave, actual root cause: Solid's `setState` merges
  objects instead of replacing them, so a peer's speaking level never left
  the UI store once set — the identicon waved forever on both clients while
  the channel carried zero packets, and every previous backend-side "clear"
  was a silent no-op. Speaker levels now go through `reconcile()`.
- Scratchy speech onsets: the capture pipeline keeps a 100 ms pre-roll of
  VAD-gated audio and flushes it when VAD opens, so the quiet first
  consonants are no longer clipped.

### Added
- Public channels appear in everyone's sidebar immediately on creation
  (server broadcasts the channel list); private channels stay unannounced.

### Changed
- Default VAD level is 15 in both clients (TUI default was 10).

## [1.9.15] — 2026-06-12

### Added
- Hot-plug prompt: a newly connected audio device shows a "use it now?"
  toast (yes switches immediately, no is remembered for the session). The
  device already configured in settings reconnecting switches back
  automatically without a prompt.
- Degraded-call chips in the voice strip: "no mic" / "no audio out".

### Changed
- Capture and playback are now independent halves of the voice pipeline:
  a missing or dead microphone leaves you listening, a missing output
  leaves you speaking, and with neither the call/chat still works — audio
  recovers automatically as soon as devices appear.
- Calls follow the system default device: when "Default" is selected, a
  default-device change moves the live call within ~2 seconds.
- Changing the input/output device in settings now applies immediately
  mid-call instead of at the next join.
- The settings test tone falls back to the system default when the saved
  output device is gone.

## [1.9.14] — 2026-06-12

### Fixed
- macOS: the bundled app could never access the microphone — the bundle was
  missing `NSMicrophoneUsageDescription`, so macOS silently delivered zeros
  and never showed the permission prompt (dev builds worked because the
  terminal's permission applied). Added `src-tauri/Info.plist` and an
  `Entitlements.plist` (audio-input) for future hardened-runtime signing.
  On first join the app now prompts for microphone access.
- Audio device lifecycle: a died stream (e.g. unplugging a headset) is now
  detected via stream error callbacks plus a capture starvation watchdog,
  and the desktop voice pipeline rebuilds itself automatically (throttled),
  falling back to the system default device when the saved one is gone.
  A failed pipeline start is retried, so voice recovers as soon as a device
  appears. A note is posted to the feed when a rebuild happens.
- A stale saved device name no longer fails the whole join — it falls back
  to the system default with a warning.

## [1.9.13] — 2026-06-11

### Fixed
- Jitter buffer wedged permanently after a forward sequence gap longer than
  the ring (a ~300 ms loss burst, e.g. the deaf window while UDP registration
  re-heals): every later packet was dropped as "too far ahead", playback went
  silent forever while the speaking indicator kept animating. The buffer now
  resyncs onto the new stream position after `JITTER_RESYNC_AFTER` consecutive
  out-of-window packets; a single stray packet still cannot reset a healthy
  stream. Client-only fix.

## [1.9.12] — 2026-06-11

### Fixed
- Self-healing UDP voice registration. The idle keepalive now re-hellos with
  the real session token instead of the inert token-0 packet, so the relay
  refreshes the client's registered address after a NAT rebind — previously
  the server kept relaying voice to the stale external port and the client
  went permanently deaf while everything else looked healthy.
- Server: tokens of live in-channel sessions are no longer expired by the
  30-second cleanup (only orphaned tokens age out), otherwise the re-hello
  self-healing above would stop working 30 seconds after joining.
- Requires updating both the server and the clients to take effect.

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
