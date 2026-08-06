// Settings speak the same language as the room: a row of entities on top —
// sound, look, keys, window, key — and their details underneath, where the
// falling words would be.
//
// There is one control: a row of words, lit = chosen. Toggles are a pair of
// antonyms, device pickers are the same row, and the only continuous values
// (gain, volume, threshold) get a row of ticks. Nothing here is a checkbox,
// a select or a slider, because none of those exist in this interface.
//
// With 28 controls, searching beats remembering: typing in the input line
// filters every group at once.

import {
  For,
  Show,
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";
import { pushLog, state } from "../../lib/store";
import { refreshStatus } from "../../lib/actions";
import {
  cmd,
  on,
  type DeviceInfo,
  type OverlayPosition,
  type OverlayVisibility,
  type VoiceMode,
} from "../../lib/tauri";
import {
  setTweak,
  tweaks,
  type Accent,
  type AvatarStyle,
  type Crowd,
  type Theme,
  type Typeface,
} from "../../lib/theme";
import { formatAccel } from "../../lib/platform";
import { SUPPORTED_LANGS, t, type Lang } from "../../lib/i18n";
import { fill, query } from "../../lib/scene";
import { Face, Field, Meter, Note, Opts, Word } from "./words";

type GroupKey = "sound" | "look" | "keys" | "window" | "key";
const GROUPS: GroupKey[] = ["sound", "look", "keys", "window", "key"];

type Entry = {
  group: GroupKey;
  label?: string;
  /// Extra words the search should match beyond the label.
  keys?: string;
  hint?: string;
  when?: () => boolean;
  render: () => JSX.Element;
  bare?: boolean;
};

const ACCENT_HEX: Record<Accent, string> = {
  indigo: "#6272ff",
  forest: "#4fa877",
  rust: "#c2703b",
  slate: "#8b8e94",
};
const FONT_STACK: Record<Typeface, string> = {
  inter: "'Inter Tight', Inter, system-ui, sans-serif",
  plex: "'IBM Plex Sans', system-ui, sans-serif",
  mono: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
};

/// Long device names would blow the row apart; the tail is the noisy half.
const shortDevice = (name: string) =>
  name.length > 28 ? name.slice(0, 27) + "…" : name;

function DeviceRow(props: { kind: "input" | "output" }) {
  const [devices] = createResource<DeviceInfo[]>(() =>
    props.kind === "input" ? cmd.listInputDevices() : cmd.listOutputDevices(),
  );
  const current = () =>
    props.kind === "input"
      ? state.status?.input_device ?? null
      : state.status?.output_device ?? null;

  const apply = async (name: string | null) => {
    try {
      if (props.kind === "input") await cmd.setInputDevice(name);
      else await cmd.setOutputDevice(name);
      await refreshStatus();
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  return (
    <span class="opts">
      <Word tone="quiet" current={!current()} onClick={() => apply(null)}>
        {t("set.system_default")}
      </Word>
      <For each={devices() ?? []}>
        {(d) => (
          <Word
            tone="quiet"
            current={current() === d.name}
            title={d.name}
            onClick={() => apply(d.name)}
          >
            {shortDevice(d.name)}
          </Word>
        )}
      </For>
    </span>
  );
}

/// Five seconds of echo through the server. A word that changes in place —
/// no button, no status bar.
function SoundCheck() {
  type Phase = "idle" | "running" | "done";
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [secs, setSecs] = createSignal(5);
  const [verdict, setVerdict] = createSignal("");
  let timer: ReturnType<typeof setInterval> | undefined;

  const stop = () => {
    if (timer) clearInterval(timer);
    timer = undefined;
  };

  onMount(async () => {
    const offs = await Promise.all([
      on("echo_test_started", () => {
        setPhase("running");
        setSecs(5);
        stop();
        timer = setInterval(() => setSecs((s) => Math.max(0, s - 1)), 1000);
      }),
      on("echo_test_result", (r) => {
        stop();
        setPhase("done");
        setVerdict(
          r.roundtrip_ok
            ? t("settings.sound_check_ok")
            : !r.registered
              ? t("settings.sound_check_no_conn")
              : !r.mic_ok
                ? t("settings.sound_check_no_mic")
                : t("settings.sound_check_silent"),
        );
        setTimeout(() => setPhase("idle"), 6000);
      }),
    ]);
    onCleanup(() => {
      stop();
      offs.forEach((o) => o());
      if (phase() === "running") cmd.cancelEchoTest().catch(() => {});
    });
  });

  const start = async () => {
    if (phase() === "running") return;
    if (!state.status?.connected) {
      setPhase("done");
      setVerdict(t("settings.sound_check_disconnected"));
      setTimeout(() => setPhase("idle"), 4000);
      return;
    }
    if (state.status?.channel) {
      setPhase("done");
      setVerdict(t("settings.sound_check_in_channel"));
      setTimeout(() => setPhase("idle"), 4000);
      return;
    }
    try {
      await cmd.startEchoTest();
    } catch (e) {
      setPhase("done");
      setVerdict(String(e));
    }
  };

  return (
    <span class="opts">
      <Word tone={phase() === "idle" ? "quiet" : "loud"} onClick={start}>
        {phase() === "running"
          ? t("set.speak_now", { secs: secs() })
          : phase() === "done"
            ? verdict()
            : t("set.check_me")}
      </Word>
    </span>
  );
}

/// Hotkeys are caught, not typed: click the combo, press it, done.
function HotkeyRow(props: {
  action: string;
  bound: string | null;
  refresh: () => void;
}) {
  const [catching, setCatching] = createSignal(false);

  createEffect(() => {
    if (!catching()) return;
    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") return setCatching(false);
      if (e.key === "Backspace" || e.key === "Delete") {
        setCatching(false);
        try {
          await cmd.unsetHotkey(props.action);
          props.refresh();
        } catch {}
        return;
      }
      if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;
      const mods: string[] = [];
      if (e.ctrlKey) mods.push("Control");
      if (e.altKey) mods.push("Alt");
      if (e.shiftKey) mods.push("Shift");
      if (e.metaKey) mods.push("CommandOrControl");
      const key =
        e.code === "Space"
          ? "Space"
          : e.key.length === 1
            ? e.key.toUpperCase()
            : e.key;
      setCatching(false);
      try {
        await cmd.setHotkey(props.action, [...mods, key].join("+"));
        props.refresh();
      } catch (err) {
        pushLog(`${err}`, "error");
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  return (
    <span class="opts">
      <Word
        tone={catching() ? "loud" : props.bound ? "plain" : "quiet"}
        onClick={() => setCatching((v) => !v)}
      >
        {catching()
          ? t("set.press_now")
          : props.bound
            ? formatAccel(props.bound)
            : t("set.unbound")}
      </Word>
    </span>
  );
}

function CopyWord(props: { value: string | null | undefined }) {
  const [done, setDone] = createSignal(false);
  return (
    <Word
      tone="quiet"
      onClick={async () => {
        if (!props.value) return;
        try {
          await navigator.clipboard.writeText(props.value);
          setDone(true);
          setTimeout(() => setDone(false), 1600);
        } catch (e) {
          pushLog(`${e}`, "error");
        }
      }}
    >
      {done() ? t("common.copied") : t("common.copy")}
    </Word>
  );
}

export default function SettingsScene() {
  const [group, setGroup] = createSignal<GroupKey>("sound");
  const [hotkeys, setHotkeys] = createSignal<Record<string, string>>({});

  const refreshHotkeys = async () => {
    try {
      const list = await cmd.listHotkeys();
      const m: Record<string, string> = {};
      for (const [a, k] of list) m[a] = k;
      setHotkeys(m);
    } catch {}
  };

  onMount(() => {
    void refreshStatus();
    void refreshHotkeys();
  });

  const push = async (fn: () => Promise<void>) => {
    try {
      await fn();
      await refreshStatus();
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  const st = () => state.status;

  const entries = (): Entry[] => [
    // ── sound ────────────────────────────────────────────────
    {
      group: "sound",
      label: t("set.hears_me"),
      keys: "input microphone mic устройство микрофон",
      render: () => <DeviceRow kind="input" />,
    },
    {
      group: "sound",
      label: t("set.plays_in"),
      keys: "output speakers наушники динамики вывод",
      render: () => (
        <span class="opts-stack">
          <DeviceRow kind="output" />
          <Word tone="quiet" onClick={() => cmd.playTestSignal().catch(() => {})}>
            {t("common.test")}
          </Word>
        </span>
      ),
    },
    {
      group: "sound",
      label: t("set.my_voice"),
      keys: "gain усиление громкость микрофона",
      render: () => (
        <Meter
          label={t("set.my_voice")}
          value={st()?.input_gain_pct ?? 100}
          max={200}
          unit="%"
          set={(v) => push(() => cmd.setInputGain(v))}
        />
      ),
    },
    {
      group: "sound",
      label: t("set.their_voices"),
      keys: "volume громкость вывод",
      render: () => (
        <Meter
          label={t("set.their_voices")}
          value={st()?.output_vol_pct ?? 100}
          max={200}
          unit="%"
          set={(v) => push(() => cmd.setOutputVolume(v))}
        />
      ),
    },
    {
      group: "sound",
      label: t("set.transmit"),
      keys: "voice mode vad ptt режим передача",
      render: () => (
        <Opts<VoiceMode>
          opts={["open", "vad", "ptt"]}
          label={(m) => t(`set.mode_${m}`)}
          current={(m) => (st()?.voice_mode ?? "vad") === m}
          pick={(m) => push(() => cmd.setVoiceMode(m))}
        />
      ),
    },
    {
      group: "sound",
      label: t("set.threshold"),
      keys: "vad порог голоса",
      hint: t("set.threshold_hint"),
      when: () => (st()?.voice_mode ?? "vad") === "vad",
      render: () => (
        <Meter
          label={t("set.threshold")}
          value={st()?.vad_level_pct ?? 15}
          max={100}
          unit="%"
          set={(v) => push(() => cmd.setVadLevel(v))}
        />
      ),
    },
    {
      group: "sound",
      label: t("set.noise"),
      keys: "denoise rnnoise шум шумодав",
      hint: t("settings.denoise_hint"),
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.noise_clean") : t("set.noise_asis"))}
          current={(v) => (st()?.denoise ?? false) === v}
          pick={(v) => push(() => cmd.setDenoise(v))}
        />
      ),
    },
    {
      group: "sound",
      label: t("set.check"),
      keys: "echo test проверка звука эхо",
      hint: t("settings.sound_check_hint"),
      render: () => <SoundCheck />,
    },

    // ── look ─────────────────────────────────────────────────
    {
      group: "look",
      label: t("set.light"),
      keys: "theme dark light тема свет",
      render: () => (
        <Opts<Theme>
          opts={["dark", "light"]}
          label={(v) => t(`set.light_${v}`)}
          current={(v) => tweaks.theme === v}
          pick={(v) => setTweak("theme", v)}
        />
      ),
    },
    {
      group: "look",
      label: t("set.highlight"),
      keys: "accent colour цвет акцент подсветка",
      render: () => (
        <Opts<Accent>
          opts={["indigo", "forest", "rust", "slate"]}
          label={(v) => t(`set.accent_${v}`)}
          current={(v) => tweaks.accent === v}
          pick={(v) => setTweak("accent", v)}
          paint={(v, on) => ({ color: on ? ACCENT_HEX[v] : undefined })}
        />
      ),
    },
    {
      group: "look",
      label: t("set.letters"),
      keys: "font typeface шрифт буквы гарнитура",
      render: () => (
        <Opts<Typeface>
          opts={["mono", "inter", "plex"]}
          label={(v) => t(`set.face_${v}`)}
          current={(v) => tweaks.typeface === v}
          pick={(v) => setTweak("typeface", v)}
          paint={(v) => ({ "font-family": FONT_STACK[v] })}
        />
      ),
    },
    {
      group: "look",
      label: t("set.crowd"),
      keys: "crowd many люди толпа молчащие",
      hint: t("set.crowd_hint"),
      render: () => (
        <Opts<Crowd>
          opts={["recent", "all"]}
          label={(v) => t(`set.crowd_${v}`)}
          current={(v) => tweaks.crowd === v}
          pick={(v) => setTweak("crowd", v)}
        />
      ),
    },
    {
      group: "look",
      label: t("set.hints"),
      keys: "hints подсказки клавиши",
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.show") : t("set.hide"))}
          current={(v) => tweaks.composerHints === v}
          pick={(v) => setTweak("composerHints", v)}
        />
      ),
    },
    {
      group: "look",
      label: t("set.faces"),
      keys: "avatar identicon аватар лица оверлей",
      hint: t("set.faces_hint"),
      render: () => (
        <Opts<AvatarStyle>
          opts={["weave", "pixel", "faces"]}
          label={(v) => t(`avatar.${v}`)}
          current={(v) => tweaks.avatarStyle === v}
          pick={(v) => setTweak("avatarStyle", v)}
        />
      ),
    },
    {
      group: "look",
      label: t("settings.language"),
      keys: "language язык locale",
      render: () => (
        <Opts<Lang>
          opts={SUPPORTED_LANGS.map((l) => l.code)}
          label={(c) => SUPPORTED_LANGS.find((l) => l.code === c)?.label ?? c}
          current={(c) => (st()?.language ?? "en") === c}
          pick={(c) => push(() => cmd.setLanguage(c))}
        />
      ),
    },

    // ── keys ─────────────────────────────────────────────────
    {
      group: "keys",
      label: t("set.key_ptt"),
      keys: "hotkey ptt talk говорить",
      render: () => (
        <HotkeyRow action="ptt" bound={hotkeys()["ptt"] ?? null} refresh={refreshHotkeys} />
      ),
    },
    {
      group: "keys",
      label: t("set.key_mute"),
      keys: "hotkey mute микрофон",
      render: () => (
        <HotkeyRow action="mute" bound={hotkeys()["mute"] ?? null} refresh={refreshHotkeys} />
      ),
    },
    {
      group: "keys",
      label: t("set.key_join"),
      keys: "hotkey quick join быстрый вход",
      render: () => (
        <HotkeyRow
          action="quick_join"
          bound={hotkeys()["quick_join"] ?? null}
          refresh={refreshHotkeys}
        />
      ),
    },
    {
      group: "keys",
      bare: true,
      keys: "hotkey global",
      render: () => <Note>{t("set.keys_note")}</Note>,
    },
    {
      group: "keys",
      label: t("set.in_window"),
      keys: "shortcuts local",
      render: () => <span class="fixed">{t("set.in_window_list")}</span>,
    },

    // ── window ───────────────────────────────────────────────
    {
      group: "window",
      label: t("set.over_games"),
      keys: "overlay hud игра оверлей",
      hint: t("settings.overlay_hint"),
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.show") : t("set.no_need"))}
          current={(v) => (st()?.overlay_enabled ?? false) === v}
          pick={(v) => push(() => cmd.setOverlayEnabled(v))}
        />
      ),
    },
    {
      group: "window",
      label: t("set.in_corner"),
      keys: "overlay position позиция угол",
      when: () => !!st()?.overlay_enabled,
      render: () => (
        <Opts<OverlayPosition>
          opts={["tl", "tr", "lc", "rc", "bl", "br"]}
          label={(p) => ({ tl: "↖", tr: "↗", lc: "←", rc: "→", bl: "↙", br: "↘" })[p]}
          current={(p) => (st()?.overlay_position ?? "tr") === p}
          pick={(p) => push(() => cmd.setOverlayPosition(p))}
          glyphs
        />
      ),
    },
    {
      group: "window",
      label: t("settings.overlay_visibility"),
      keys: "overlay visibility показывать",
      when: () => !!st()?.overlay_enabled,
      render: () => (
        <Opts<OverlayVisibility>
          opts={["in_call", "always"]}
          label={(v) =>
            v === "in_call"
              ? t("settings.overlay_vis_in_call")
              : t("settings.overlay_vis_always")
          }
          current={(v) => (st()?.overlay_visibility ?? "in_call") === v}
          pick={(v) => push(() => cmd.setOverlayVisibility(v))}
        />
      ),
    },
    {
      group: "window",
      label: t("settings.notifications"),
      keys: "notifications уведомления",
      hint: t("settings.notifications_desc"),
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.notify_send") : t("set.notify_quiet"))}
          current={(v) => (st()?.notifications ?? true) === v}
          pick={(v) => push(() => cmd.setNotifications(v))}
        />
      ),
    },
    {
      group: "window",
      label: t("set.close_button"),
      keys: "tray close трей крестик",
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.tray_hides") : t("set.tray_quits"))}
          current={(v) => (st()?.close_to_tray ?? false) === v}
          pick={(v) => push(() => cmd.setCloseToTray(v))}
        />
      ),
    },
    {
      group: "window",
      label: t("set.on_login"),
      keys: "autostart запуск вход",
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.launch") : t("set.no_need"))}
          current={(v) => (st()?.autostart ?? false) === v}
          pick={(v) => push(() => cmd.setAutostart(v))}
        />
      ),
    },

    // ── key ──────────────────────────────────────────────────
    {
      group: "key",
      bare: true,
      keys: "fingerprint отпечаток",
      render: () => <Face>{st()?.fingerprint ?? t("common.none")}</Face>,
    },
    {
      group: "key",
      bare: true,
      keys: "fingerprint отпечаток copy скопировать",
      render: () => (
        <Note>
          {t("set.fingerprint_note")}{" "}
          {/* Copying belongs next to the thing you'd send someone, not two
              rows down under "server". */}
          <CopyWord value={st()?.fingerprint} />
        </Note>
      ),
    },
    {
      group: "key",
      label: t("set.called"),
      keys: "name nickname имя ник",
      render: () => (
        <span class="opts">
          <Word onClick={() => fill(`/name ${st()?.name ?? ""}`)}>
            {st()?.name ?? t("common.none")}
          </Word>
        </span>
      ),
    },
    {
      group: "key",
      label: t("set.whole_key"),
      keys: "pubkey ключ ed25519",
      render: () => (
        <span class="opts">
          <span class="fixed mono-break">
            {st()?.pubkey ? st()!.pubkey!.slice(0, 32) + "…" : t("common.none")}
          </span>
          <CopyWord value={st()?.pubkey} />
        </span>
      ),
    },
    {
      group: "key",
      label: t("set.server"),
      keys: "server tofu сервер отпечаток",
      render: () => (
        <span class="opts">
          <span class="fixed">{state.serverAddr}</span>
          {/* The server's own TOFU fingerprint isn't in app_status, so there is
              nothing of the server's to copy here — only its address. */}
          <CopyWord value={state.serverAddr} />
          <Word tone="quiet" onClick={() => fill(`/forget ${state.serverAddr}`)}>
            {t("set.forget")}
          </Word>
        </span>
      ),
    },
    {
      group: "key",
      label: t("settings.paranoid"),
      keys: "paranoid паранойя",
      hint: t("settings.paranoid_hint"),
      render: () => (
        <Opts<boolean>
          opts={[true, false]}
          label={(v) => (v ? t("set.strict") : t("set.usual"))}
          current={(v) => (st()?.paranoid ?? false) === v}
          pick={(v) => push(() => cmd.setParanoid(v))}
        />
      ),
    },
    {
      group: "key",
      label: t("set.backup"),
      keys: "backup restore резервная копия",
      hint: t("set.backup_hint"),
      render: () => (
        <span class="opts">
          <Word tone="quiet" onClick={() => fill("/backup")}>
            {t("set.backup_save")}
          </Word>
          <Word tone="quiet" onClick={() => fill("/restore")}>
            {t("set.backup_load")}
          </Word>
        </span>
      ),
    },
  ];

  const shown = () => {
    const q = query().trim().toLowerCase();
    const all = entries();
    if (!q)
      return all
        .filter((e) => e.group === group())
        .filter((e) => !e.when || e.when());
    return all.filter(
      (e) =>
        (e.label ?? "").toLowerCase().includes(q) ||
        (e.keys ?? "").toLowerCase().includes(q) ||
        (e.hint ?? "").toLowerCase().includes(q),
    );
  };

  return (
    <div class="cast" style={{ "--nameSize": "26px" }}>
      <div class="ranks">
        <div class="rank">
          <For each={GROUPS}>
            {(g) => (
              <div class="person">
                <button
                  class="name"
                  classList={{ dim: !!query() || group() !== g }}
                  onClick={() => setGroup(g)}
                >
                  {t(`set.group_${g}`)}
                </button>
              </div>
            )}
          </For>
        </div>
      </div>

      <div class="detail">
        <Show when={shown().length} fallback={<Note>{t("set.nothing_like")}</Note>}>
          <For each={shown()}>
            {(e) => (
              <Show when={!e.bare} fallback={e.render()}>
                <Field
                  label={e.label}
                  from={query() ? t(`set.group_${e.group}`) : undefined}
                  hint={e.hint}
                >
                  {e.render()}
                </Field>
              </Show>
            )}
          </For>
        </Show>
      </div>

      {/* Which build this is belongs to the app, not to one of the groups —
          it stays put whichever section you're reading. */}
      <div class="settings-foot">{t("set.version", { v: st()?.version ?? "—" })}</div>
    </div>
  );
}
