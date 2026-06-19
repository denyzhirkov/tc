import { Show, onCleanup, onMount } from "solid-js";
import Sidebar from "./components/Sidebar";
import ChannelHeader from "./components/ChannelHeader";
import Pane from "./components/Pane";
import Peers from "./components/Peers";
import Settings from "./components/Settings";
import Palette from "./components/Palette";
import InviteConfirm from "./components/InviteConfirm";
import DevicePrompt from "./components/DevicePrompt";
import UpdateNotification from "./components/UpdateNotification";
import { drainPendingInvite, fetchInitialStatus, subscribeAll } from "./lib/wire";
import { pushLog, state, update } from "./lib/store";
import { cmd } from "./lib/tauri";
import { installTheme } from "./lib/theme";
import { shortcut } from "./lib/platform";

export default function App() {
  installTheme();

  onMount(async () => {
    pushLog(`tc_ — press ${shortcut("k")} to jump`, "system");
    const unlisten = await subscribeAll();
    onCleanup(() => unlisten.forEach((u) => u()));
    await fetchInitialStatus();
    // A cold-start tc:// invite takes priority over auto-connect.
    const invited = await drainPendingInvite();
    if (!invited && state.conn === "disconnected") {
      try {
        await cmd.connect(state.serverAddr);
      } catch (e) {
        pushLog(`auto-connect failed: ${e}`, "error");
      }
    }
    // The sidebar's channel list is now refreshed by `wire.ts` whenever the
    // connection_state flips to "connected".

    // Update check is best-effort and out-of-band: offline / API hiccups are
    // swallowed by the backend (returns null) so they never surface as errors.
    cmd
      .checkForUpdate()
      .then((info) => info && update.updateAvailable(info))
      .catch(() => {});
  });

  return (
    <div class="h-full flex bg-bg text-text">
      <Sidebar />
      <main class="flex-1 min-w-0 flex flex-col">
        <ChannelHeader />
        <div class="flex-1 min-h-0 flex">
          <Pane />
          <Show when={state.channel && !state.dm}>
            <Peers />
          </Show>
        </div>
      </main>
      <Show when={state.showSettings}>
        <Settings />
      </Show>
      <Palette />
      <InviteConfirm />
      {/* Corner stack for non-blocking system toasts (newest at the bottom). */}
      <div class="fixed bottom-4 right-4 z-40 flex flex-col-reverse items-end gap-2">
        <DevicePrompt />
        <UpdateNotification />
      </div>
    </div>
  );
}
