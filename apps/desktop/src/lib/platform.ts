// Platform-aware accelerator labels. macOS uses the Apple symbols; everywhere
// else we spell it out (`Ctrl+K`, `Alt+M`). Driven by `navigator.platform`
// and `navigator.userAgent` — Tauri's own OS detection requires async, which
// is overkill for static UI labels.

const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
const plat = typeof navigator !== "undefined" ? navigator.platform : "";

export const isMac = /Mac|iPhone|iPad|iPod/i.test(plat) || /Macintosh/.test(ua);

/// The system "command" key label as it appears in shortcut hints.
export const cmdKey = isMac ? "⌘" : "Ctrl";

/// "⌘K" on macOS, "Ctrl+K" elsewhere.
export function shortcut(letter: string): string {
  return isMac ? `⌘${letter.toUpperCase()}` : `Ctrl+${letter.toUpperCase()}`;
}
