// UI tweaks (theme / accent / typeface). Persisted in localStorage, applied
// to :root via CSS variables so any component can react instantly.

import { createEffect } from "solid-js";
import { createStore } from "solid-js/store";

export type Theme = "dark" | "light";
/// Colour of the chosen word. "plain" is the original look — brightness
/// alone. The named hues cost luminance against the unselected grey, which
/// is why picking one also adds weight (see --chosen-weight in styles.css).
export type Accent = "plain" | "indigo" | "forest" | "rust";
export type Typeface = "inter" | "plex" | "mono";
// How this client draws identicons. Purely local presentation — never sent to
// peers; each user sees every avatar in their own chosen style.
export type AvatarStyle = "weave" | "pixel" | "faces";

/// What a crowded room does with people who aren't talking: keep only the
/// recent voices on stage, or show everyone however small they get.
export type Crowd = "recent" | "all";

export type Tweaks = {
  theme: Theme;
  accent: Accent;
  typeface: Typeface;
  avatarStyle: AvatarStyle;
  /// Show the key hints under the input line.
  composerHints: boolean;
  crowd: Crowd;
};

const KEY = "tc.tweaks";

const FONTS: Record<Typeface, string> = {
  inter:
    "'Inter Tight','Inter',ui-sans-serif,-apple-system,system-ui,sans-serif",
  plex: "'IBM Plex Sans',ui-sans-serif,-apple-system,system-ui,sans-serif",
  mono: "'JetBrains Mono','SF Mono',Menlo,Consolas,ui-monospace,monospace",
};

const AVATAR_STYLES: AvatarStyle[] = ["weave", "pixel", "faces"];

// The room is typeset in one face, and that face is monospace: the terminal
// heritage is carried by the type rather than by decoration.
const ACCENTS: Accent[] = ["plain", "indigo", "forest", "rust"];

const DEFAULTS: Tweaks = {
  theme: "dark",
  accent: "plain",
  typeface: "mono",
  avatarStyle: "weave",
  composerHints: true,
  crowd: "recent",
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
    // The old accent set (indigo/forest/rust/slate) drove a variable nothing
    // read; "slate" is gone because a grey accent cannot be told from plain.
    if (!ACCENTS.includes(merged.accent)) merged.accent = DEFAULTS.accent;
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
    root.dataset.accent = tweaks.accent;
    root.style.setProperty("--font-ui", FONTS[tweaks.typeface]);
  });
}
