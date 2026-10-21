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

## The accent gets a job: the colour of "chosen" (`la6ylw`, v1.13.0, 2026-08-10)

Began as a question — what does the colour choice in settings give us? — and the
answer was: nothing. `theme.ts` wrote `--c-accent` on every change and **no rule
read it**. `git log -S 'var(--c-accent'` put the death at `cfe2d9b`, where the
frosted-glass toasts were deleted; the knob outlived what it drove. The only
pixels it moved were the four option words in its own row, painted by `Opts`.

Deleted it first, then the owner proposed painting the selected item in the top
line. Right place, wrong mechanism. Right because rooms are chrome rather than
people, so an accent there does not compete with identity colour, and the room
wash is capped at 5.5% alpha so it does not interfere. Wrong because the
contrast budget was already spent: unselected words sit at **4.81:1** against
the background, barely over the 4.5 that 11px text needs, so "selected" was
obliged to be near-white. Measured, plain text scores **3.25** against the
unselected grey, a real hue scores **1.46–2.31**, and a hue held to 3.05
collapses to a 5% tint (`#e2e1e4` for indigo — a colour nobody can see). Dimming
the unselected words to buy room drops them to 3.36:1 or 2.30:1, below
legibility. The obstacle was arithmetic, not taste.

Resolution: the hue does not carry the state — **weight** does, and the hue
rides on top. Choosing an accent adds `font-weight: 600` to chosen words and
`700` to the lit settings group. Colour-blind readers keep a non-colour signal,
unselected words keep their legibility, and the accent gets to be an actual
colour. `plain` is the default and adds no weight, because it trades nothing
away.

Applied at the three places the interface says *chosen*: `.top .word-on`,
`.word-on` in every control, and `.settings .name:not(.dim)` — the settings root
gained a `settings` class so the rule cannot reach the room or the lobby. People
and rooms are exempt by construction: their colour is identity, set inline from
the name, and inline wins. `slate` was dropped, since a grey accent cannot be
told from plain text however it is tuned — the very defect that got the old
setting deleted.

Shipped hues, each clearing 4.5:1 against its own background: dark `#8a97ea`,
`#5fd39b`, `#ef8b5c`; light `#2a3a9e`, `#1d6b46`, `#9c4a1e`.

Verified in Chrome against the built stylesheet — all eight theme×accent
combinations reporting colour and weight on freshly created nodes, plus
screenshots of dark/indigo and light/rust. Two harness traps cost time and are
worth remembering: a stale cached `app.css` (cache-bust the link), and CSSOM
walking — in Chrome a plain style rule exposes an **empty `cssRules`** because of
CSS nesting, so recursing before reading `selectorText` silently finds nothing
and makes a working stylesheet look broken.

## A voice can be liquid (`t77mj3`, v1.14.0, 2026-08-11)

The string had been the only way a voice could look since `n4vpum`. The owner
wanted a second: a lava lamp, with the drops falling out of the speaking name
the way spoken words already fall.

It cost one module and one `<Show>`, because the shape was already right. The
slot under a name was a single element (`.trace`), fed by the single frame loop
in `Cast` that knows how loud everyone is. Swapping the medium inside that slot
is a setting; growing a second animation loop, or a second reading of
`state.speakers`, would have been the mistake.

- **The drops are CSS, not a simulation.** `lib/lava.ts` decides only *when* a
  drop is shed and how big it is — from the same smoothed level the string
  gets. Everything after birth is one keyframed animation per drop, removed on
  `animationend`; nothing about a drop is computed per frame. This is the same
  trick the falling text has always used (`Falling.tsx`), and it is why fourteen
  liquid names cost what fourteen strings cost.
- **A lava lamp that falls.** Real ones rise. Here they sink, because in this
  room everything a person says falls — drops in the same column as the words,
  under the same mask, in the same identity colour.
- **Merging without a filter graph.** The signature of liquid is two bodies
  running together. A gooey SVG filter (`feGaussianBlur` + alpha contrast) does
  that properly but is a per-element filter pass on HTML in WKWebView — a
  portability bet for decoration. Instead the softness lives in the drop's own
  rim (a radial gradient fading to transparent) with 0.6px of blur on the
  column. Enough blur to merge outright would have washed the identity colour
  out, and colour is who is speaking.
- **The painter is owned, not shared.** Switching the setting disposes one
  branch and creates the other, and Solid does not order the two — so each
  painter only retires its own entry in the map (`if (painters.get(name) ===
  paint)`). Without the guard, flipping the setting silently unregisters the
  painter that just registered, and the voice goes dead until the next re-render.
