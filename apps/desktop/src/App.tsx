import { onCleanup, onMount } from "solid-js";
import Shell from "./components/room/Shell";
import Palette from "./components/Palette";
import InviteConfirm from "./components/InviteConfirm";
import { drainPendingInvite, fetchInitialStatus, subscribeAll } from "./lib/wire";
import { pushLog, state, update } from "./lib/store";
import { cmd } from "./lib/tauri";
import { installTheme } from "./lib/theme";
import { goto } from "./lib/scene";
import { shortcut } from "./lib/platform";

export default function App() {
  installTheme();

  onMount(async () => {
    // Right-click otherwise reaches the webview and it answers with a browser's
    // menu — on Windows the full "Back / Reload / Save as / Inspect", which is
    // the technology showing through an interface that has no buttons in it at
    // all. Nothing here wants a second mouse button yet.
    const noMenu = (e: Event) => e.preventDefault();
    window.addEventListener("contextmenu", noMenu);
    onCleanup(() => window.removeEventListener("contextmenu", noMenu));

    pushLog(`tc_ — ${shortcut("k")}`, "system");
    const unlisten = await subscribeAll();
    onCleanup(() => unlisten.forEach((u) => u()));
    await fetchInitialStatus();
    goto(state.channel || state.dm ? "room" : "lobby");

    // `wire` asks for the channel list when the connection *flips* to
    // connected. If the backend was already connected when this window came
    // up — a reload, a reopened window, HMR in dev — that flip never happens
    // and the lobby would sit empty next to a server full of rooms.
    if (state.conn === "connected") cmd.listChannels().catch(() => {});

    // A cold-start tc:// invite takes priority over auto-connect.
    const invited = await drainPendingInvite();
    if (!invited && state.conn === "disconnected") {
      try {
        await cmd.connect(state.serverAddr);
      } catch (e) {
        pushLog(`auto-connect failed: ${e}`, "error");
      }
    }

    // Best-effort and out-of-band: offline / API hiccups are swallowed by the
    // backend (returns null) so they never surface as errors.
    cmd
      .checkForUpdate()
      .then((info) => info && update.updateAvailable(info))
      .catch(() => {});
  });

  return (
    <div class="app">
      <Shell />
      <Palette />
      <InviteConfirm />
    </div>
  );
}
