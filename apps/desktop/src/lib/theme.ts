// UI tweaks (theme / accent / typeface). Persisted in localStorage, applied
// to :root via CSS variables so any component can react instantly.

import { createEffect } from "solid-js";
import { createStore } from "solid-js/store";

export type Theme = "dark" | "light";
export type Accent = "indigo" | "forest" | "rust" | "slate";
export type Typeface = "inter" | "plex" | "mono";
// How this client draws identicons. Purely local presentation — never sent to
// peers; each user sees every avatar in their own chosen style.
export type AvatarStyle = "weave" | "pixel" | "faces";

export type Tweaks = {
  theme: Theme;
  accent: Accent;
  typeface: Typeface;
  avatarStyle: AvatarStyle;
  /// Show the subtle hotkey-hint strip above the composer input.
  composerHints: boolean;
};

const KEY = "tc.tweaks";

const ACCENTS: Record<Accent, string> = {
  indigo: "#5b6cff",
  forest: "#3a8a5e",
  rust: "#c2562b",
  slate: "#7a7d83",
};

const FONTS: Record<Typeface, string> = {
  inter:
    "'Inter Tight','Inter',ui-sans-serif,-apple-system,system-ui,sans-serif",
  plex: "'IBM Plex Sans',ui-sans-serif,-apple-system,system-ui,sans-serif",
  mono: "'JetBrains Mono','SF Mono',Menlo,Consolas,ui-monospace,monospace",
};

const AVATAR_STYLES: AvatarStyle[] = ["weave", "pixel", "faces"];

const DEFAULTS: Tweaks = {
  theme: "dark",
  accent: "indigo",
  typeface: "inter",
  avatarStyle: "weave",
  composerHints: true,
};

function load(): Tweaks {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw);
    const merged = { ...DEFAULTS, ...parsed };
    // Migrate legacy values: `serif` (Newsreader) was retired in favour of
    // a monospace option — fall back to the default sans typeface.
    if (!(merged.typeface in FONTS)) merged.typeface = DEFAULTS.typeface;
    if (!AVATAR_STYLES.includes(merged.avatarStyle))
      merged.avatarStyle = DEFAULTS.avatarStyle;
    return merged;
  } catch {
    return { ...DEFAULTS };
  }
}

const [tweaks, setTweaks] = createStore<Tweaks>(load());

export { tweaks };

export function setTweak<K extends keyof Tweaks>(key: K, value: Tweaks[K]) {
  setTweaks(key, value);
  try {
    localStorage.setItem(KEY, JSON.stringify(tweaks));
  } catch {}
}

/// Apply current tweaks to the document. Call once at startup; the effect
/// inside will keep CSS vars in sync as tweaks change.
export function installTheme() {
  const root = document.documentElement;
  createEffect(() => {
    root.dataset.theme = tweaks.theme;
    root.style.setProperty("--c-accent", ACCENTS[tweaks.accent]);
    root.style.setProperty("--font-ui", FONTS[tweaks.typeface]);
  });
}
