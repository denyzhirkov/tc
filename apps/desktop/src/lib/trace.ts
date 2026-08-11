// A string hangs from every name. When someone speaks, a wave runs down it and
// away from them — loud makes it swing wide, and the far end always calms.
//
// The wave is driven by real loudness, not by a timer: levels reach the
// frontend five times a second (`level_pump.rs`) and are smoothed again in
// Cast's frame loop, so what travels down the string is that envelope. Each
// committed sample carries its own phase, so a burst of voice moves downward
// as one crest instead of the whole string wobbling in place.

/// About 4s of voice hangs on the string at once. A longer string holds more
/// voice rather than the same wave stretched thin — the crests keep their
/// spacing on screen, and one still takes 800ms to pass.
const SAMPLES = 40;
/// Milliseconds between committed samples — the speed the wave travels at.
const TICK = 100;
/// Points the string is drawn with. The ring holds 40 levels; a sine sampled
/// that coarsely reads as a zigzag, so the curve is interpolated between them.
const POINTS = 76;
/// Radians of wave per committed sample — eight samples to a wavelength, so
/// roughly five crests are on the string and one passes every 800ms.
const DPHI = Math.PI / 4;
/// Widest swing, in half-widths of the element.
const AMP = 0.85;
/// Audible means visible: the quietest voice still moves the string.
const FLOOR = 0.06;
const HUSH = 0.02;

export class Trace {
  private ring = new Float32Array(SAMPLES);
  private head = 0;
  private commits = 0;
  private last = 0;
  /// Starts true so the first call draws the string at rest, then settles.
  private moving = true;

  /// Whether this voice is audible *right now*. The string shows itself while
  /// true and fades over ~3s once it goes false — the wave already in flight
  /// rides out under the fade (see `.trace path` in styles.css).
  audible = false;

  /// Feed the current smoothed level. Returns the path to draw, or null when
  /// the string is already hanging still — a silent room costs nothing.
  step(level: number, now: number): string | null {
    this.audible = level > HUSH;
    if (now - this.last >= TICK) {
      this.last = now;
      this.head = (this.head + 1) % SAMPLES;
      this.commits++;
      this.ring[this.head] = level;
      const was = this.moving;
      this.moving = this.ring.some((v) => v > HUSH);
      // One last draw as it comes to rest, then nothing until someone speaks.
      return was || this.moving ? this.path(now) : null;
    }
    return this.moving ? this.path(now) : null;
  }

  /// The string in a normalised viewBox ("-1 0 2 1"): x spans [-1, 1] and y
  /// spans [0, 1]. Pixel size is CSS's business, so a crowded room is a style
  /// rule rather than a branch in here.
  private path(now: number): string {
    // Where we are between two samples. Without it the wave would advance in
    // ten visible steps a second instead of travelling.
    const frac = Math.min(1, (now - this.last) / TICK);
    let d = "";
    for (let p = 0; p < POINTS; p++) {
      const u = p / (POINTS - 1);
      const f = Math.max(0, u * (SAMPLES - 1) - frac);
      const i = Math.floor(f);
      const a =
        this.level(i) + (this.level(i + 1) - this.level(i)) * (f - i);
      // The far end of a string always calms, however hard it is driven.
      const amp = a * AMP * (1 - u) ** 0.55;
      const x = amp * Math.sin((this.commits + frac - u * (SAMPLES - 1)) * DPHI);
      d += `${p ? "L" : "M"}${x.toFixed(4)} ${u.toFixed(4)}`;
    }
    return d;
  }

  /// Level `i` commits ago, gated so silence is a straight line rather than a
  /// hairline tremble.
  private level(i: number): number {
    const v = this.ring[(this.head - Math.min(i, SAMPLES - 1) + SAMPLES) % SAMPLES];
    return v > HUSH ? Math.min(1, FLOOR + v) : 0;
  }
}
