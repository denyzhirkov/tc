// A newer release exists — said in words over the input line, not in a card.
// "Later" dismisses for the session; the next launch re-checks.
//
// On macOS the build is unsigned, so a freshly-installed update is quarantined
// by Gatekeeper and refuses to launch. The one-liner that clears it is offered
// here, since being stranded after downloading is worse than a second line.

import { Show, createSignal } from "solid-js";
import { cmd } from "../lib/tauri";
import { pushLog, state, update } from "../lib/store";
import { t } from "../lib/i18n";
import { isMac } from "../lib/platform";
import { Ask } from "./room/words";

// `productName` in tauri.conf.json — the installed bundle is /Applications/tc_.app.
const UNQUARANTINE_CMD = "xattr -dr com.apple.quarantine /Applications/tc_.app";

export default function UpdateNotification() {
  const [copied, setCopied] = createSignal(false);

  const dismiss = () => update.updateAvailable(null);

  /// `openRelease` just hands a URL to the OS browser, so the same command
  /// serves both the release page and a direct asset link.
  const openUrl = async (url: string) => {
    update.updateAvailable(null);
    try {
      await cmd.openRelease(url);
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  const copyCmd = async () => {
    try {
      await navigator.clipboard.writeText(UNQUARANTINE_CMD);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  return (
    <Show when={state.updateAvailable}>
      {(info) => (
        <>
          <Ask
            answers={[
              // The build for *this* machine when the release carries one —
              // an arm64 Mac shouldn't have to pick its own dmg off a list.
              ...(info().download_url
                ? [
                    {
                      label: t("update.download"),
                      onClick: () => openUrl(info().download_url!),
                      loud: true,
                    },
                    { label: t("update.notes"), onClick: () => openUrl(info().url) },
                  ]
                : [
                    {
                      label: t("update.view"),
                      onClick: () => openUrl(info().url),
                      loud: true,
                    },
                  ]),
              { label: t("update.dismiss"), onClick: dismiss },
            ]}
          >
            {t("update.available", { version: info().version })}
            <Show when={info().download_name}>
              {" "}
              <span class="ask-quiet" title={info().download_name!}>
                {info().download_name}
              </span>
            </Show>
          </Ask>
          <Show when={isMac}>
            <Ask answers={[{ label: copied() ? t("common.copied") : t("common.copy"), onClick: copyCmd }]}>
              <span class="ask-quiet">{t("update.macUnsign")}</span>{" "}
              <span class="ask-cmd">{UNQUARANTINE_CMD}</span>
            </Ask>
          </Show>
        </>
      )}
    </Show>
  );
}
