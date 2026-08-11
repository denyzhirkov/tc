// The other way a voice can look: instead of a string with a wave on it, the
// name sheds drops. They sink, sway, and dissolve — a lava lamp running the
// wrong way round, because in this room everything a person says falls.
//
// Driven by the same smoothed level as `trace.ts`, from the same frame loop:
// loudness decides how often a drop is shed and how big it is. Nothing else is
// computed per frame — once born, a drop is a CSS animation and belongs to the
// compositor, exactly like a falling line of speech (`Falling.tsx`).

/// Drops per second at full voice. Quiet speech drips; a shout pours.
const RATE = 7;
/// Most drops from one name at once. Past this it reads as fog, not as voice.
const MAX = 16;
/// Below this a voice sheds nothing — the same silence the string hangs at.
const HUSH = 0.05;

export class Lava {
  /// Whether this voice is audible right now. Kept for symmetry with `Trace`;
  /// bubbles need no visibility gating of their own, since a drop only exists
  /// because someone spoke.
  audible = false;

  private accum = 0;
  private last = 0;

  constructor(private host: HTMLElement) {}

  /// Feed the current smoothed level. Sheds drops; draws nothing itself.
  step(level: number, now: number) {
    const dt = this.last ? Math.min(0.05, (now - this.last) / 1000) : 0;
    this.last = now;
    this.audible = level > HUSH;
    if (!this.audible) {
      // Don't bank silence — a pause shouldn't burst when the voice returns.
      this.accum = 0;
      return;
    }
    this.accum += dt * RATE * (0.25 + level);
    while (this.accum >= 1) {
      this.accum -= 1;
      this.shed(level);
    }
  }

  private shed(level: number) {
    if (this.host.childElementCount >= MAX) return;

    const drop = document.createElement("div");
    drop.className = "bub";
    // Everything the animation needs, decided once. Louder means bigger,
    // slower and more opaque — the three ways this reads as volume.
    drop.style.setProperty("--r", `${(8 + level * 18 + Math.random() * 5).toFixed(1)}px`);
    drop.style.setProperty("--x", `${(50 + (Math.random() * 2 - 1) * 20).toFixed(1)}%`);
    // Long enough that the taller column is sunk through rather than fallen
    // down: the drop must always look heavier than it is fast.
    drop.style.setProperty("--life", `${(5 + level * 2 + Math.random() * 2.6).toFixed(2)}s`);
    drop.style.setProperty("--peak", (0.5 + level * 0.35).toFixed(2));
    drop.style.setProperty("--sway", `${(3 + Math.random() * 7).toFixed(1)}px`);
    drop.style.setProperty("--swayTime", `${(1.6 + Math.random() * 1.6).toFixed(2)}s`);
    // A shared phase would make every drop swing in step, which reads as a
    // machine. A negative delay starts each one already mid-swing.
    drop.style.setProperty("--phase", Math.random().toFixed(3));

    const skin = document.createElement("div");
    skin.className = "bub-skin";
    drop.appendChild(skin);

    // The sway loops forever, so the only animation that ever ends is the fall
    // — but it is the skin that would report it, hence the target check.
    drop.addEventListener("animationend", (e) => {
      if (e.target === drop) drop.remove();
    });
    this.host.appendChild(drop);
  }
}