- **Lengthening the column is not a CSS-only change.** The reach went from
  ~140px to ~300px, shared by both looks so switching never changes how much of
  the room a voice takes. For the drops that is one variable. For the string it
  is not: 2.4 s of voice stretched over twice the distance is the same three
  crests, spaced twice as far apart and travelling twice as fast. `SAMPLES` went
  24 → 40, so **a longer string holds more voice** rather than the same voice
  drawn thin; the fade-out followed (2.6 s → 3.8 s) to keep the promise that a
  wave in flight rides out before the string disappears.

Verified the way `cygn5s` established: the real `styles.css`, built, served over
localhost, with both looks side by side under a synthetic voice envelope. Worth
remembering — `requestAnimationFrame` does not run in a background tab, so a
preview that samples the DOM from `setTimeout` reports "nothing is happening"
for a page that is working perfectly.

## Sound settings you can actually aim, and a device that leaves (`75xum2`, `4kwyen`, `s4evgw`, v1.15.0, 2026-08-11)

Three complaints from one screenshot of the sound settings, plus a question
that turned out to be a UI gap rather than a feature request.

**The scale could not be aimed.** Twelve ticks over 0–200 is 16.7% a tick, so
any particular percentage was luck. Twenty ticks (row 81 → 137px, ~69%) puts a
tick at a tenth, and `−`/`+` at the ends move two at a time; the arrow keys took
the same fine step, since jumping a whole tick was the same defect by another
route. The height ramp is now derived from the tallest tick rather than the
index — otherwise a longer row also grows upward.

**Rows hugged the left.** `.field` is a `160px | 1fr` grid inside a full-width
`.detail`, so every row spanned the window and the content sat left of centre
with dead space to the right. Bounded to 680px and centred with auto margins —
as a *block*, not per row: centring each row on its own content moves the
label/value boundary from row to row, and that seam is what the eye reads down.

**A device that goes away.** Unplug a headset and it stayed in the list until
restart. Two independent causes, either sufficient: `Supervisor::tick` diffed
the device name sets every 2s but only iterated `difference(&known)` in the
arrival direction, so departures were computed and discarded; and `DeviceRow`
read the list through `createResource` once at mount with no refetch, so an
event would not have reached it anyway. Now the snapshot carries
`devices_changed` for movement in either direction and `level_pump` emits it
**before** the idle `continue` — a list goes stale just as easily with no call
running, and that is precisely when someone is in settings choosing one. The
chosen-but-absent device is kept in the row, disabled and marked: `on_new_device`
already re-selects a configured device silently when it returns, so resetting to
the system default would have made that behaviour pointless, and a row with
nothing lit reads as broken.

**"Can we have per-person volume?"** — it existed everywhere except within
reach. `peer_gain.rs` applies a per-sender factor on the receive path before
mixing (ArcSwap: lock-free reads on the drain round, copy-on-write on the rare
write), `peer_volumes` persists, `set_peer_volume`/`list_peer_volumes` were
registered, `state.peerVolumes` was already hydrated in `wire.ts` — and nothing
rendered or wrote it. The TUI had `/volume <name> <pct>`; the desktop had no
door. So the whole story is UI: a click on a name opens the same meter under
that person, one at a time, self-closing 6s after the last touch (each nudge
resets it). Own name and DMs excluded. A name off 100% carries a `°`, because
the setting outlives the room and the app — without the mark you forget you
turned someone down and conclude their microphone is bad.

Worth keeping: **before building a feature in this repo, check whether the core
already does it.** Three of the four items above were UI-only, and the fourth
was two missing lines in a poller that had been diffing the right data all along.

## Two small truths about the composer and the top line (v1.15.1, 2026-08-11)

**Hints that hid.** The key row under the composer was `Show when={hints && !value()}` —
it left as soon as you typed, taking both the reference and the layout with it.
Fixed by keeping the row and dimming what stopped being true instead: `Shell`'s
key handler bails on `isTyping`, so bare letters (`m`, `space`) are shortcuts
only while the line is *unfocused*. Two consequences worth keeping: the predicate
is **focus, not emptiness** (a focused-but-empty line already eats `m`), and the
row must not change size — a map that redraws while you read it is worse than a
dim line.

