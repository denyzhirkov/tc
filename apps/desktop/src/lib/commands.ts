// Slash-command parser. Mirrors the TUI vocabulary so muscle memory carries over.
import { cmd } from "./tauri";
import { pushLog, state, update } from "./store";
import { isMac } from "./platform";

// Every recognised command head (incl. aliases). Keep in sync with the switch
// in `runCommand` — used by the composer to colour the input when the user is
// typing a command the app actually understands.
export const KNOWN_COMMANDS = new Set<string>([
  "/server", "/s", "/reconnect", "/r", "/disconnect", "/dc",
  "/list", "/ls", "/create", "/connect", "/c", "/join", "/j",
  "/leave", "/l", "/name", "/n", "/mute", "/m", "/mode", "/dm",
  "/devices", "/dev", "/input", "/in", "/output", "/out", "/gain",
  "/vol", "/vad", "/test", "/echotest", "/echo", "/paranoid", "/denoise",
  "/hotkey", "/hk", "/notify", "/autostart", "/tray", "/servers", "/srv",
  "/forget", "/fav", "/history", "/settings", "/set", "/show_dev_logs",
  "/devlogs", "/backup", "/restore", "/invite", "/i", "/version", "/ver",
  "/help", "/h",
]);

export type InputKind = "chat" | "command" | "unknown-command";

/// Classify the composer's current text for visual feedback. `#…` (join
/// shortcut) and a recognised `/cmd` are "command"; an unrecognised `/word`
/// is "unknown-command"; everything else is plain chat. Matching is
/// case-sensitive, mirroring `runCommand`'s exact-head switch.
export function classifyInput(raw: string): InputKind {
  const t = raw.trimStart();
  if (!t) return "chat";
  if (t.startsWith("#")) return t.length > 1 ? "command" : "unknown-command";
  if (!t.startsWith("/")) return "chat";
  const head = t.split(/\s+/)[0];
  return KNOWN_COMMANDS.has(head) ? "command" : "unknown-command";
}

