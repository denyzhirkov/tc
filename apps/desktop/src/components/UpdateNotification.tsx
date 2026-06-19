// Non-blocking toast shown when a newer GitHub release than this build exists.
// "View release" opens the release page in the default browser; "later"
// dismisses it for the session (the next launch re-checks).

import { Show } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { t } from "../lib/i18n";
import Toast from "./Toast";
import { Download } from "./Icons";

export default function UpdateNotification() {
  const dismiss = () => update.updateAvailable(null);

  const view = async () => {
    const info = state.updateAvailable;
    update.updateAvailable(null);
    if (!info) return;
    try {
      await cmd.openRelease(info.url);
    } catch (e) {
      pushLog(`error: ${e}`, "error");
    }
  };

  return (
    <Show when={state.updateAvailable}>
      {(info) => (
        <Toast
          icon={<Download size={18} />}
          title={t("update.available", { version: info().version })}
          body={t("update.hint")}
          actions={[
            { label: t("update.dismiss"), onClick: dismiss },
            { label: t("update.view"), onClick: view, primary: true },
          ]}
        />
      )}
    </Show>
  );
}
