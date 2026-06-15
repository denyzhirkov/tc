# `tc` — State Index

> Thin index — read first in a new session. Machine-of-record for task state is `tsk`. Deep history in `docs/state/*`.

**Phase:** Desktop client (Tauri + Solid) — feature-complete core, polishing OS-integration. Latest release **1.9.17**, protocol **3**.

## Recently closed (newest first)

- Sound check / echo test (`x6hyqs`, **v1.9.17**, shared+server+both clients): 5 s live echo through the server — mic → Opus → XChaCha20 → UDP → server reflects → decode → playback, so you hear yourself back. Tests mic, headphones, codec, crypto, jitter **and** the round-trip in one action. Desktop button in Settings → Audio (countdown + verdict) and `/echotest` in both clients. Server stays a dumb relay: ephemeral single-member `echo-<id>` channel, reflects ciphertext to the tester's *registered* address (no reflection vector), never decrypts/stores. New additive wire msgs `StartEchoTest`/`StopEchoTest`/`EchoTestReady`; `PROTOCOL_VERSION` unchanged. e2e test proves byte-identical reflection to the sender. See `decisions.md` → "Sound check / echo test".
- Phantom-wave root cause + voice UX (`c8y7zv`, **v1.9.16**): Solid `setState` merge (keys never deleted) was THE phantom-wave cause — speaker levels now go through `reconcile()`; VAD pre-roll (100 ms ring) fixes clipped/scratchy speech onsets; server broadcasts `ChannelList` to all clients when a public channel is created; default VAD level 15 in both clients.
- Device UX & stability pack (`kihuol`, **v1.9.15**, client+UI): capture/playback split into independent halves behind swappable ring-buffer slots (no mic = listen-only, no output = speak-only, neither = chat works, audio self-heals); calls follow the system default device; settings device changes apply mid-call; hot-plug "use it now?" toast (configured device reconnecting switches back silently); `no mic`/`no audio out` chips in VoiceStrip.
- Audio device lifecycle resilience (`hqvzm9`, **v1.9.14**): macOS bundle was missing `NSMicrophoneUsageDescription` — the installed app silently got mic zeros and never prompted (dev worked via the terminal's TCC grant); added `src-tauri/Info.plist` + `Entitlements.plist`. Stream-death detection (error callbacks + capture starvation watchdog) → `VoiceHandle::is_healthy()`; the desktop voice actor supervises and rebuilds the pipeline (throttled, retries failed starts); stale device names fall back to the system default instead of failing the join.
- Jitter forward-gap resync (`3b6rh2`, **v1.9.13**, client-only): a loss burst longer than the ring (15 pkts = 300 ms, e.g. the deaf window while registration re-heals) wedged the jitter buffer forever — audio silent, speaking indicator alive. Resync after `JITTER_RESYNC_AFTER` consecutive out-of-window packets; single strays can't reset a healthy stream.
- Self-healing UDP registration (`80wpaa`, **v1.9.12**, server + client): idle keepalive now re-hellos with the real token (token-0 kept NAT mappings alive but not the relay registration — a NAT rebind left the client permanently deaf, proven via nettop on the live app socket vs a fresh probe listener on the same NAT); server cleanup no longer expires tokens of live in-channel sessions.
- Phantom speaking indicator fix (`u6l9cm`, **v1.9.11**): `level_pump` now emits `voice_stopped` on the active→inactive flip, `left_channel` clears voice state, a 1s frontend watchdog zeroes meters if `voice_level` stalls; `input_peak` resets to 0 on mute; the settings test tone hard-stops inside the audio callback (no endless beep on a wedged stream teardown). Proven by a probe listener: the channel carried zero packets while the UI showed a "constant stream" — the waves were a frozen frontend snapshot, not network traffic.
- Concurrent-speakers fix (`oyvhmx`, **v1.9.10**): per-sender jitter buffer + Opus decoder (`HashMap<name, SenderStream>` in `client/voice.rs`) with PCM mixing into playback; the old shared buffer reset on every interleaved packet → two people speaking at once = silence for everyone. Plus an echo guard: inbound packets carrying our own name are dropped (a loopback through a peer no longer plays your voice back to you).
- tc:// invite pack (`nmhah9`…`go7i87`): single-instance forwarding (Win/Linux), буферизация invite при холодном старте (`deeplink::Gate` + `take_pending_invite`), `register_all()` для dev/AppImage, `/invite` в обоих клиентах (`tc_shared::invite_url`), join без setTimeout, confirm-диалог для серверов вне registry (анти drive-by).
- Кросс-платформенные тесты (`fxwmpb`): CI-матрица [ubuntu/macos/windows] для `cargo test`, dual-stack e2e по `::1`, behavioral `BatchSender` (реально исполняет sendmmsg/sendmsg_x/fallback). Матрица сразу поймала 2 реальных падения (macOS CMake, Windows UDP-readiness). Релиз **v1.9.8**.
- One-way-audio fix pack (Windows): canonical-IP compare + идемпотентный re-ACK при UDP-регистрации (`36eq37`), UDP target из фактического TCP peer IP (`6l8z9n`), NO-RX статус + фоновый re-hello (`p5az9y`), UDP keepalive в VAD-тишине (`enyns4`); `/show_dev_logs` в desktop (`x46y07`)
- `120068e` perf: LTO release profile + macOS `sendmsg_x` batched UDP fan-out
- P8.3 CI matrix mac/win/linux + GitHub Releases
- P8.1/P8.2 ASCII identicon from pubkey + terminal styling pass
- P7.1/P7.2 Text channel mode + DM view in desktop pane
- P6.x Server list + favourites, `tc://` deeplink, local text history per (server, channel)

## In progress

- `joiuup` — **P5.4** Tray icon + autostart + OS notifications (desktop)

## Up next

- EPIC 15 — Forward secrecy for voice keys (`66b9lj`: KeyRotation message, periodic server rotation, dual-key transition)
- Масштабируемость сервера (`aw76wp`)
- Аутентификация и авторизация (`r8td09`)
- NAT traversal (`9bfq6b`)
- Видеозвонки 1:1 desktop через WebRTC во webview (`m5ddm4`) — анализ/контракт зафиксирован, реализация не начата; перед стартом Фаза 0 (проба webview-WebRTC на macOS/Linux) + ADR-0003
- Автообновления (`1aw8jk`) — desktop updater (Tauri, нужен ключ подписи + GH-секреты) + CLI notify-only; контракт зафиксирован, реализация не начата

## Pointers

- Architecture & conventions → `CLAUDE.md`
- Tooling / contracts / security baseline → `docs/STACK.md`
- Wire format → `PROTOCOL.md`
- Architectural truths & gotchas → `docs/state/decisions.md`
- Test surface → `docs/state/testing.md`
- Resume & maintenance protocol → `docs/state/resume-protocol.md`
- Closed history → `docs/state/closed-cli-core.md`, `docs/state/closed-desktop.md`
- ADRs → `docs/adr/`

_Last updated: 2026-06-11 (tc:// invite pack)._
