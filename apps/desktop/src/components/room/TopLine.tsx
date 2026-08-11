// Rooms are words too. The line rhymes with the row of names below it: the
// place you're in is lit, the rest are dim, and one that has voices in it
// breathes. Private channels (no `pub-` prefix) carry a leading dot.

import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { pushLog, state } from "../../lib/store";
import { joinChannel } from "../../lib/actions";
import { openDm } from "../../lib/dm";
import { openPalette } from "../../lib/palette";
import { cmd, type DmPeerView } from "../../lib/tauri";
import { goto, toggle, view } from "../../lib/scene";
import { isPrivateRoom, roomLabel } from "../../lib/rooms";
import { t } from "../../lib/i18n";
import { Word } from "./words";

export default function TopLine() {
  const [dms, setDms] = createSignal<DmPeerView[]>([]);

  const refresh = async () => {
    try {
      setDms(await cmd.listDmPeers());
    } catch {
      setDms([]);
    }
  };

  onMount(() => {
    void refresh();
    // No event carries "a DM peer appeared", so poll gently while mounted.
    const id = setInterval(refresh, 4000);
    onCleanup(() => clearInterval(id));
  });

  /// What a room word does depends on where you already are. Somewhere else —
  /// go there. Here, but looking at settings or history — come back. Here and
  /// looking at it — the word has nothing left to do, so it hands you the link
  /// to bring someone else in. Same `tc://` link as `/invite`, and the echo
  /// above the line reports it, because that is where this interface puts the
  /// result of anything you asked for.
  const enter = async (id: string) => {
    if (state.channel !== id || state.dm) {
      goto("room");
      return void joinChannel(id);
    }
    if (view() !== "room") return goto("room");
    try {
      const link = await cmd.inviteLink();
      await navigator.clipboard.writeText(link);
      pushLog(`${link} — ${t("common.copied")}`, "system");
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  /// The room you are standing in is the one you can hand out.
  const shareable = (id: string) =>
    state.channel === id && !state.dm && view() === "room";

  const connWord = () => {
    switch (state.conn) {
      case "connected":
        return t("top.sealed");
      case "connecting":
        return t("top.connecting");
      case "reconnecting":
        return t("top.reconnecting");
      default:
        return t("top.offline");
    }
  };

  return (
    <div class="top">
      <For each={state.channelList}>
        {(ch) => (
          <Word
            current={view() === "room" && state.channel === ch.channel_id && !state.dm}
            title={
              shareable(ch.channel_id)
                ? t("top.share_room")
                : t("top.in_room", { count: ch.participant_count })
            }
            onClick={() => void enter(ch.channel_id)}
          >
            {roomLabel(ch.channel_id)}
          </Word>
        )}
      </For>

      {/* The room we're in that the public list doesn't carry: a private one
          (the server never announces those), or a public one created since
          the last list arrived. */}
      <Show
        when={
          state.channel &&
          !state.channelList.some((c) => c.channel_id === state.channel)
        }
      >
        <Word
          current={view() === "room" && !state.dm}
          title={
            shareable(state.channel!)
              ? t("top.share_room")
              : isPrivateRoom(state.channel!)
                ? t("top.private_room")
                : t("top.in_room", { count: state.participants.length })
          }
          onClick={() => void enter(state.channel!)}
        >
          {roomLabel(state.channel!)}
        </Word>
      </Show>

      <For each={dms()}>
        {(p) => (
          <Word
            tone="quiet"
            current={state.dm?.pubkey_hex === p.pubkey_hex}
            title={p.pubkey_hex.slice(0, 16)}
            onClick={() => {
              goto("room");
              void openDm(p.pubkey_hex, p.name);
            }}
          >
            @{p.name}
          </Word>
        )}
      </For>

      <Word tone="quiet" onClick={() => openPalette("/create ")}>
        {t("top.new_room")}
      </Word>

      <span class="top-spacer" />

      <Word
        tone="quiet"
        current={view() === "history"}
        onClick={() => toggle("history")}
      >
        {t("top.history")}
      </Word>
      <Word
        tone="quiet"
        current={view() === "settings"}
        onClick={() => toggle("settings")}
      >
        {t("top.settings")}
      </Word>
      <Word
        tone="quiet"
        title={t("top.switch_server")}
        onClick={() => openPalette("/server ")}
      >
        {state.serverAddr}
      </Word>
      <span
        class="sealed"
        classList={{ off: state.conn !== "connected" }}
        title={state.conn === "connected" ? t("top.sealed_hint") : undefined}
      >
        {connWord()}
      </span>
    </div>
  );
}
