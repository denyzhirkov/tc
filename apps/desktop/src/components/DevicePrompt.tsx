// Non-blocking toast for hot-plugged audio devices: "use it now? yes / no".
// "Yes" selects the device (applies mid-call immediately); "no" is remembered
// for the session so the same device doesn't nag again.

import { Show } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { dismissedDevices } from "../lib/wire";
import { t } from "../lib/i18n";
import Toast from "./Toast";
import { Mic, Speaker } from "./Icons";

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
        <Toast
          icon={p().kind === "input" ? <Mic size={18} /> : <Speaker size={18} />}
          title={t(
            p().kind === "input" ? "device.added_input" : "device.added_output",
            { name: p().name },
          )}
          body={t("device.use_now")}
          actions={[
            { label: t("device.no"), onClick: decline },
            { label: t("device.yes"), onClick: accept, primary: true },
          ]}
        />
      )}
    </Show>
  );
}
