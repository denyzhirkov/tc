# Closed history — Desktop app (Tauri + Solid)

History of the desktop arc (`apps/desktop`). The desktop is a Tauri 2 + SolidJS frontend over the same `tc-client` core. Source of record is `tsk`.

## P0–P1 — Core reuse & identity

- **P0.1** `tc-client` → `[lib]` + `[bin]` so the desktop can link the core.
- **P1.1** Ed25519 identity carried in `Hello`; server keeps a pubkey→name map.
- **P1.2** Local identity store in the core.
- **P1.3** `DirectMessage` protocol + server routing (DMs between peers).

## P2 — Scaffold & bridge

- **P2.1** `apps/desktop` scaffold: pnpm + Vite + Solid + TS + Tailwind.
- **P2.2** `tc-desktop` crate added to the workspace (reuses `tc-shared` + `tc-client`).
- **P2.3** `AppCore` bridge + `app_status` command.

## P3 — Voice & UI bridge

- **P3.1** Voice-path Tauri commands.
- **P3.2** Tauri events for server push.
- **P3.3** Solid single-pane layout; **P3.4** in-window command bar (later superseded by the ⌘K palette).

## P4 — Audio

- **P4.1** Audio device commands (list/select input/output).
- **P4.2** `audio_level` event + ASCII meter UI.
- **P4.3** Test-signal playback.

## P5 — Voice modes & OS integration

- **P5.1** Voice-mode switch (VAD / push-to-talk / open).
- **P5.2** Global hotkeys (`tauri-plugin-global-shortcut`).
- **P5.3** Settings persistence in the OS config dir.
- **P5.4** *(in progress)* Tray icon + autostart + OS notifications.

## P6 — Servers, deeplinks, history

- **P6.1** Server list + favourites + last channel.
- **P6.2** `tc://` URL scheme + deeplink handler.
- **P6.3** Local text history per `(server, channel)`.

## P7 — Channels & DMs

- **P7.1** Text-channel mode in the pane; **P7.2** DM view.

## P8 — Polish & release

- **P8.1** ASCII identicon from pubkey (spiral phyllotaxis from fingerprint).
- **P8.2** Terminal-styling pass — later a full redesign (1.8.0): quiet sans aesthetic, click-driven sidebar (servers/channels/DMs), grouped messages with day separators, `IN CALL` peer panel with voice bars + fingerprints, `CONNECTED` strip; ⌘K palette; `@`-mention popover opening a DM; sidebar voice-activity wave; Settings tweaks (theme/accent/density/typeface).
- **P8.3** CI matrix mac/win/linux → GitHub Releases (unified CLI + desktop release workflow).

## Later (1.9.x)

- Server perf + security + loadtest harness (1.9.0).
- Desktop auto-reconnect with backoff + disconnect button (1.9.1); i18n en/ru + auto-mute on channel join (1.9.2).
- Release plumbing: zip `.app` artifacts, `CMAKE_POLICY_VERSION_MINIMUM` for desktop build, version sync across `package.json`/`tauri.conf.json`.

## One-way-audio fix pack + dev logs (2026-06-10)

Диагноз полевого бага (Windows: «клиент 1 слышит клиента 2, обратно — нет»): UDP-регистрация клиента 2 молча проваливалась — клиент после неудачных hello «proceeding anyway» (отправка форвардится relay'ем без валидации отправителя, приём невозможен — адреса нет в peer cache). Корневые причины и фиксы:

- **`36eq37`** server: `register_udp_by_token` сравнивает IP в canonical-форме (`::ffff:a.b.c.d` ≡ v4); токен больше не сжигается ни на mismatch, ни на успехе (идемпотентный re-ACK для потерянных ACK; TTL обновляется), re-hello с нового порта заменяет stale-адрес в канале (NAT rebind).
- **`6l8z9n`** client: UDP target теперь — фактический peer IP TCP-сессии (`ServerConnection::peer_addr`), сокет биндится той же address family. Убран повторный DNS-резолв (TCP по IPv6 + UDP по IPv4 давал постоянный reject).
- **`p5az9y`** client: провал handshake больше не молчит — `VoiceHandle::is_registered`, статус `NO RX (registering…)` в TUI status bar / VoiceStrip, фоновый re-hello с backoff (1s→5s, `UDP_REHELLO_*`); первый входящий пакет тоже подтверждает регистрацию.
- **`enyns4`**: UDP keepalive (hello с токеном 0) раз в 20 с при VAD-тишине (`UDP_KEEPALIVE_INTERVAL_SECS`) — NAT/Windows-Firewall flow не протухает; сервер считает их в `udp_keepalives`, не форвардит. Не ломает старые сервера (для них это invalid hello).
- **`x46y07`** desktop: `/show_dev_logs` (alias `/devlogs`) — toggle-стрим DEBUG+ событий `tracing` (только `tc*`-таргеты, throttle 25/с) в основную ленту через слой `dev_log::DevLogLayer` + Tauri-событие `dev_log`.

## tc:// invite pack (2026-06-11)

P6.2 оставил deeplink в состоянии «работает на macOS при тёплом старте». Доведено до продуктового состояния (`nmhah9`…`go7i87`):

- **`nmhah9`** single-instance: `tauri-plugin-single-instance` (feature `deep-link`) первым плагином — на Windows/Linux клик по ссылке при запущенном приложении больше не спавнит второй процесс; колбэк показывает/фокусирует окно, URL из argv re-триггерит `on_open_url`.
- **`ts4fuh`** холодный старт: `deeplink::Gate` буферизует invite до готовности фронта; команда `take_pending_invite` (take-семантика + флаг ready), `drainPendingInvite()` в `App.tsx` — invite имеет приоритет над auto-connect.
- **`y7tuy0`** `register_all()` в setup под `cfg(linux | (debug, windows))` — dev-режим и AppImage.
- **`uko25t`** `/invite` (`/i`) в обоих клиентах: `tc_shared::invite_url` (дефолтный порт опускается), desktop копирует в clipboard, TUI печатает; help-тексты обновлены.
- **`ziitag`** убран `setTimeout(400)` перед join — `connect` резолвится после установления TLS, join упорядочен в том же потоке.
- **`go7i87`** confirm-диалог (`InviteConfirm.tsx`) для серверов вне registry — защита от drive-by connect (утечка IP/pubkey + TOFU-pin чужого серта); известные сервера коннектятся без трения. i18n en/ru.
