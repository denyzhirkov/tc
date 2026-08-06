// One layout, four casts. Everything the app can show is a row of entities,
// their details underneath, and the line you type into.
//
// Keyboard first, and deliberately so: the input line does NOT hold focus by
// default, because space belongs to your voice. Typing any printable key
// hands focus to the line; Escape hands it back to the room.

import { Show, createEffect, onCleanup, onMount } from "solid-js";
import { state } from "../../lib/store";
import { toggleMute } from "../../lib/actions";
import { cmd } from "../../lib/tauri";
import { followRoom, goto, ground, toggle, view } from "../../lib/scene";
import DevicePrompt from "../DevicePrompt";
import UpdateNotification from "../UpdateNotification";
import Cast from "./Cast";
import Echo from "./Echo";
import Falling from "./Falling";
import HistoryScene from "./HistoryScene";
import Lobby from "./Lobby";
import Line from "./Line";
import SettingsScene from "./SettingsScene";
import TopLine from "./TopLine";

const isTyping = () => {
  const el = document.activeElement;
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA");
};

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
      <div class="stage">
        <Show when={view() === "room"}>
          <Cast />
          <Falling />
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
      <Echo />
      <Line />
    </div>
  );
}
