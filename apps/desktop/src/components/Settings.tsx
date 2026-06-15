// Settings overlay. Rewritten in the quiet desktop style — sectioned cards,
// soft borders, no ASCII frames. Adds an "appearance" section with the
// design's tweaks (theme / accent / typeface).

import {
  For,
  Show,
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { closeSettings, refreshStatus } from "../lib/actions";
import { pushLog, state } from "../lib/store";
import { cmd, on, type DeviceInfo, type VoiceMode } from "../lib/tauri";
import {
  setTweak,
  tweaks,
  type Accent,
  type Theme,
  type Typeface,
} from "../lib/theme";
import { isMac } from "../lib/platform";
import { SUPPORTED_LANGS, t, type Lang } from "../lib/i18n";
import Identicon from "./Identicon";

function Section(props: { title: string; children: any }) {
  return (
    <section class="mb-6">
      <h3 class="text-[11px] uppercase tracking-wider text-muted mb-2 select-none">
        {props.title}
      </h3>
      <div class="bg-surface border border-line rounded-xl p-4 flex flex-col gap-3">
        {props.children}
      </div>
    </section>
  );
}

function Row(props: { label: string; children: any }) {
  return (
    <div class="flex items-center gap-4">
      <span class="w-32 text-sm text-muted shrink-0">{props.label}</span>
      <span class="flex-1 flex items-center gap-2 flex-wrap">
        {props.children}
      </span>
    </div>
  );
}

function inputCls() {
  return "bg-bg px-2.5 py-1 rounded-md border border-line hover:border-faint focus:border-accent text-sm";
}
function chipCls(active: boolean) {
  return `px-2.5 py-1 rounded-md text-sm border cursor-pointer select-none transition-colors ${
    active
      ? "border-accent text-accent bg-accent-bg"
      : "border-line text-text2 hover:border-faint hover:text-text"
  }`;
}
function btnCls(props?: { danger?: boolean }) {
  return `px-2.5 py-1 rounded-md text-sm border border-line cursor-pointer select-none ${
    props?.danger
      ? "text-muted hover:text-danger hover:border-danger"
      : "text-text2 hover:text-text hover:border-faint"
  }`;
}

// Sound check: a 5s live echo through the server. Backend owns the timing and
// emits `echo_test_started` → countdown, `echo_test_result` → verdict.
function SoundCheck() {
  const ECHO_SECS = 5;
  type Phase = "idle" | "starting" | "running" | "done";
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [secs, setSecs] = createSignal(ECHO_SECS);
  const [msg, setMsg] = createSignal<{ text: string; ok: boolean } | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;
  const unlisteners: Array<() => void> = [];

  const stopTimer = () => {
    if (timer) clearInterval(timer);
    timer = undefined;
  };

  onMount(async () => {
    unlisteners.push(
      await on("echo_test_started", () => {
        setMsg(null);
        setPhase("running");
        setSecs(ECHO_SECS);
        stopTimer();
        timer = setInterval(() => {
          setSecs((s) => Math.max(0, s - 1));
        }, 1000);
      }),
    );
    unlisteners.push(
      await on("echo_test_result", (r) => {
        stopTimer();
        setPhase("done");
        if (r.roundtrip_ok) setMsg({ text: t("settings.sound_check_ok"), ok: true });
        else if (!r.registered)
          setMsg({ text: t("settings.sound_check_no_conn"), ok: false });
        else if (!r.mic_ok)
          setMsg({ text: t("settings.sound_check_no_mic"), ok: false });
        else setMsg({ text: t("settings.sound_check_silent"), ok: false });
      }),
    );
  });

  onCleanup(() => {
    stopTimer();
    for (const u of unlisteners) u();
    // Abort a still-running check so the server channel + handle tear down.
    if (phase() === "running" || phase() === "starting") {
      cmd.cancelEchoTest().catch(() => {});
    }
  });

  const busy = () => phase() === "starting" || phase() === "running";

  const start = async () => {
    if (busy()) return;
    if (!state.status?.connected) {
      setMsg({ text: t("settings.sound_check_disconnected"), ok: false });
      return;
    }
    if (state.status?.channel) {
      setMsg({ text: t("settings.sound_check_in_channel"), ok: false });
      return;
    }
    setMsg(null);
    setPhase("starting");
    try {
      await cmd.startEchoTest();
    } catch (e) {
      setPhase("idle");
      setMsg({ text: String(e), ok: false });
    }
  };

  return (
    <div class="flex flex-col gap-1.5">
      <div class="flex items-center gap-2 flex-wrap">
        <span
          class={btnCls()}
          classList={{ "opacity-50 pointer-events-none": busy() }}
          onClick={start}
        >
          {busy()
            ? t("settings.sound_check_recording", { secs: String(secs()) })
            : t("settings.sound_check_start")}
        </span>
        <Show when={!busy() && msg()}>
          <span
            class="text-sm"
            classList={{ "text-accent": msg()!.ok, "text-danger": !msg()!.ok }}
          >
            {msg()!.text}
          </span>
        </Show>
      </div>
      <span class="text-xs text-muted">{t("settings.sound_check_hint")}</span>
    </div>
  );
}

function NameInput() {
  const [val, setVal] = createSignal(state.status?.name ?? "");
  createEffect(() => setVal(state.status?.name ?? ""));
  const submit = async () => {
    const v = val().trim();
    if (!v) return;
    try {
      await cmd.setName(v);
      await refreshStatus();
    } catch {}
  };
  return (
    <input
      class={`${inputCls()} w-56`}
      value={val()}
      onInput={(e) => setVal(e.currentTarget.value)}
      onChange={submit}
      onKeyDown={(e) => e.key === "Enter" && submit()}
    />
  );
}

function DeviceSelect(props: {
  kind: "input" | "output";
  current: string | null;
}) {
  const [devices] = createResource<DeviceInfo[]>(() =>
    props.kind === "input" ? cmd.listInputDevices() : cmd.listOutputDevices(),
  );
  const apply = async (name: string | null) => {
    try {
      if (props.kind === "input") await cmd.setInputDevice(name);
      else await cmd.setOutputDevice(name);
      await refreshStatus();
    } catch {}
  };
  return (
    <select
      class={`${inputCls()} w-72`}
      onChange={(e) => apply(e.currentTarget.value || null)}
    >
      <option value="" selected={!props.current}>
        (system default)
      </option>
      <For each={devices() ?? []}>
        {(d) => (
          <option value={d.name} selected={props.current === d.name}>
            {d.name}
          </option>
        )}
      </For>
    </select>
  );
}

function Slider(props: {
  value: number;
  min: number;
  max: number;
  apply: (v: number) => Promise<void>;
  unit?: string;
}) {
  const [val, setVal] = createSignal(props.value);
  createEffect(() => setVal(props.value));
  const onChange = async (e: any) => {
    const v = parseInt(e.currentTarget.value, 10);
    setVal(v);
    try {
      await props.apply(v);
      await refreshStatus();
    } catch {}
  };
  return (
    <>
      <input
        type="range"
        min={props.min}
        max={props.max}
        value={val()}
        onInput={onChange}
        class="flex-1 accent-accent"
      />
      <span class="w-12 text-right text-muted text-sm tabular-nums">
        {val()}
        {props.unit ?? ""}
      </span>
    </>
  );
}

function ModePicker() {
  const cur = (): VoiceMode => (state.status?.voice_mode ?? "vad") as VoiceMode;
  const set = async (m: VoiceMode) => {
    try {
      await cmd.setVoiceMode(m);
      await refreshStatus();
    } catch {}
  };
  const item = (m: VoiceMode, label: string) => (
    <span class={chipCls(cur() === m)} onClick={() => set(m)}>
      {label}
    </span>
  );
  return (
    <span class="flex items-center gap-2">
      {item("open", "always on")}
      {item("vad", "vad")}
      {item("ptt", "push to talk")}
    </span>
  );
}

function HotkeyRow(props: {
  action: string;
  current: string | null;
  refresh: () => void;
}) {
  const [val, setVal] = createSignal(props.current ?? "");
  createEffect(() => setVal(props.current ?? ""));

  const apply = async () => {
    const v = val().trim();
    if (!v) return;
    try {
      await cmd.setHotkey(props.action, v);
      props.refresh();
    } catch (e) {
      alert(`hotkey: ${e}`);
    }
  };
  const clear = async () => {
    try {
      await cmd.unsetHotkey(props.action);
      setVal("");
      props.refresh();
    } catch {}
  };

  return (
    <Row label={props.action}>
      <input
        class={`${inputCls()} w-56`}
        value={val()}
        placeholder={isMac ? "e.g. Alt+M" : "e.g. Ctrl+Alt+M"}
        onInput={(e) => setVal(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && apply()}
      />
      <span class={btnCls()} onClick={apply}>
        save
      </span>
      <span class={btnCls({ danger: true })} onClick={clear}>
        clear
      </span>
    </Row>
  );
}

function CopyBtn(props: { value: string; children: any }) {
  const copy = async () => {
    if (!props.value) return;
    try {
      await navigator.clipboard.writeText(props.value);
      pushLog(`copied: ${props.value.slice(0, 16)}…`, "system");
    } catch (e) {
      pushLog(`copy failed: ${e}`, "error");
    }
  };
  return (
    <span class={btnCls()} onClick={copy}>
      {props.children}
    </span>
  );
}

function Toggle(props: {
  value: boolean;
  onChange: (v: boolean) => void;
  label: string;
}) {
  return (
    <label class="flex items-center gap-2.5 cursor-pointer select-none text-sm">
      <span
        class={`relative inline-block w-8 h-[18px] rounded-full transition-colors ${
          props.value ? "bg-accent" : "bg-surface2 border border-line"
        }`}
        onClick={() => props.onChange(!props.value)}
      >
        <span
          class={`absolute top-[2px] w-[14px] h-[14px] rounded-full bg-white shadow transition-all ${
            props.value ? "left-[16px]" : "left-[2px]"
          }`}
        />
      </span>
      <span class="text-text2">{props.label}</span>
    </label>
  );
}

function Appearance() {
  const themes: { value: Theme; label: string }[] = [
    { value: "dark", label: "dark" },
    { value: "light", label: "light" },
  ];
  const accents: { value: Accent; label: string; color: string }[] = [
    { value: "indigo", label: "indigo", color: "#5b6cff" },
    { value: "forest", label: "forest", color: "#3a8a5e" },
    { value: "rust", label: "rust", color: "#c2562b" },
    { value: "slate", label: "slate", color: "#7a7d83" },
  ];
  const faces: { value: Typeface; label: string }[] = [
    { value: "inter", label: "Inter" },
    { value: "plex", label: "IBM Plex" },
    { value: "mono", label: "JetBrains Mono" },
  ];
  return (
    <>
      <Row label="theme">
        <For each={themes}>
          {(t) => (
            <span
              class={chipCls(tweaks.theme === t.value)}
              onClick={() => setTweak("theme", t.value)}
            >
              {t.label}
            </span>
          )}
        </For>
      </Row>
      <Row label="accent">
        <For each={accents}>
          {(a) => (
            <span
              class={`flex items-center gap-1.5 ${chipCls(
                tweaks.accent === a.value,
              )}`}
              onClick={() => setTweak("accent", a.value)}
            >
              <span
                class="inline-block w-2.5 h-2.5 rounded-full"
                style={{ background: a.color }}
              />
              {a.label}
            </span>
          )}
        </For>
      </Row>
      <Row label="typeface">
        <For each={faces}>
          {(f) => (
            <span
              class={chipCls(tweaks.typeface === f.value)}
              onClick={() => setTweak("typeface", f.value)}
            >
              {f.label}
            </span>
          )}
        </For>
      </Row>
    </>
  );
}

export default function Settings() {
  const [hotkeys, setHotkeys] = createSignal<Record<string, string>>({});
  const refreshHotkeys = async () => {
    try {
      const list = await cmd.listHotkeys();
      const m: Record<string, string> = {};
      for (const [a, k] of list) m[a] = k;
      setHotkeys(m);
    } catch {}
  };

  const [notify, setNotify] = createSignal(state.status?.notifications ?? true);
  const [tray, setTray] = createSignal(state.status?.close_to_tray ?? false);
  const [auto, setAuto] = createSignal(state.status?.autostart ?? false);
  const [para, setPara] = createSignal(state.status?.paranoid ?? false);
  const [denoise, setDenoise] = createSignal(state.status?.denoise ?? false);

  onMount(async () => {
    await refreshStatus();
    setNotify(state.status?.notifications ?? true);
    setTray(state.status?.close_to_tray ?? false);
    setAuto(state.status?.autostart ?? false);
    setPara(state.status?.paranoid ?? false);
    setDenoise(state.status?.denoise ?? false);
    await refreshHotkeys();
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeSettings();
    };
    window.addEventListener("keydown", onEsc);
    onCleanup(() => window.removeEventListener("keydown", onEsc));
  });

  const fp = () => state.status?.fingerprint ?? "(none)";

  return (
    <div class="fixed inset-0 z-40 bg-black/40 flex items-start justify-center pt-12 pb-12 overflow-y-auto">
      <div class="w-[720px] max-w-[92vw] bg-bg border border-line rounded-2xl shadow-2xl p-6">
        <div class="flex items-center mb-4">
          <h2 class="text-lg font-semibold">{t("settings.title")}</h2>
          <button
            class={`ml-auto ${btnCls()}`}
            onClick={() => closeSettings()}
          >
            {t("settings.esc_close")}
          </button>
        </div>

        <Section title={t("common.appearance")}>
          <Appearance />
        </Section>

        <Section title={t("settings.identity")}>
          <Row label={t("common.nickname")}>
            <NameInput />
          </Row>
          <Row label={t("common.identicon")}>
            <Identicon pubkeyHex={state.status?.pubkey} />
          </Row>
          <Row label={t("settings.fingerprint")}>
            <span class="font-mono text-muted text-sm">{fp()}</span>
            <CopyBtn value={state.status?.fingerprint ?? ""}>{t("common.copy")}</CopyBtn>
          </Row>
          <Row label={t("settings.pubkey")}>
            <span class="font-mono text-muted text-xs break-all">
              {state.status?.pubkey ?? t("common.none")}
            </span>
            <CopyBtn value={state.status?.pubkey ?? ""}>{t("common.copy")}</CopyBtn>
          </Row>
        </Section>

        <Section title={t("settings.audio")}>
          <Row label={t("settings.input")}>
            <DeviceSelect
              kind="input"
              current={state.status?.input_device ?? null}
            />
          </Row>
          <Row label={t("settings.output")}>
            <DeviceSelect
              kind="output"
              current={state.status?.output_device ?? null}
            />
            <span class={btnCls()} onClick={() => cmd.playTestSignal()}>
              {t("common.test")}
            </span>
          </Row>
          <Row label={t("settings.input_gain")}>
            <Slider
              value={state.status?.input_gain_pct ?? 100}
              min={0}
              max={200}
              unit="%"
              apply={(v) => cmd.setInputGain(v)}
            />
          </Row>
          <Row label={t("settings.output_volume")}>
            <Slider
              value={state.status?.output_vol_pct ?? 100}
              min={0}
              max={200}
              unit="%"
              apply={(v) => cmd.setOutputVolume(v)}
            />
          </Row>
          <Show when={state.status?.voice_mode === "vad"}>
            <Row label={t("settings.vad_level")}>
              <Slider
                value={state.status?.vad_level_pct ?? 15}
                min={0}
                max={100}
                apply={(v) => cmd.setVadLevel(v)}
              />
            </Row>
          </Show>
          <Row label={t("settings.voice_mode")}>
            <ModePicker />
          </Row>
          <Row label={t("settings.sound_check")}>
            <SoundCheck />
          </Row>
          <div class="flex flex-col gap-1.5">
            <Toggle
              label={t("settings.denoise")}
              value={denoise()}
              onChange={async (v) => {
                setDenoise(v);
                await cmd.setDenoise(v);
                if (state.status) state.status.denoise = v;
              }}
            />
            <span class="text-xs text-muted">{t("settings.denoise_hint")}</span>
          </div>
          <div class="flex flex-col gap-1.5">
            <Toggle
              label={t("settings.paranoid")}
              value={para()}
              onChange={async (v) => {
                setPara(v);
                await cmd.setParanoid(v);
                if (state.status) state.status.paranoid = v;
              }}
            />
            <span class="text-xs text-muted">{t("settings.paranoid_hint")}</span>
          </div>
        </Section>

        <Section title={t("settings.hotkeys")}>
          <HotkeyRow
            action="mute"
            current={hotkeys()["mute"] ?? null}
            refresh={refreshHotkeys}
          />
          <HotkeyRow
            action="ptt"
            current={hotkeys()["ptt"] ?? null}
            refresh={refreshHotkeys}
          />
          <HotkeyRow
            action="quick_join"
            current={hotkeys()["quick_join"] ?? null}
            refresh={refreshHotkeys}
          />
          <div class="text-xs text-muted ml-32">
            {t("settings.hotkey_format", {
              example: isMac ? "Alt+M" : "Ctrl+Alt+M",
            })}
          </div>
        </Section>

        <Section title={t("settings.app")}>
          <Toggle
            label={t("settings.notifications_desc")}
            value={notify()}
            onChange={async (v) => {
              setNotify(v);
              await cmd.setNotifications(v);
            }}
          />
          <Toggle
            label={t("settings.tray_desc")}
            value={tray()}
            onChange={async (v) => {
              setTray(v);
              await cmd.setCloseToTray(v);
            }}
          />
          <Toggle
            label={t("settings.autostart_desc")}
            value={auto()}
            onChange={async (v) => {
              setAuto(v);
              try {
                await cmd.setAutostart(v);
              } catch (e) {
                alert(`autostart: ${e}`);
                setAuto(!v);
              }
            }}
          />
          <Row label={t("settings.language")}>
            <select
              class="bg-surface border border-line rounded px-2 py-1 text-sm"
              value={state.status?.language ?? "en"}
              onChange={async (e) => {
                const lang = (e.currentTarget.value as Lang) ?? "en";
                try {
                  await cmd.setLanguage(lang);
                  await refreshStatus();
                } catch (err) {
                  alert(`language: ${err}`);
                }
              }}
            >
              <For each={SUPPORTED_LANGS}>
                {(l) => <option value={l.code}>{l.label}</option>}
              </For>
            </select>
          </Row>
        </Section>

        <div class="text-center text-xs text-muted select-none pb-2">
          tc_ v{state.status?.version ?? "—"}
        </div>
      </div>
    </div>
  );
}
