// A microphone or a pair of headphones was just plugged in — asked in words
// over the input line. "No" is remembered for the session so the same device
// doesn't nag again.

import { Show } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { dismissedDevices } from "../lib/wire";
import { t } from "../lib/i18n";
import { Ask } from "./room/words";

export default function DevicePrompt() {
  const decline = () => {
    const p = state.devicePrompt;
    if (p) dismissedDevices.add(`${p.kind}:${p.name}`);
    update.devicePrompt(null);
  };

  const accept = async () => {
    const p = state.devicePrompt;
    update.devicePrompt(null);
    if (!p) return;
    try {
      if (p.kind === "input") await cmd.setInputDevice(p.name);
      else await cmd.setOutputDevice(p.name);
      pushLog(t("device.switched", { name: p.name }), "system");
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  return (
    <Show when={state.devicePrompt}>
      {(p) => (
        <Ask
          answers={[
            { label: t("device.yes"), onClick: accept, loud: true },
            { label: t("device.no"), onClick: decline },
          ]}
        >
          {t(p().kind === "input" ? "device.added_input" : "device.added_output", {
            name: p().name,
          })}{" "}
          <span class="ask-quiet">{t("device.use_now")}</span>
        </Ask>
      )}
    </Show>
  );
}
