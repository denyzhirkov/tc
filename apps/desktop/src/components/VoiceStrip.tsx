// "Connected" block at the bottom of the sidebar — title, current channel,
// and a row of three primary controls (mute, deafen/mode, leave).

import { Show } from "solid-js";
import { state } from "../lib/store";
import { leaveChannel, toggleMute, toggleVadPtt } from "../lib/actions";
import { Mic, MicOff, PhoneOff, Hash, SpeakerOff } from "./Icons";

function CtrlBtn(props: {
  onClick: () => void;
  title?: string;
  active?: boolean;
  danger?: boolean;
  children: any;
}) {
  const cls = () => {
    if (props.danger) return "bg-danger text-white hover:opacity-90";
    if (props.active)
      return "bg-accent-bg text-accent border border-accent/30";
    return "bg-surface text-text2 hover:text-text border border-line";
  };
  return (
    <button
      class={`flex-1 h-9 rounded-md flex items-center justify-center ${cls()}`}
      title={props.title}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

export default function VoiceStrip() {
  const muted = () => !!state.status?.muted;
  const mode = () => state.status?.voice_mode ?? "vad";
  return (
    <Show when={state.channel}>
      <div class="border-t border-line bg-surface2 px-3 py-3 flex flex-col gap-2.5">
        <div class="flex items-center gap-2">
          <span class="inline-block w-1.5 h-1.5 rounded-full bg-ok shrink-0" />
          <span class="text-[10px] uppercase tracking-wider text-muted">
            connected
          </span>
        </div>
        <div class="flex items-center gap-1.5 text-text">
          <span class="text-faint">
            <Hash size={14} />
          </span>
          <span class="text-sm font-medium truncate flex-1">
            {state.channel}
          </span>
          <Show when={state.voice}>
            {(v) => (
              <span class="text-[10px] text-muted tabular-nums">
                {v().tier} · {v().loss_percent}%
              </span>
            )}
          </Show>
        </div>
        <div class="flex items-center gap-1.5">
          <CtrlBtn
            onClick={() => toggleMute()}
            active={muted()}
            title={muted() ? "unmute" : "mute"}
          >
            {muted() ? <MicOff size={16} /> : <Mic size={16} />}
          </CtrlBtn>
          <CtrlBtn
            onClick={() => toggleVadPtt()}
            title={`mode: ${mode()} (click vad ↔ ptt)`}
          >
            <span class="flex items-center gap-1 text-[10px] uppercase tracking-wider">
              <SpeakerOff size={14} />
              {mode()}
            </span>
          </CtrlBtn>
          <CtrlBtn onClick={() => leaveChannel()} danger title="leave call">
            <PhoneOff size={16} />
          </CtrlBtn>
        </div>
      </div>
    </Show>
  );
}
