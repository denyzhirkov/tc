# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [1.11.0] — 2026-08-10

### Added
- **You can see a voice, not just hear it.** While someone speaks, a thin
  string hangs under their name and a wave runs down it, away from them — wide
  when they're loud, calming toward the far end, carrying the last couple of
  seconds of what they said. It arrives as they start and is gone three seconds
  after they stop, so a quiet room stays quiet. Words still fall from the same
  place, along the string of the voice that said them. In a crowded room the
  string shortens and recedes, the same way falling speech already does, and
  with reduced motion turned on it never appears.

## [1.10.2] — 2026-08-06

### Fixed
- **Copying your fingerprint.** The copy action sat in the "server" row and
  copied *your* fingerprint, so anyone using it to check the server got the
  wrong thing — while the fingerprint shown at the top of the section, the one
  you send a peer to prove it's you, couldn't be copied at all. Copying now
  sits under that fingerprint; the server row copies its address.

## [1.10.1] — 2026-08-06

### Added
- **Updates offer the build for your machine.** "Download" now points at the
  file for the system you're on — the arm64 or Intel disk image on a Mac, the
  installer on Windows, the AppImage on Linux — instead of leaving you to pick
  one off a list of fourteen. "What's new" still opens the release page, so a
  direct download doesn't skip the notes. If a release has nothing for your
  platform, you get the release page as before.

### Changed
- **Update and new-device messages** are no longer cards floating in the
  corner: they appear as a line over the input line, in the same words-only
  language as the rest of the app, and no longer cover the key hints.

### Fixed
- **"Create channel" in ⌘K made a private room without saying so.** It ran
  `/create` with no name, which is how the server makes an unlisted room —
  invisible to everyone else. There are now two entries: "create a room…",
  which waits for you to type a name, and "create a private room", which says
  what it does.

## [1.10.0] — 2026-08-06

### Changed
- **The desktop interface is new.** The sidebar, message pane and participant
  panel are gone. One layout now serves every screen — a row of entities,
  their details underneath, and a line to type into — with people (a room),
  rooms (the lobby) or setting groups standing in that row.
  - **People are their names.** No avatars or tiles: whoever speaks grows and
    brightens, everyone else recedes. Rows of names stagger so they never form
    a grid, and the type shrinks as the room fills. Past fourteen people only
    the recent voices stand; the quiet wait in a line below and are lifted back
    the moment they speak (switch it off in settings → look).
  - **What people say falls from them** and fades — across the room when it's
    quiet, one row down and faster when it's busy. The full log is still there
    under "what was said" (⌘J).
  - **Settings speak the same language.** Every control is a row of words where
    the lit one is chosen; fonts are shown in their own face, accent colours in
    their own colour. Global hotkeys are now caught — click the combo and press
    it, instead of typing `Alt+M` by hand. Typing in the input line searches all
    28 settings at once.
  - **Commands answer where you're looking.** `/help`, `/devices`, `/servers`
    and friends open the log; `/list` opens the lobby, since the rooms
    themselves are the answer; everything else — including errors from a
    mistyped command, which used to vanish silently — appears above the input
    line for a few seconds.
  - Rooms are shown by name: the wire's `pub-` prefix is hidden, and a private
    room carries a leading dot. Private rooms are still absent from the public
    list by design.
  - The input line no longer takes focus on its own, so space stays with
    push-to-talk; any letter you type hands focus over, Escape hands it back.

Voice, crypto, protocol and the terminal client are untouched — this release
changes only how the desktop app looks and is operated. Protocol stays at 3.

## [1.9.27] — 2026-06-30

### Added
- **Unquarantine command in the update toast.** Builds are unsigned, so macOS
  refuses to open a freshly downloaded app; the toast now shows the exact
  `xattr` command to run after replacing it.

## [1.9.26] — 2026-06-22

### Fixed
- **Windows autostart, again.** The saved setting is re-applied to the OS on
  every launch, so a stale `Run` path — left behind by a moved dev build or a
  different install location — heals itself. Task Manager's "disabled" shadow
  entry, which silently overrode the `Run` value, is cleared as well.
  macOS/Linux are unchanged.

## [1.9.25] — 2026-06-22

### Added
- **Room overlay over fullscreen games.** An optional click-through window
  that floats the roster and who's speaking on top of borderless-windowed
  games. Off by default; six edge positions, shown in-call or always. It is a
  pure subscriber to the same events the main window uses — the core gains
  nothing but a roster snapshot for hydration. Honest limit: borderless
  windowed only, exclusive fullscreen can't be overlaid without injection.

## [1.9.24] — 2026-06-19

### Changed
- **Frosted-glass system toasts.** Update and hot-plugged-device
  notifications share one translucent blurred panel with an accent glow that
  follows the chosen accent colour, and a fade-and-scale entrance that
  respects reduced-motion. They now stack in the corner instead of covering
  each other.

## [1.9.23] — 2026-06-18

### Added
- **Per-peer volume.** Turn one person up or down (0–200%) without touching
  anyone else. Applied on your machine before mixing — never sent to the relay
  or to the peer. Survives a rename, costs nothing at unity gain.
