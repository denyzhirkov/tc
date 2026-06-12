// Non-blocking toast for hot-plugged audio devices: "use it now? yes / no".
// "Yes" selects the device (applies mid-call immediately); "no" is remembered
// for the session so the same device doesn't nag again.

import { Show } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { dismissedDevices } from "../lib/wire";
import { t } from "../lib/i18n";

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
      pushLog(`error: ${e}`, "error");
    }
  };

  return (
    <Show when={state.devicePrompt}>
      {(p) => (
        <div class="fixed bottom-4 right-4 z-40 w-[320px] bg-surface border border-line rounded-xl shadow-2xl p-3">
          <div class="text-sm mb-1">
            {t(
              p().kind === "input" ? "device.added_input" : "device.added_output",
              { name: p().name },
            )}
          </div>
          <div class="text-xs text-muted mb-3">{t("device.use_now")}</div>
          <div class="flex justify-end gap-2">
            <button
              class="px-3 py-1 text-sm rounded-lg border border-line text-text2 hover:bg-hover"
              onClick={decline}
            >
              {t("device.no")}
            </button>
            <button
              class="px-3 py-1 text-sm rounded-lg bg-accent-bg text-text hover:opacity-90"
              onClick={accept}
            >
              {t("device.yes")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
