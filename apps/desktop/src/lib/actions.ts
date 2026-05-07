// Reusable UI actions wired to backend commands.
// Used by both the command parser and clickable widgets.

import { cmd, type VoiceMode } from "./tauri";
import { pushLog, state, update } from "./store";

export async function refreshStatus() {
  try {
    // Always replace the whole object so Solid's store proxy notifies subscribers.
    // Object.assign on the existing proxy is unreliable for nested reactivity.
    update.status(await cmd.status());
  } catch {}
}

export async function toggleMute() {
  const next = !(state.status?.muted ?? false);
  try {
    await cmd.setMute(next);
    await refreshStatus();
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

/// Toggle between the two "live" modes. The third (open = always-send) is
/// reserved for settings — too easy to enable by accident from a click.
export async function toggleVadPtt() {
  const cur = (state.status?.voice_mode ?? "vad") as VoiceMode;
  const next: VoiceMode = cur === "ptt" ? "vad" : "ptt";
  try {
    await cmd.setVoiceMode(next);
    await refreshStatus();
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

export async function reconnect() {
  try {
    await cmd.connect(state.serverAddr);
  } catch (e) {
    pushLog(`reconnect failed: ${e}`, "error");
  }
}

export async function disconnect() {
  try {
    await cmd.disconnect();
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

export async function joinChannel(id: string) {
  // No-op if already in this channel — click handlers can stay simple.
  if (state.channel === id) return;
  try {
    // Leave the current channel first; the server rejects JoinChannel while
    // already in another. TCP order guarantees the server sees Leave→Join in
    // sequence, so no explicit wait is needed.
    if (state.channel) await cmd.leaveChannel();
    await cmd.joinChannel(id);
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

export async function leaveChannel() {
  try {
    await cmd.leaveChannel();
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

export async function connectTo(addr: string) {
  try {
    await cmd.connect(addr);
  } catch (e) {
    pushLog(`error: ${e}`, "error");
  }
}

export function toggleSettings() {
  update.showSettings(!state.showSettings);
}

export function closeSettings() {
  update.showSettings(false);
}
