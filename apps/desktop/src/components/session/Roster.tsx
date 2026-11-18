// Who is on the line, beside the transcript, and what the line itself is doing.
//
// The two blocks under the roster read like a tool's output on purpose: they
// are the facts this app is for — the voice is sealed end to end, the relay
// sees a room id and nothing else — stated where a call is actually happening
// rather than in a settings page.
//
// Only what the core reports is printed. There is no round-trip time and no
// jitter-buffer depth in `voice_level`, and a peer's mute never crosses the
// wire, so none of those appear here.

import { For, Show, createResource } from "solid-js";
import { state } from "../../lib/store";
import { levelNow } from "../../lib/scoreFeed";
import { SCORE_AUDIBLE } from "../../lib/score";
import { openDm } from "../../lib/dm";
import { cmd } from "../../lib/tauri";
import { tint } from "../../lib/tint";
import { t } from "../../lib/i18n";
import Identicon from "../Identicon";

const TICKS = 12;

function Person(props: { name: string }) {
  const isMe = () => props.name === state.status?.name;
  // The roster carries names; the picture is drawn from the key behind one.
  const [key] = createResource(
    () => (isMe() ? null : props.name),
    (name) => cmd.resolvePeer(name).catch(() => null),
  );
  const pubkey = () => (isMe() ? state.status?.pubkey : key());
  const muted = () => isMe() && !!state.status?.muted;
  const level = () => (muted() ? 0 : levelNow(props.name));
  const speaking = () => level() >= SCORE_AUDIBLE;

  const word = () => {
    if (muted()) return t("ses.muted");
    return speaking() ? t("ses.speaking") : t("ses.quiet");
  };

  const talkTo = () => {
    const pk = key();
    if (pk) void openDm(pk, props.name);
  };

  return (
    <li class="ses-person" classList={{ "ses-now": speaking() }}>
      <Identicon pubkeyHex={pubkey()} size={20} />
      <Show
        when={!isMe() && !state.dm}
        fallback={
          <span class="ses-name" style={{ color: tint(props.name) }}>
            {props.name}
            <Show when={isMe()}>
              <span class="ses-you">{t("room.you_tail")}</span>
            </Show>
          </span>
        }
      >
        <button
          type="button"
          class="ses-name"
          style={{ color: tint(props.name) }}
          title={t("ses.write_to", { name: props.name })}
          onClick={talkTo}
        >
          {props.name}
        </button>
      </Show>
      <span class="ses-state">{word()}</span>
      <span class="ses-meter" aria-hidden="true">
        <For each={Array.from({ length: TICKS }, (_, i) => i)}>
          {(i) => <i classList={{ on: level() * TICKS > i + 0.5 }} />}
        </For>
      </span>
    </li>
  );
}

export default function Roster() {
  const people = () => {
    if (state.dm) {
      const me = state.status?.name;
      return me ? [state.dm.name, me] : [state.dm.name];
    }
    return state.participants;
  };

  const mic = () => {
    const s = state.status;
    if (!s) return "";
    if (s.muted) return t("ses.muted");
    const mode = t(`ses.mode_${s.voice_mode}`);
    return s.voice_mode === "vad" ? `${mode}, ${s.vad_level_pct}%` : mode;
  };

  return (
    <aside class="ses-roster" aria-label={t("ses.on_the_line")}>
      <div class="ses-roster-head">
        <h2>{t("ses.on_the_line")}</h2>
        <span>{people().length}</span>
      </div>
      <ul class="ses-people" classList={{ "ses-away": state.conn !== "connected" }}>
        <For each={people()}>{(name) => <Person name={name} />}</For>
      </ul>

      <div class="ses-out">
        <div class="ses-out-head"># {t("ses.line")}</div>
        <dl>
          <dt>{t("ses.voice")}</dt>
          <dd>{t("ses.end_to_end")}</dd>
          <dt>{t("ses.cipher")}</dt>
          <dd>xchacha20-poly1305</dd>
          <dt>{t("ses.relay")}</dt>
          <dd>{t("ses.relay_sees")}</dd>
          <Show when={!state.dm && state.voice}>
            {(v) => (
              <>
                <dt>{t("ses.udp")}</dt>
                <dd>{v().registered ? t("ses.udp_ok") : t("ses.udp_wait")}</dd>
                <dt>{t("ses.loss")}</dt>
                <dd>{v().loss_percent.toFixed(1)} %</dd>
                <dt>{t("ses.codec")}</dt>
                <dd>{v().tier}</dd>
                <dt>{t("ses.rate")}</dt>
                <dd>
                  ↑ {v().tx_kbps.toFixed(0)} ↓ {v().rx_kbps.toFixed(0)} kbps
                </dd>
              </>
            )}
          </Show>
        </dl>
      </div>

      <div class="ses-out">
        <div class="ses-out-head"># {t("room.you_tail")}</div>
        <dl>
          <dt>{t("ses.name")}</dt>
          <dd>{state.status?.name ?? ""}</dd>
          <dt>{t("ses.key")}</dt>
          <dd>ed25519 {state.status?.fingerprint ?? ""}</dd>
          <dt>{t("ses.mic")}</dt>
          <dd>{mic()}</dd>
        </dl>
      </div>
    </aside>
  );
}