export async function runCommand(raw: string) {
  const input = raw.trim();
  if (!input) return;

  // Plain text → DM if in a DM thread, otherwise chat in current channel.
  if (!input.startsWith("/") && !input.startsWith("#")) {
    if (state.dm) {
      const { sendDm } = await import("./dm");
      await sendDm(input);
      return;
    }
    if (!state.channel) {
      pushLog("not in a channel — use /join <id> or /create", "error");
      return;
    }
    pushLog(`you: ${input}`, "chat", Math.floor(Date.now() / 1000));
    try {
      await cmd.sendChat(input);
    } catch (e) {
      pushLog(`send failed: ${e}`, "error");
    }
    return;
  }

  // #abc shortcut for /join
  if (input.startsWith("#")) {
    const id = input.slice(1).trim();
    if (!id) return pushLog("usage: #<channel_id>", "error");
    return safe(() => cmd.joinChannel(id));
  }

  const [head, ...rest] = input.split(/\s+/);
  const arg = rest.join(" ").trim();

  switch (head) {
    // Server connection
    case "/server":
    case "/s":
      if (!arg) return pushLog(`server: ${state.serverAddr}`, "system");
      return safe(() => cmd.connect(arg));
    case "/reconnect":
    case "/r":
      return safe(() => cmd.connect(state.serverAddr));
    case "/disconnect":
    case "/dc":
      return safe(() => cmd.disconnect());
    // Channels
    case "/list":
    case "/ls":
      return safe(() => cmd.listChannels());
    case "/create":
      return safe(() => cmd.createChannel(arg || null));
    case "/connect":
    case "/c":
    case "/join":
    case "/j":
      if (!arg) return pushLog("usage: /connect <channel>", "error");
      return safe(() => cmd.joinChannel(arg));
    case "/leave":
    case "/l":
      return safe(() => cmd.leaveChannel());
    case "/name":
    case "/n":
      if (!arg) return pushLog("usage: /name <name>", "error");
      return safe(() => cmd.setName(arg));
    case "/mute":
    case "/m": {
      const next = !(state.status?.muted ?? false);
      await safe(() => cmd.setMute(next));
      pushLog(next ? "muted" : "unmuted", "system");
      await refreshStatus();
      return;
    }
    // (clicked-button mute lives in actions.ts and stays silent.)
    case "/mode": {
      const m = arg.toLowerCase();
      if (m !== "open" && m !== "vad" && m !== "ptt") {
        return pushLog("usage: /mode <open|vad|ptt>", "error");
      }
      await safe(() => cmd.setVoiceMode(m));
      pushLog(`voice mode → ${m}`, "system");
      await refreshStatus();
      return;
    }
    case "/dm": {
      if (rest.length < 2) return pushLog("usage: /dm <pubkey-or-fingerprint> <text>", "error");
      const ref = rest[0];
      const text = rest.slice(1).join(" ");
      try {
        const full = await cmd.resolvePeer(ref);
        if (!full) {
          pushLog(`could not resolve peer '${ref}' — use full 64-char pubkey for first DM`, "error");
          return;
        }
        await cmd.sendDmTo(full, text);
        pushLog(`(dm to ${ref}) you: ${text}`, "dm");
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    }
    case "/devices":
    case "/dev":
      return printDevices();
    case "/input":
    case "/in":
      return setInput(arg);
    case "/output":
    case "/out":
      return setOutput(arg);
    case "/gain":
      return setGain(arg);
    case "/vol":
      return setVol(arg);
    case "/vad":
      return setVad(arg);
    case "/test":
      return safe(() => cmd.playTestSignal());
    case "/echotest":
    case "/echo":
      if (!state.status?.connected)
        return pushLog("not connected — /server <addr> first", "error");
      if (state.channel)
        return pushLog("leave the channel first (/leave) before a sound check", "error");
      return safe(() => cmd.startEchoTest());
    case "/paranoid": {
      const v = arg.toLowerCase();
      const cur = state.status?.paranoid ?? false;
      const next = v === "" ? !cur : v === "on" || v === "true" || v === "1";
      const valid = v === "" || next || v === "off" || v === "false" || v === "0";
      if (!valid) return pushLog("usage: /paranoid <on|off>", "error");
      await safe(() => cmd.setParanoid(next));
      if (state.status) state.status.paranoid = next;
      pushLog(
        next
          ? "paranoid mode on — constant packet rate hides speaking patterns from the server"
          : "paranoid mode off",
        "system",
      );
      return;
    }
    case "/denoise": {
      const v = arg.toLowerCase();
      const cur = state.status?.denoise ?? false;
      const next = v === "" ? !cur : v === "on" || v === "true" || v === "1";
      const valid = v === "" || next || v === "off" || v === "false" || v === "0";
      if (!valid) return pushLog("usage: /denoise <on|off>", "error");
      await safe(() => cmd.setDenoise(next));
      if (state.status) state.status.denoise = next;
      pushLog(next ? "noise suppression on (RNNoise)" : "noise suppression off", "system");
      return;
    }
    case "/hotkey":
    case "/hk":
      return handleHotkey(rest);
    case "/notify":
      return handleToggle(arg, "notifications", cmd.setNotifications);
    case "/autostart":
      return handleToggle(arg, "autostart", cmd.setAutostart);
    case "/tray":
      return handleToggle(arg, "close to tray", cmd.setCloseToTray);
    case "/servers":
    case "/srv":
      return printServers(arg);
    case "/forget":
      if (!arg) return pushLog("usage: /forget <addr>", "error");
      try {
        await cmd.forgetServer(arg);
        pushLog(`forgot ${arg}`, "system");
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    case "/fav": {
      const a = (rest[0] ?? "").trim();
      const onoff = (rest[1] ?? "on").toLowerCase();
      if (!a) return pushLog("usage: /fav <addr> [on|off]", "error");
      try {
        await cmd.setServerFavourite(a, onoff !== "off");
        pushLog(`fav ${a} → ${onoff !== "off" ? "on" : "off"}`, "system");
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    }
    case "/history": {
      if (arg === "clear") {
        if (!state.channel) return pushLog("not in a channel", "error");
        try {
          await cmd.clearHistory(state.serverAddr, state.channel);
          pushLog("history cleared for this channel", "system");
        } catch (e) {
          pushLog(`error: ${e}`, "error");
        }
        return;
      }
      if (!state.channel) return pushLog("not in a channel", "error");
      try {
        const lines = await cmd.getHistory(state.serverAddr, state.channel, 100);
        for (const l of lines)
          pushLog(
            `${l.from}: ${l.text}`,
            l.kind === "dm" ? "dm" : "chat",
            l.ts_unix,
          );
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    }
    case "/settings":
    case "/set":
      update.showSettings(!state.showSettings);
      return;
    case "/show_dev_logs":
    case "/devlogs": {
      const next = !state.devLogs;
      try {
        await cmd.setDevLogs(next);
        update.devLogs(next);
        pushLog(
          next
            ? "dev logs on — streaming DEBUG+ into this feed (/show_dev_logs to stop)"
            : "dev logs off",
          "system",
        );
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    }
    case "/backup":
      try {
        const json = await cmd.exportSettings();
        try {
          await navigator.clipboard.writeText(json);
          pushLog(
            `backup copied to clipboard (${json.length} chars) — paste somewhere safe`,
            "system",
          );
        } catch {
          // Clipboard API may not be granted in some Tauri configs;
          // fall back to dumping the JSON into the log so the user can copy it manually.
          pushLog("clipboard unavailable — JSON dumped below", "system");
          for (const line of json.split("\n")) pushLog(line, "system");
        }
      } catch (e) {
        pushLog(`backup failed: ${e}`, "error");
      }
      return;
    case "/restore": {
      const text = arg
        ? arg
        : await navigator.clipboard.readText().catch(() => "");
      if (!text || !text.trim()) {
        return pushLog(
          "usage: /restore <json>  — or copy a backup to clipboard and run /restore",
          "error",
        );
      }
      try {
        await cmd.importSettings(text);
        pushLog(
          "settings restored — reconnect or restart to apply everywhere",
          "system",
        );
        const refreshed = await cmd.status();
        update.status(refreshed);
        if (refreshed.server_addr) update.serverAddr(refreshed.server_addr);
      } catch (e) {
        pushLog(`restore failed: ${e}`, "error");
      }
      return;
    }
    case "/invite":
    case "/i":
      try {
        const link = await cmd.inviteLink();
        try {
          await navigator.clipboard.writeText(link);
          pushLog(`${link} — copied to clipboard`, "system");
        } catch {
          pushLog(link, "system");
        }
      } catch (e) {
        pushLog(`error: ${e}`, "error");
      }
      return;
    case "/version":
    case "/ver":
      return pushLog(`tc_ v${state.status?.version ?? "unknown"}`, "system");
    case "/help":
    case "/h":
      return printHelp();
    default:
      pushLog(`unknown command: ${head} (try /help)`, "error");
  }
}

async function printDevices() {
  try {
    const [ins, outs] = await Promise.all([
      cmd.listInputDevices(),
      cmd.listOutputDevices(),
    ]);
    pushLog("── input devices ──", "system");
    ins.forEach((d) => pushLog(`  ${d.index}. ${d.name}`, "system"));
    pushLog("── output devices ──", "system");
    outs.forEach((d) => pushLog(`  ${d.index}. ${d.name}`, "system"));
    pushLog("/input <n>  /output <n>   to select  ·  /test to verify", "system");
  } catch (e) {
    pushLog(`device list failed: ${e}`, "error");
  }
}

async function setInput(arg: string) {
  if (!arg) return safe(() => cmd.setInputDevice(null));
  const idx = parseInt(arg, 10);
  if (!Number.isFinite(idx)) return pushLog("usage: /input <index>", "error");
  try {
    const list = await cmd.listInputDevices();
    const dev = list.find((d) => d.index === idx);
    if (!dev) return pushLog(`no input device #${idx}`, "error");
    await cmd.setInputDevice(dev.name);
    pushLog(`input → ${dev.name} (re-join channel to apply)`, "system");
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

async function setOutput(arg: string) {
  if (!arg) return safe(() => cmd.setOutputDevice(null));
  const idx = parseInt(arg, 10);
  if (!Number.isFinite(idx)) return pushLog("usage: /output <index>", "error");
  try {
    const list = await cmd.listOutputDevices();
    const dev = list.find((d) => d.index === idx);
    if (!dev) return pushLog(`no output device #${idx}`, "error");
    await cmd.setOutputDevice(dev.name);
    pushLog(`output → ${dev.name} (re-join channel to apply)`, "system");
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

async function setGain(arg: string) {
  const n = parseIntInRange(arg, 0, 200, "gain");
  if (n === null) return;
  await safe(() => cmd.setInputGain(n));
  pushLog(`mic gain ${n}%`, "system");
  await refreshStatus();
}

async function setVol(arg: string) {
  const n = parseIntInRange(arg, 0, 200, "vol");
  if (n === null) return;
  await safe(() => cmd.setOutputVolume(n));
  pushLog(`output volume ${n}%`, "system");
  await refreshStatus();
}

async function setVad(arg: string) {
  const n = parseIntInRange(arg, 0, 100, "vad");
  if (n === null) return;
  await safe(() => cmd.setVadLevel(n));
  pushLog(n === 0 ? "vad off" : `vad ${n}/100`, "system");
  await refreshStatus();
}

function parseIntInRange(arg: string, min: number, max: number, label: string): number | null {
  const n = parseInt(arg, 10);
  if (!Number.isFinite(n) || n < min || n > max) {
    pushLog(`usage: /${label} <${min}-${max}>`, "error");
    return null;
  }
  return n;
}

async function refreshStatus() {
  try {
    const s = await cmd.status();
    if (!state.status) return;
    Object.assign(state.status, s);
  } catch {}
}

async function printServers(filter: string) {
  try {
    const list = await cmd.listServers();
    if (list.length === 0) return pushLog("no servers yet — /server <addr> to add one", "system");
    const sorted = [...list].sort((a, b) => {
      // favourites first, then most recent
      if (a.favourite !== b.favourite) return a.favourite ? -1 : 1;
      return b.last_used_unix - a.last_used_unix;
    });
    pushLog("── known servers ──", "system");
    for (const s of sorted) {
      if (filter && !s.addr.includes(filter)) continue;
      const star = s.favourite ? "★" : " ";
      const ch = s.last_channel ? ` · last #${s.last_channel}` : "";
      const lbl = s.label ? ` (${s.label})` : "";
      pushLog(`  ${star} ${s.addr}${lbl}${ch}`, "system");
    }
    pushLog("/fav <addr> [on|off] · /forget <addr>", "system");
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

async function handleToggle(
  arg: string,
  label: string,
  apply: (enabled: boolean) => Promise<void>,
) {
  const v = arg.toLowerCase();
  const enabled = v === "on" || v === "true" || v === "1" || v === "yes";
  const valid = enabled || v === "off" || v === "false" || v === "0" || v === "no";
  if (!valid) {
    return pushLog(`usage: /${label.split(" ")[0]} <on|off>`, "error");
  }
  try {
    await apply(enabled);
    pushLog(`${label} → ${enabled ? "on" : "off"}`, "system");
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

async function handleHotkey(rest: string[]) {
  const sub = rest[0];
  if (sub === "list" || rest.length === 0) {
    try {
      const list = await cmd.listHotkeys();
      if (list.length === 0) {
        pushLog("no hotkeys bound", "system");
      } else {
        pushLog("── hotkeys ──", "system");
        for (const [a, k] of list) pushLog(`  ${a.padEnd(12)} ${k}`, "system");
      }
    } catch (e) {
      pushLog(`error: ${e}`, "error");
    }
    return;
  }
  if (sub === "unset") {
    if (!rest[1]) return pushLog("usage: /hotkey unset <action>", "error");
    await safe(() => cmd.unsetHotkey(rest[1]));
    pushLog(`hotkey ${rest[1]} cleared`, "system");
    return;
  }
  if (sub === "set") {
    if (rest.length < 3) {
      return pushLog(
        isMac
          ? "usage: /hotkey set <action> <combo>  e.g. /hotkey set ptt Alt+V"
          : "usage: /hotkey set <action> <combo>  e.g. /hotkey set ptt Ctrl+Alt+V",
        "error",
      );
    }
    const action = rest[1];
    const accel = rest.slice(2).join(" ");
    try {
      await cmd.setHotkey(action, accel);
      pushLog(`hotkey ${action} → ${accel}`, "system");
    } catch (e) {
      const msg = String(e);
      pushLog(`error: ${msg}`, "error");
      if (msg.includes("RegisterEventHotKey") || msg.toLowerCase().includes("register")) {
        pushLog(
          isMac
            ? "combo is taken by the OS or another app — try Alt+V, Alt+M, F19, etc."
            : "combo is taken by the OS or another app — try Ctrl+Alt+V, Ctrl+Alt+M, F19, etc.",
          "system",
        );
      }
    }
    return;
  }
  pushLog("usage: /hotkey list | set <action> <combo> | unset <action>", "error");
  pushLog("actions: mute, ptt, quick_join", "system");
  pushLog(
    isMac
      ? "safe macOS combos: Alt+V, Alt+M, Alt+J, F13-F19"
      : "safe combos: Ctrl+Alt+V, Ctrl+Alt+M, Ctrl+Alt+J, F13-F24",
    "system",
  );
}

async function safe(fn: () => Promise<void>) {
  try {
    await fn();
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

function printHelp() {
  for (const line of [
    "── commands ──",
    "  /server (/s) <ip>    set server address (reconnects)",
    "  /reconnect (/r)      reconnect to current server",
    "  /disconnect (/dc)    drop connection",
    "  /list (/ls)          list public channels",
    "  /create [name]       create channel (private if no name)",
    "  /connect (/c) <id>   join channel (alias /join /j, or just #<id>)",
    "  /leave (/l)          leave current channel",
    "  /name (/n) <name>    set display name",
    "  /mute (/m)           toggle mute",
    "  /mode <open|vad|ptt> capture mode (open = always send, ptt = hotkey)",
    "  /dm <pk-hex> <text>  direct message",
    "  /devices (/dev)      list audio devices",
    "  /input (/in) <n>     pick input device",
    "  /output (/out) <n>   pick output device",
    "  /gain <0-200>        mic gain (default 100)",
    "  /vol  <0-200>        output volume (default 100)",
    "  /vad  <0-100>        VAD level (0 = off)",
    "  /test                play 0.5s sine through current output",
    "  /echotest (/echo)    5s mic+connection check — hear yourself back via the server",
    "  /denoise <on|off>    RNNoise mic noise suppression",
    "  /paranoid <on|off>   hide speaking patterns from the server (constant packet rate)",
    "  /hotkey (/hk)        list / set / unset global hotkeys",
    isMac
      ? "    /hotkey set ptt Alt+V       /hotkey set mute Alt+M"
      : "    /hotkey set ptt Ctrl+Alt+V  /hotkey set mute Ctrl+Alt+M",
    isMac
      ? "    safe macOS combos: Alt+<key>, F13-F19  (Cmd+Space is taken by Spotlight)"
      : "    safe combos: Ctrl+Alt+<key>, F13-F24  (avoid Win+/Super+ keys — taken by the WM)",
    "  /servers (/srv) [q]  list known servers (★ = favourite)",
    "  /fav <addr> [on|off] mark server as favourite",
    "  /forget <addr>       drop a server from the registry",
    "  /invite (/i)         tc:// link to current server/channel (copies to clipboard)",
    "  /history [clear]     re-print channel history (or wipe for current channel)",
    "  /notify <on|off>     OS notifications on chat/peer-join when window unfocused",
    "  /autostart <on|off>  launch tc_ on system login",
    "  /tray <on|off>       hide to tray on close (instead of quit)",
    "  /settings (/set)     toggle settings panel (also click ⚙)",
    "  /show_dev_logs       toggle DEBUG+ log stream in this feed (alias /devlogs)",
    "  /backup              copy all settings + saved servers to clipboard as JSON",
    "  /restore [json]      apply a backup (clipboard if no arg). private key stays untouched",
    "  /version (/ver)      print the client version",
    "  /help (/h)           this help",
    "  <text>               chat in current channel",
  ]) {
    pushLog(line, "system");
  }
}