**A word whose job depends on where you stand.** The room word in the top line
had two jobs (go there / come back from settings) and a third state where it had
none: you are in that room and looking at it. That state now copies the `tc://`
invite. The result goes to the echo above the input line — both because that is
the rule for command results here, and because swapping the word for "copied"
would shove every other room in the line sideways for a second and a half. The
hover title changes only in the state where the click actually copies, so the
overload is visible before you commit to it.

## The volume knob rises out of the name (`7wgyjw`, v1.16.0, 2026-08-11)

The click-to-open knob from `s4evgw` lasted one release. Hovering is the right
verb for something you reach for mid-conversation — but hovering needs a delay
(0.5s in, 1.5s out), because a cursor crosses names on its way somewhere else
and a control with no wait ambushes everyone it passes.

- **Handlers on `.person`, not on `.name`.** The knob covers the name, so a
  listener on the name would treat the move toward "+" as a departure. Anything
  that overlays its own trigger has this bug waiting for it.
- **The animation could not live in CSS.** `Cast`'s frame loop writes every
  name's `opacity` and `transform` each frame from its voice level, and inline
  style beats a stylesheet — a CSS transition on the name would be overwritten
  sixty times a second. So each name carries a `sunk` factor smoothed in the
  loop and *multiplied into* its live values: `opacity * (1 - depth)`,
  `scale * (1 - 0.26·depth)`, plus a blur written only while depth > 0. The two
  movements compose instead of fighting. Same reason the dimming of everyone
  else is on `.person` (a parent) rather than `.name`: nesting multiplies.
- **A reverse animation needs the element to outlive the decision.** Closing
  sets `surfaced = false` and only unmounts `TUNE_FADE` (280ms) later, matching
  the CSS transition. Unmounting immediately gives a knob that vanishes while
  the name is still rising — which reads as a glitch, not a return.
- **No panel, but not nothing.** The owner rejected the bordered panel; the
  knob is still wider than a name with neighbours close by, so the ticks landed
  on other people's letters. Resolution: a radial pool of `--c-bg` with no edge
  at all, sized past the knob and fading out well before it ends — the dark the
  knob rose from, rather than a lid over the room.
- **`prefers-reduced-motion` has no frame loop**, so nothing sinks the name out
  of the way. There, the tuned name is `visibility: hidden` — the one property
  the loop's inline `opacity` cannot overrule.

Two rules generalised to every meter in the app: `cursor: pointer` (nothing is
dragged — each tick is a click and the ends are words), and a 26×26 hit box on
`−`/`+` with negative margins so the enlarged target costs the row no width.

**Rejoining replayed the room's past as falling text.** `Falling` guards its
mount (`seen = last id`), which covers the first render — but a rejoin appends
the saved history to a log the component is *already* watching, so fifty stored
lines arrived as fifty new ids and dropped simultaneously into an illegible
pile. The fix is semantic rather than a batch-size heuristic: every live line is
stamped from the local clock the moment it arrives (`wire.ts` for incoming,
`commands.ts` for your own), while history carries its original `ts_unix`, so a
line older than `JUST_SAID` (5s) is something read out of storage and does not
fall. A chat line with no stamp at all still falls — that would be a bug worth
seeing rather than swallowing.

## Room looks: score and talking heads

- **Score and heads** (`uercuu`, `g931tf` = `q298jm` + `fvgtta`, unreleased): two alternative room looks beside the default names. The score came from a design round on the canvas prototype (https://claude.ai/artifact/DocNv4nnSJdkyT2UjKwUAR): the names room shows the present, the score shows who spoke when — overlaps, pauses, who has been quiet — with chat as lyrics under the sender's staff and a scroll back through two minutes. It is one DPR-correct canvas plus a names column of real buttons (volume knob, your acts), fed by an app-wide recorder. Review caught the "back to now" button clipped out of busy rooms, a rejoining speaker never lighting up, the "all" crowd overflowing the input line and quiet typists never getting a staff. Heads asked for real lip sync, which the 5 Hz packet-size proxy could not give, so the core now measures each peer's loudness from the rendered frame and streams it at 30 Hz without locking `AppCore` — which also made the old levels honest and forced a retune: one gate at the source plus a peak-held envelope keeps the default names look steady instead of throbbing per syllable. Faces are generated line art (head, eyes, brows, hair, mouth) in the identity hue; mouths follow raw frame loudness, eyes blink, brows lift on loud syllables. Reduced motion keeps only the mouth. Not yet seen in the real app with live speech — needs a manual pass in `tauri dev`.

