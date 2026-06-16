// Non-blocking toast shown when a newer GitHub release than this build exists.
// "View release" opens the release page in the default browser; "later"
// dismisses it for the session (the next launch re-checks).

import { Show } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { t } from "../lib/i18n";

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
        <div class="fixed bottom-4 right-4 z-40 w-[320px] bg-surface border border-line rounded-xl shadow-2xl p-3">
          <div class="text-sm mb-1">
            {t("update.available", { version: info().version })}
          </div>
          <div class="text-xs text-muted mb-3">{t("update.hint")}</div>
          <div class="flex justify-end gap-2">
            <button
              class="px-3 py-1 text-sm rounded-lg border border-line text-text2 hover:bg-hover"
              onClick={dismiss}
            >
              {t("update.dismiss")}
            </button>
            <button
              class="px-3 py-1 text-sm rounded-lg bg-accent-bg text-text hover:opacity-90"
              onClick={view}
            >
              {t("update.view")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
