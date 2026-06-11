import { Show, onCleanup, onMount } from "solid-js";
import Sidebar from "./components/Sidebar";
import ChannelHeader from "./components/ChannelHeader";
import Pane from "./components/Pane";
import Peers from "./components/Peers";
import Settings from "./components/Settings";
import Palette from "./components/Palette";
import InviteConfirm from "./components/InviteConfirm";
import { drainPendingInvite, fetchInitialStatus, subscribeAll } from "./lib/wire";
import { pushLog, state } from "./lib/store";
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
    </div>
  );
}
