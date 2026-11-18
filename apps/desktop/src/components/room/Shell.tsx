// One layout, four casts. Everything the app can show is a row of entities,
// their details underneath, and the line you type into.
//
// Keyboard first, and deliberately so: the input line does NOT hold focus by
// default, because space belongs to your voice. Typing any printable key
// hands focus to the line; Escape hands it back to the room.

import { Show, createEffect, onCleanup, onMount } from "solid-js";
import { state } from "../../lib/store";
import { toggleMute } from "../../lib/actions";
import { roomLabel } from "../../lib/rooms";
import { t } from "../../lib/i18n";
import Roster from "../session/Roster";
import Transcript from "../session/Transcript";
import { cmd } from "../../lib/tauri";
import { followRoom, goto, ground, toggle, view } from "../../lib/scene";
import { tweaks } from "../../lib/theme";
import DevicePrompt from "../DevicePrompt";
import UpdateNotification from "../UpdateNotification";
import Cast from "./Cast";
import Echo from "./Echo";
import Falling from "./Falling";
import HistoryScene from "./HistoryScene";
import Lobby from "./Lobby";
import Line from "./Line";
import Score from "./Score";
import SettingsScene from "./SettingsScene";
import TopLine from "./TopLine";

const isTyping = () => {
  const el = document.activeElement;
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA");
};

/// The session interface (`tweaks.layout`): same scenes, same line, a
/// different room and a frame around them.
const session = () => tweaks.layout === "session";

export default function Shell() {
  // A join or a leave moves the scene under the user's feet.
  createEffect(() => {
    void state.channel;
    void state.dm;
    followRoom();
  });

  onMount(() => {
    let talking = false;

    const down = async (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        return toggle("settings");
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "j") {
        e.preventDefault();
        return toggle("history");
      }
      if (e.key === "Escape") {
        if (isTyping()) return (document.activeElement as HTMLElement).blur();
        if (view() !== ground()) return goto(ground());
        return;
      }
      if (isTyping() || e.metaKey || e.ctrlKey || e.altKey) return;

      // Space is the voice, not a character — it only reaches the line once
      // the line has focus.
      if (e.code === "Space") {
        if (state.status?.voice_mode !== "ptt" || talking || e.repeat) return;
        e.preventDefault();
        talking = true;
        cmd.pttPress().catch(() => {});
        return;
      }
      if (e.key === "m") {
        e.preventDefault();
        return void toggleMute();
      }
      // In the session interface rooms are numbered, and the number goes there.
      // The room you are already in is skipped: clicking its own tab copies an
      // invitation, which a stray digit should not do.
      // Not in settings, where a digit is the start of a search.
      if (session() && view() !== "settings" && e.key >= "1" && e.key <= "9") {
        const tab = document.querySelectorAll<HTMLElement>(".top [data-tab]")[+e.key - 1];
        if (!tab) return;
        e.preventDefault();
        if (tab.getAttribute("aria-current") !== "true") tab.click();
        return;
      }
      // Any other printable key means the user wants to write.
      if (e.key.length === 1) {
        document.querySelector<HTMLInputElement>(".line input")?.focus();
      }
    };

    const up = (e: KeyboardEvent) => {
      if (e.code === "Space" && talking) {
        talking = false;
        cmd.pttRelease().catch(() => {});
      }
    };

    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    onCleanup(() => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      if (talking) cmd.pttRelease().catch(() => {});
    });
  });

  return (
    <div class="shell">
      <TopLine />
      <Show when={session() && state.conn === "reconnecting"}>
        <div class="ses-strip" role="status">
          ! {t("ses.lost", { server: state.serverAddr })}
          <Show when={state.channel}>
            {" "}
            {t("ses.lost_stay", { room: roomLabel(state.channel!) })}
          </Show>
        </div>
      </Show>
      {/* `display: contents` outside the session interface, so the stage and
          the line stay direct children of the shell's column as before. */}
      <div class="ses-main">
        <div class="stage">
          <Show when={view() === "room"}>
            <Show
              when={session()}
              fallback={
                <Show
                  when={tweaks.roomLook === "score"}
                  fallback={
                    <>
                      <Cast />
                      <Falling />
                    </>
                  }
                >
                  <Score />
                </Show>
              }
            >
              <Transcript />
            </Show>
          </Show>
          <Show when={view() === "lobby"}>
            <Lobby />
          </Show>
          <Show when={view() === "settings"}>
            <SettingsScene />
          </Show>
          <Show when={view() === "history"}>
            <HistoryScene />
          </Show>
        </div>
        {/* Things the app needs an answer to sit above the echo: both speak over
            the input line, but these wait for you. */}
        <div class="asks">
          <DevicePrompt />
          <UpdateNotification />
        </div>
        {/* In a session room the transcript already carries what the app says. */}
        <Show when={!(session() && view() === "room")}>
          <Echo />
        </Show>
        <Line />
      </div>
      <Show when={session() && view() === "room" && (state.channel || state.dm)}>
        <Roster />
      </Show>
    </div>
  );
}
