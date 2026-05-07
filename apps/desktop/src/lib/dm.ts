import { cmd } from "./tauri";
import { pushLog, state, update } from "./store";

export async function openDm(pubkeyHex: string, name: string) {
  update.openDm(pubkeyHex, name);
  try {
    const lines = await cmd.getDmHistory(pubkeyHex, 100);
    if (lines.length > 0) {
      for (const l of lines)
        update.pushDmLog(`${l.from}: ${l.text}`, "dm", l.ts_unix);
    } else {
      update.pushDmLog(`DM with ${name} — no history yet`, "system");
    }
  } catch (e) {
    pushLog(`dm open failed: ${e}`, "error");
  }
}

export function closeDm() {
  update.closeDm();
}

export async function sendDm(text: string) {
  if (!state.dm) return;
  try {
    await cmd.sendDmTo(state.dm.pubkey_hex, text);
    update.pushDmLog(
      `you: ${text}`,
      "dm",
      Math.floor(Date.now() / 1000),
    );
  } catch (e) {
    update.pushDmLog(`error: ${e}`, "error");
  }
}
