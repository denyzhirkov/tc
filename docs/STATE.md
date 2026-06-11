# `tc` — State Index

> Thin index — read first in a new session. Machine-of-record for task state is `tsk`. Deep history in `docs/state/*`.

**Phase:** Desktop client (Tauri + Solid) — feature-complete core, polishing OS-integration. Latest release **1.9.9**, protocol **3**.

## Recently closed (newest first)

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
