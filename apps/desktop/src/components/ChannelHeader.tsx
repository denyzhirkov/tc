// Header strip above the message area. Shows the active channel (or DM target)
// with a quiet subtitle (`voice + text · N in call`), and an inline leave-call
// action on the right.

import { Show } from "solid-js";
import { state } from "../lib/store";
import { leaveChannel } from "../lib/actions";
import { closeDm } from "../lib/dm";
import { Hash, PhoneOff } from "./Icons";

export default function ChannelHeader() {
  return (
    <div class="h-14 px-4 flex items-center gap-3 border-b border-line bg-bg shrink-0">
      <Show
        when={state.dm}
        fallback={
          <Show
            when={state.channel}
            fallback={
              <span class="text-muted text-sm">no channel selected</span>
            }
          >
            <span class="text-muted">
              <Hash size={18} />
            </span>
            <div class="flex flex-col leading-tight">
              <span class="font-medium text-text">{state.channel}</span>
              <span class="text-[11px] text-muted">
                voice + text
                <Show when={state.participants.length > 0}>
                  {" "}· {state.participants.length} in call
                </Show>
              </span>
            </div>
            <span class="ml-auto">
              <button
                class="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-danger hover:bg-hover"
                onClick={() => leaveChannel()}
                title="leave call"
              >
                <PhoneOff size={14} />
                <span>leave call</span>
              </button>
            </span>
          </Show>
        }
      >
        <span class="text-warn text-[11px] uppercase tracking-wider">DM</span>
        <div class="flex flex-col leading-tight">
          <span class="font-medium text-text">{state.dm?.name}</span>
          <span class="text-[11px] text-muted font-mono">
            {state.dm?.pubkey_hex.slice(0, 16)}…
          </span>
        </div>
        <span class="ml-auto">
          <button
            class="px-2.5 py-1 rounded-md text-sm text-muted hover:text-text hover:bg-hover"
            onClick={() => closeDm()}
          >
            ← back
          </button>
        </span>
      </Show>
    </div>
  );
}
