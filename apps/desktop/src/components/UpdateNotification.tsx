// Non-blocking toast shown when a newer GitHub release than this build exists.
// "View release" opens the release page in the default browser; "later"
// dismisses it for the session (the next launch re-checks).
//
// On macOS the build is unsigned, so a freshly-installed update is quarantined
// by Gatekeeper and refuses to launch. We surface the one-liner that clears the
// quarantine attribute right in the toast so the user isn't stranded after
// downloading the new version.

import { Show, createSignal } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { t } from "../lib/i18n";
import { isMac } from "../lib/platform";
import Toast from "./Toast";
import { Download } from "./Icons";

// `productName` in tauri.conf.json — the installed bundle is /Applications/tc_.app.
const UNQUARANTINE_CMD = "xattr -dr com.apple.quarantine /Applications/tc_.app";

export default function UpdateNotification() {
  const dismiss = () => update.updateAvailable(null);
  const [copied, setCopied] = createSignal(false);

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

  const copyCmd = async (e: MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(UNQUARANTINE_CMD);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      pushLog(`copy failed: ${err}`, "error");
    }
  };

  const body = (
    <>
      {t("update.hint")}
      <Show when={isMac}>
        <div class="mt-2">
          <div class="text-muted">{t("update.macUnsign")}</div>
          <button
            type="button"
            onClick={copyCmd}
            title={t("common.copy")}
            class="mt-1 w-full text-left font-mono text-[11px] px-2 py-1.5 rounded-md bg-hover border border-line text-text2 break-all hover:bg-accent-bg transition-colors"
          >
            {copied() ? t("common.copied") : UNQUARANTINE_CMD}
          </button>
        </div>
      </Show>
    </>
  );

  return (
    <Show when={state.updateAvailable}>
      {(info) => (
        <Toast
          icon={<Download size={18} />}
          title={t("update.available", { version: info().version })}
          body={body}
          actions={[
            { label: t("update.dismiss"), onClick: dismiss },
            { label: t("update.view"), onClick: view, primary: true },
          ]}
        />
      )}
    </Show>
  );
}