- Hotkey hints above the composer, with a show/hide toggle in settings.

### Changed
- Default window size 1200×800 (minimum 640×420).

## [1.9.22] — 2026-06-17

### Added
- **Command-mode indicator in the composer.** The border, focus ring and a
  left stripe change colour the moment your text reads as a command — accent
  for one the app knows, amber for one it doesn't — so you can tell it will be
  run as a command and not sent as chat.

## [1.9.21] — 2026-06-17

### Added
- **Avatars with a choice of style:** Truchet weave (default), mirrored pixel
  sigil, or fun-emoji faces, each in a squircle with a palette derived from
  the peer's key. The style is your own local preference and is never sent to
  anyone.

### Fixed
- **Persistent public channels survived an explicit `/leave`** — `leave_channel`
  was missing the guard that the other cleanup paths already had, so a
  pre-created room could be reaped when its last member left. Regression test
  added.

## [1.9.20] — 2026-06-16

### Added
- **Update notification.** On launch the desktop client checks GitHub Releases
  and, when a newer version exists, shows a dismissable toast with a "view
  release" button that opens the release page. The check is best-effort and
  silent on failure (offline clients are never nagged); drafts and pre-releases
  are ignored.

## [1.9.19] — 2026-06-16

### Fixed
- **Windows autostart toggle** failed with "The system cannot find the file
  specified (os error 2)". The bundled `auto-launch` 0.5 (pinned by
  `tauri-plugin-autostart`) *opens* `HKCU\…\Run` instead of creating it, so on
  profiles where that key is absent both enable and disable error out. Windows
  now manages the Run value directly (`create_subkey` is idempotent; delete
  tolerates an absent value; the exe path is quoted for paths with spaces); the
  plugin is still used on macOS/Linux.

### Added
- **Client version** shown at the bottom of the desktop Settings panel, and a
  `/version` (`/ver`) command in both clients that prints it to the log.

## [1.9.18] — 2026-06-15

### Added
- **HD voice + noise suppression** (voice-quality pack):
  - **Noise suppression** (RNNoise via `nnnoiseless`, opt-in): cleans keyboard /
    fan / background off the mic before encoding. Pure-Rust, no build deps;
    runs in the capture thread (480-sample frames, no per-frame allocation),
    applies live mid-call. Desktop toggle in Settings → Audio; `/denoise
    <on|off>` in both clients. Persisted.
  - **Higher Opus ceiling**: HIGH tier 32 → **64 kbit/s** (Opus selects 20 kHz
    fullband automatically at that rate), tiers rebalanced
    (40 / 24 / 12 kbit/s), and `signal=Voice` hints the encoder for speech.
    Audio bandwidth is left to Opus to pick from the bitrate (pinning it would
    hurt the low/loss tiers). Adaptive degradation on loss is unchanged.
- **Paranoid mode** (opt-in toggle, both clients): traffic-analysis resistance
  for voice. With it on, the client sends a **constant packet rate** of
  **flat-size** frames even during silence/mute, so the relay (or any on-path
  observer) can no longer infer *who is speaking when* from packet timing/size —
  the metadata leak that VAD/mute otherwise expose. Mute sends comfort silence
  (encoded zeros); the real mic never leaves while muted. Opus switches to
  CBR + DTX-off so silence frames don't collapse into tiny tell-tale packets.
  - Desktop: toggle in Settings → Audio; both clients: `/paranoid <on|off>`.
    Persisted; applies live mid-call (capture thread reads the flag per frame).
  - Zero wire/format change, zero server change, hot-path stays allocation-free;
    a normal-mode peer in the same channel is unaffected.
  - Honest scope: closes the **timing** leak fully and flattens size to CBR
    granularity. Not byte-exact constant size (residual CBR variance + tier
    changes remain) — strict padding needs a payload length field (future work).
    Cost: constant bandwidth even in silence. Labelled accordingly in the UI.

## [1.9.17] — 2026-06-15

### Added
- **Sound check (echo test)** in both clients: a 5 s live echo that sends your
  mic through the full pipeline (Opus → XChaCha20 → UDP → server → back →
  decrypt → decode → jitter → playback) so you hear yourself returned. Tests
  microphone, headphones, codec, crypto, jitter buffer **and** the round-trip
  to the server in one action. Desktop: a button in Settings → Audio with a
  live countdown and a verdict; both clients: `/echotest` (`/echo`).
  - The server stays a dumb relay: it mints an ephemeral, single-member
    `echo-<id>` channel, reflects the ciphertext back to the tester's
    hello-confirmed registered address, and never decrypts or stores anything.
    Reflecting to the registered address (not the raw UDP source) keeps a
    spoofed source from turning it into a reflection primitive.
  - New wire messages (additive, backward-compatible): `StartEchoTest` /
    `StopEchoTest` (client→server), `EchoTestReady` (server→client). Old
    servers ignore the request and the client times out with a clear message;
    `PROTOCOL_VERSION` unchanged.

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
