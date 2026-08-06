// No room: the rooms themselves become the cast. Same layout, same type —
// only the entities in the row have changed.
//
// The server's channel list carries a headcount but not a roster, so a room
// says how many are inside rather than who. Naming them would need a protocol
// change, and the relay deliberately knows as little as possible.

import { For, Show, createSignal, onMount } from "solid-js";
import { state } from "../../lib/store";
import { joinChannel } from "../../lib/actions";
import { openDm } from "../../lib/dm";
import { openPalette } from "../../lib/palette";
import { goto } from "../../lib/scene";
import { tint } from "../../lib/tint";
import { roomLabel } from "../../lib/rooms";
import { cmd, type DmPeerView } from "../../lib/tauri";
import { t } from "../../lib/i18n";
import { Note } from "./words";

export default function Lobby() {
  const [dms, setDms] = createSignal<DmPeerView[]>([]);

  onMount(async () => {
    // The room list is kept fresh by `wire`: on connect, on leaving a channel,
    // and by the server's own broadcast when someone creates a public room.
    // Asking again here would just duplicate the request we already sent.
    try {
      setDms(await cmd.listDmPeers());
    } catch {
      setDms([]);
    }
  });

  const enter = (id: string) => {
    goto("room");
    void joinChannel(id);
  };

  return (
    <div class="cast" style={{ "--nameSize": "26px" }}>
      <Show
        when={state.conn === "connected"}
        fallback={<Note>{t("lobby.offline")}</Note>}
      >
        {/* Rooms are set in the same big type as people, so leaving a call
            would otherwise look like nothing happened. This says where you
            are, and the smaller size keeps the two states apart. */}
        <div class="standing-out">{t("lobby.not_in_room")}</div>
        <div class="ranks">
          <div class="rank">
            <For each={state.channelList}>
              {(ch) => (
                <div class="person">
                  <button
                    class="name"
                    style={{ color: tint(ch.channel_id) }}
                    onClick={() => enter(ch.channel_id)}
                  >
                    {roomLabel(ch.channel_id)}
                  </button>
                  <div class="who">
                    {ch.participant_count
                      ? t("lobby.inside", { count: ch.participant_count })
                      : t("lobby.empty")}
                  </div>
                </div>
              )}
            </For>
            <Show when={dms().length}>
              <For each={dms()}>
                {(p) => (
                  <div class="person">
                    <button
                      class="name"
                      style={{ color: tint(p.name) }}
                      onClick={() => {
                        goto("room");
                        void openDm(p.pubkey_hex, p.name);
                      }}
                    >
                      @{p.name}
                    </button>
                    <div class="who">{t("lobby.just_you_two")}</div>
                  </div>
                )}
              </For>
            </Show>
          </div>
        </div>

        <Show when={!state.channelList.length && !dms().length}>
          <Note>{t("lobby.nothing")}</Note>
        </Show>

        <div class="lobby-foot">
          <Note>
            {t("lobby.hint")}{" "}
            <button class="word word-quiet" onClick={() => openPalette("/create ")}>
              {t("lobby.create")}
            </button>
          </Note>
        </div>
      </Show>
    </div>
  );
}
