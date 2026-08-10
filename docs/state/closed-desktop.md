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

## «Room» UI — full frontend replacement (`fhq8x7`, 2026-08-06)

The desktop UI was a Discord clone: 240 px sidebar + message pane + 224 px peer
panel. Diagnosis before touching anything — voice, the actual product, lived in
a 100 px strip at the bottom-left while text took 70 % of the screen; voice
controls were spread over three places (`VoiceStrip`, `ChannelHeader`,
`Peers`); speaking state was drawn three different ways; and E2E/TOFU — the
reason the product exists — was a 10 px monospace line nobody reads.

Replaced wholesale with one layout that serves every screen: **a row of
entities, their details underneath, and a line to type into**. The row holds
people (a room), rooms (the lobby), or setting groups (settings).

- **People are their names.** No avatars, no tiles, no roster panel. Volume
  becomes `transform: scale` + opacity, so a speaker grows without pushing
  neighbours around (changing `font-size` would reflow the rank every frame).
- **Ranks alternate counts** (4-3-4, not 4-4-3) so neighbouring rows fall into
  each other's gaps instead of forming a grid. Type shrinks with headcount;
  past 14 standing, only recent voices stay and the quiet wait in a line below,
  lifted back the moment they speak (≥2.5 s between swaps, you're never
  displaced). Toggle: settings → look → "when it's crowded".
- **Speech falls from the mouth that said it** and fades. Distance and life
  scale with the crowd (≈14 s across the room alone, ≈7 s one rank down when
  full); one line per voice at a time.
- **Colour is identity**, derived from the name via FNV → hue. Caveat: the
  roster carries names, not pubkeys, so renaming recolours a peer — the
  fingerprint in settings remains the thing that proves who someone is.
- **The room's tint follows the loudest voice**, damped hard in JS (hue drifts
  the short way round the wheel over ~4 s, brightness ~3 s, max 5.5 % alpha,
  repaint only past a visible delta). A CSS transition can't do this — the var
  is rewritten every frame, so the transition never gets to start.
- **All 28 settings** moved into the same grammar: one control, a row of words
  where the lit one is chosen. Toggles became pairs of antonyms, device pickers
  and enums the same row, and only gain/volume/VAD kept a continuous control
  (12 ticks). Fonts are set in their own face, accents in their own colour.
  Search across every group from the input line, since 28 controls beat memory.
- **Hotkeys are caught, not typed** — click the combo, press it; ⌫ clears.
- **The input line doesn't autofocus**: space belongs to push-to-talk, and any
  printable key hands focus over (Escape hands it back).
- **Command output is routed** (`runCommand` wraps `execute`): commands whose
  only result is text open "what was said"; `/list` opens the lobby, because
  the rooms themselves are the answer; everything else — and every error —
  echoes for six seconds above the input line. Before this, a mistyped command
  wrote to a log the new UI never showed.

Removed: `Sidebar`, `ChannelHeader`, `Pane`, `Peers`, `VoiceStrip`,
`VoiceWave`, `Composer`, `Settings`, `Popover`, `lib/fp.ts`.
Added: `components/room/*` (Shell, Cast, Falling, TopLine, Lobby,
SettingsScene, HistoryScene, Line, Echo, words) + `lib/{scene,stage,tint,rooms}.ts`.
Kept untouched: `Palette` (⌘K), `InviteConfirm`, `DevicePrompt`,
`UpdateNotification`, `Identicon` (still drawn by the game overlay).
Rust: not a line — ADR-0001 held.

## Voice trace — a string under every name (`n4vpum`, 2026-08-10)

Speech already fell from the mouth that said it; the voice itself did not. A
speaking person only grew and brightened, which reads as presence but carries
no history — you could not see that someone had just finished a sentence.

Now a thin string hangs under each name, and speaking sends a wave down it,
away from the mouth: wide when loud, always calming toward the far end, ~2.4 s
of voice on the string at once. It hangs in the same column as the falling
text, below it in paint order, so words fall along the string of the voice that
said them.

- **The sine is the carrier, the voice is the amplitude.** Levels reach the
  frontend five times a second (`level_pump.rs`, `TICK = 200 ms`) and are
  smoothed again by `Cast`'s frame loop. Drawing that as an oscilloscope would
  have meant inventing oscillation the data never had; driving a wave with it
  is honest and reads as sound at a glance.
- **Phase is bound to the sample, not to the clock.** Each committed sample
  carries phase `commits · DPHI`, so as it moves down the ring its crest moves
  with it — a burst of voice leaves as one travelling crest. A clock-driven
  sine would have made every string wobble in place, which reads as decoration.
- **Fed from the one loop that already knows.** `Cast`'s rAF loop passes each
  peer's smoothed level into a `Trace`; a second rAF, or a second reading of
  `state.speakers`, would have created a rival source of truth about how loud
  someone is.
- **The string exists only while a voice is on it.** It arrives in 240 ms and
  leaves over three seconds — long enough for the wave still in flight to ride
  out, so it is gone before it would hang straight. Presence is a CSS
  transition on the path's opacity, toggled by one class; the frame loop only
  says *when*, never animates the fade itself. The 0.4 s delay before the fade
  doubles as hysteresis, so a pause between words barely dims it.
