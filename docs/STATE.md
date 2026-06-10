# `tc` — State Index

> Thin index — read first in a new session. Machine-of-record for task state is `tsk`. Deep history in `docs/state/*`.

**Phase:** Desktop client (Tauri + Solid) — feature-complete core, polishing OS-integration. Latest release **1.9.7**, protocol **3**.

## Recently closed (newest first)

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

## Pointers

- Architecture & conventions → `CLAUDE.md`
- Tooling / contracts / security baseline → `docs/STACK.md`
- Wire format → `PROTOCOL.md`
- Architectural truths & gotchas → `docs/state/decisions.md`
- Test surface → `docs/state/testing.md`
- Resume & maintenance protocol → `docs/state/resume-protocol.md`
- Closed history → `docs/state/closed-cli-core.md`, `docs/state/closed-desktop.md`
- ADRs → `docs/adr/`

_Last updated: 2026-06-10 (one-way-audio fix pack + /show_dev_logs)._
