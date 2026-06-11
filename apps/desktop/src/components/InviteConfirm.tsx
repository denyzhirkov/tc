// Confirmation for tc:// invites pointing at servers not in the registry —
// connecting reveals the user's IP and pubkey and TOFU-pins the server's
// cert, so it must be a deliberate choice, not a drive-by from a web link.

import { Show, onCleanup, onMount } from "solid-js";
import { pushLog, state, update } from "../lib/store";
import { acceptInvite } from "../lib/wire";
import { t } from "../lib/i18n";

export default function InviteConfirm() {
  const decline = () => {
    update.invitePrompt(null);
    pushLog(t("log.invite_declined"), "system");
  };

  const accept = async () => {
    const p = state.invitePrompt;
    update.invitePrompt(null);
    if (p) await acceptInvite(p);
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && state.invitePrompt) decline();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <Show when={state.invitePrompt}>
      {(invite) => (
        <div
          class="fixed inset-0 z-50 bg-black/40 flex items-center justify-center"
          onMouseDown={(e) => {
            if (e.currentTarget === e.target) decline();
          }}
        >
          <div class="w-[420px] max-w-[90vw] bg-surface border border-line rounded-xl shadow-2xl p-4">
            <div class="text-sm font-medium mb-2">{t("invite.title")}</div>
            <div class="text-xs text-muted mb-3">{t("invite.body")}</div>
            <div class="font-mono text-sm mb-4">
              {invite().addr}
              <Show when={invite().channel}>
                <span class="text-muted"> #{invite().channel}</span>
              </Show>
            </div>
            <div class="flex justify-end gap-2">
              <button
                class="px-3 py-1.5 text-sm rounded-lg border border-line text-text2 hover:bg-hover"
                onClick={decline}
              >
                {t("invite.cancel")}
              </button>
              <button
                class="px-3 py-1.5 text-sm rounded-lg bg-accent-bg text-text hover:opacity-90"
                onClick={accept}
              >
                {t("invite.connect")}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
