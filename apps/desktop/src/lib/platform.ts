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

/// Render a Tauri accelerator string ("CommandOrControl+Shift+M", "Alt+V",
/// "F19") for display: Apple glyphs joined tight on macOS, spelled-out and
/// `+`-joined elsewhere.
export function formatAccel(accel: string): string {
  const mapped = accel
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => {
      switch (p.toLowerCase()) {
        case "commandorcontrol":
        case "cmdorctrl":
          return isMac ? "⌘" : "Ctrl";
        case "command":
        case "cmd":
        case "super":
        case "meta":
          return isMac ? "⌘" : "Super";
        case "control":
        case "ctrl":
          return isMac ? "⌃" : "Ctrl";
        case "alt":
        case "option":
          return isMac ? "⌥" : "Alt";
        case "shift":
          return isMac ? "⇧" : "Shift";
        default:
          return p.length === 1 ? p.toUpperCase() : p;
      }
    });
  return isMac ? mapped.join("") : mapped.join("+");
}