- **Zero while quiet, smooth while speaking.** `Trace.step()` returns `null`
  once the string has settled, so a silent room costs nothing; while audible it
  returns a fresh path every frame, interpolated between the 100 ms samples —
  without that sub-tick term the wave advanced in ten visible steps a second.
- **`vector-effect="non-scaling-stroke"` is load-bearing.** The viewBox is
  normalised (`-1 0 2 1`) and stretched to the element, so pixel size — and
  therefore the crowd rule — stays CSS's business; a scaled stroke would smear
  into a wedge.
- **Crowd and reduced-motion follow the existing rules.** Past 8 standing the
  string drops a rank (44 px), narrows and recedes, mirroring how falling text
  shortens; with `prefers-reduced-motion` the frame loop never runs, so no path
  is ever drawn (a CSS rule states it anyway).
- **A DM has no string** — `levelOf` returns 0 there, and a DM is a written
  room.

Added `lib/trace.ts`; touched `components/room/Cast.tsx` (three inserts) and
`styles.css`. No new dependency, no Rust, no wire change.

## Falling text lives out its fall + a Tailwind name collision (`5x7lo4`, `cygn5s`, v1.12.1, 2026-08-10)

Two owner-reported bugs in the room shell, both frontend-only.

**Every line finishes its fall** (`5x7lo4`). Sending a second message wiped the
first out of the air: `drop()` did `live.get(who)?.remove()`, the "one line per
voice at a time" rule set out when the «Room» UI landed. Overruled — a sentence
vanishing the instant you send the next reads as loss, not as motion. Lines from
one voice now stack, each running its animation to the end. A newcomer starts
below the voice's previous line when their boxes would overlap, compared as
**geometry rather than elapsed time**: in a crowded room a line drops one rank
over ~7 s, so it keeps covering its starting point far longer than a lone
speaker's does, and an age threshold tuned for the open-room case would fail
exactly where lines are tightest. The new line is appended, measured and
corrected inside one synchronous task, so nothing paints between insert and
correction and the line never visibly jumps. `--drop` is measured from where the
line actually starts, so a queued line still lands inside the room; cleanup
clears the host element, since stacked lines outlive their `live` entry.

**`.fixed` was Tailwind's** (`cygn5s`). Settings rows in the key group drew the
value on top of the words beside it — server address over "copy it", key over
its "copy it". Not a layout bug: `styles.css` defined `.fixed { font-size;
color }` for a non-interactive value, Tailwind's utilities compile into the same
sheet, and `.fixed { position: fixed }` is one of them. Both rules applied and
the value left the flow. Diagnosed by rebuilding the row markup against the
*built* stylesheet in a browser and reading computed styles — `position` came
back `fixed`. The collision ran both ways: `Palette` and `InviteConfirm` want
the utility (`class="fixed inset-0 …"`) and were silently inheriting the custom
font-size and muted colour. Renamed to `.value` with the reason recorded at the
rule. Every hand-written class in `styles.css` was then checked against the
built sheet; this was the only genuine collision, the rest being the project's
own media-query overrides. **Standing lesson: a hand-written class here shares a
namespace with every Tailwind utility, and a collision merges silently.**

## The window you couldn't get back (`z9wczl`, 2026-08-10)

Closing the window on macOS left an app that was running, showed nothing, and
could not be recalled — "as if the system is showing an invisible window".

Diagnosed on the installed build rather than from the code: the process was
alive, `System Events` reported **0 windows**, and `open -a` changed neither.
The owner's saved setting is `close_to_tray = true`, so the window was **hidden,
not destroyed** — the app was behaving as configured. What was missing is that
`.run(generate_context!())` carried no `RunEvent` handler, so
`RunEvent::Reopen` (Dock click, `open -a`) was ignored and the only route back
was the tray icon, which is not where anyone looks first.

A second gap sat behind it, waiting for anyone on the default setting: with
`close_to_tray = false` the close was never intercepted, the window was
destroyed, and on macOS the process outlives its last window — after which the
tray's "Show window" is a no-op too, because it resolves the window by label and
no longer finds one. The setting promised "closing the window quits" and
produced a zombie instead.

Fix, three parts. `CloseRequested` on `main` always calls `prevent_close` and
then either hides or exits, so the main window is never destroyed while the app
lives. `RunEvent::Reopen` is handled on macOS. And `close_to_tray` became an
`Arc<AtomicBool>` shared with a `CloseToTray` Tauri state, because the close
handler runs on the main thread and cannot await the core mutex — the previous
`try_lock().unwrap_or(false)` degraded to `false` under contention, which was
survivable when `false` only destroyed the window and would have been an
unexplained quit once `false` means exit.

The default also moved: `close_to_tray` now defaults to **hide on macOS**, quit
elsewhere. The red button closes a window on macOS, it does not quit an app, and
a voice chat that ends the call on a stray click is the wrong surprise. An
explicitly saved choice still wins.

Verified end to end against a dev build, all three branches: setting on → window
hides and a Dock-tile click brings it back (window count 0 → 1); setting off →
process exits cleanly on close; no saved value on macOS → hides. The owner's
settings were restored afterwards.
