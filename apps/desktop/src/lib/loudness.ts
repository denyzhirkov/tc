// How loud a voice is, on the one scale the room animates from.
//
// The core reports honest per-frame loudness (`client/level.rs`): a frame's
// RMS mapped linearly in dBFS, −50 → 0 and −12 → 1, so speech at the AGC
// target reads ≈ 0.76 and the VAD threshold ≈ 0.37. Everything the room draws
// was tuned on the old packet-size proxy, which read ≈ 0.6–0.8 in speech and
// ~0 otherwise — never a noise floor. `voiceLevel` restores that contract once,
// at the source: below the gate is silence, above it the scale is stretched
// back to 0..1. Every threshold downstream keeps its old meaning.
//
// Pure on purpose (no Solid, no DOM): node tests it with type stripping only.

/// Raw loudness below which nothing is drawn: ≈ −38.6 dBFS, just under the
/// *default* VAD threshold (`config::VAD_RMS_THRESHOLD`, ≈ −36 dBFS → 0.37;
/// each user can move their own). An open microphone's room noise, a fan,
/// Opus concealment tails all sit under it; what a sender at the default
/// threshold transmits sits above it.
export const VOICE_GATE = 0.3;

/// Mirrors `config::LEVEL_FLOOR_DBFS` / `LEVEL_CEIL_DBFS` (crates/shared) —
/// the frontend has no way to import a Rust constant, and the 5 Hz payload
/// still carries our own microphone as raw RMS (`input_peak`), so it is
/// mapped here. Change both or neither.
export const LEVEL_FLOOR_DBFS = -50;
export const LEVEL_CEIL_DBFS = -12;

/// A fast-lane frame older than this, while it still said someone was
/// audible, means the lane died mid-word. The lane emits every ~33 ms while
/// anyone is audible, so this is ~15 missed frames.
export const FAST_STALE_MS = 500;
/// A quiet lane is silent by contract, so one that died in a quiet room looks
/// exactly like one that is fine. Past this long without a frame, a clearly
/// voiced peer on the 5 Hz path is taken as proof the lane is gone.
export const FAST_DEAD_MS = 1000;

/// Someone waiting in the quiet line is lifted by a clearly voiced frame
/// (raw ≈ −33 dBFS), not by one that barely clears the gate — a breath or a
/// hangover tail near the VAD threshold. Anything loud lifts, a cough
/// included; the room's swap-rate cap keeps that from churning the ranks.
export const LIFT_AT = 0.2;

/// Raw core loudness 0..1 → the room's level 0..1 (silence below the gate).
export function voiceLevel(raw: number): number {
  if (!(raw > VOICE_GATE)) return 0;
  return Math.min(1, (raw - VOICE_GATE) / (1 - VOICE_GATE));
}

/// `client/level.rs::loudness` for a raw RMS (full scale = 1).
export function loudnessOfRms(rms: number): number {
  if (!Number.isFinite(rms) || rms <= 0) return 0;
  const dbfs = 20 * Math.log10(rms);
  const l = (dbfs - LEVEL_FLOOR_DBFS) / (LEVEL_CEIL_DBFS - LEVEL_FLOOR_DBFS);
  return Math.max(0, Math.min(1, l));
}

/// Whether the fast lane speaks for the room right now. It is silent by
/// contract when nobody is audible ("no event means the last frame still
/// holds"), so an all-zero last frame stays true however old it is; only a
/// lane that went silent mid-voice, or never spoke at all (`at` 0 — an older
/// core, or nobody has made a sound yet), hands over to the 5 Hz path.
export function fastFresh(at: number, audible: boolean, now: number): boolean {
  if (at <= 0) return false;
  return !audible || now - at <= FAST_STALE_MS;
}

/// Everything `resolveLevel` needs about one person at one moment. Raw values
/// (core loudness, RMS for the 5 Hz microphone); the gate is applied inside.
/// A mutable record so a caller in a frame loop can refill one instead of
/// allocating per name per frame.
export type LevelQuery = {
  now: number;
  /// A DM is a written room: no voice at all.
  dm: boolean;
  isMe: boolean;
  muted: boolean;
  /// Push-to-talk: the microphone is not what peers hear.
  ptt: boolean;
  /// The fast lane: time of its last frame (0 = none yet), whether that frame
  /// had anyone audible, and its level for this person.
  fastAt: number;
  fastAudible: boolean;
  fast: number;
  /// The 5 Hz path: `input_peak` RMS for us, `speakers[name]` for a peer.
  slow: number;
};

/// How loud this person is right now, on the room's scale.
///
/// The fast lane while it speaks for the room (`fastFresh`), the 5 Hz payload
/// otherwise. Our own 5 Hz level is the raw microphone, before VAD or PTT, so
/// under push-to-talk it is not trusted at all — a mouth must not move with
/// words nobody hears. The dead-lane guard is for peers only: our raw
/// microphone can be loud while the lane rightly says nothing was sent, so it
/// cannot vouch for the lane being dead.
export function resolveLevel(q: LevelQuery): number {
  if (q.dm) return 0;
  if (q.isMe && q.muted) return 0;
  const slow = q.isMe ? (q.ptt ? 0 : voiceLevel(loudnessOfRms(q.slow))) : voiceLevel(q.slow);
  if (!fastFresh(q.fastAt, q.fastAudible, q.now)) return slow;
  if (!q.isMe && q.now - q.fastAt > FAST_DEAD_MS && slow > LIFT_AT) return slow;
  return voiceLevel(q.fast);
}

/// How a name follows its voice: the peak of the last `ENV_HOLD_MS` is held,
/// risen to at `ENV_ATTACK`/s and let go at `ENV_RELEASE`/s. The fast lane
/// carries every syllable, and a name that shrank in each gap between them
/// would throb where it used to swell; held this way it swells for the phrase
/// and settles after it. Faces do not use this — a mouth is per syllable.
export const ENV_HOLD_MS = 250;
export const ENV_ATTACK = 8;
export const ENV_RELEASE = 3;

/// One name's envelope. Mutable on purpose: stepped once per frame per name.
export class Envelope {
  value = 0;
  private held = 0;
  private heldAt = -Infinity;
  private last = -1;

  /// Feed this frame's level at `now` (ms); returns the envelope.
  step(level: number, now: number): number {
    const dt = this.last < 0 ? 0 : Math.min(0.05, Math.max(0, (now - this.last) / 1000));
    this.last = now;
    if (level >= this.held || now - this.heldAt > ENV_HOLD_MS) {
      this.held = level;
      this.heldAt = now;
    }
    const rate = this.held > this.value ? ENV_ATTACK : ENV_RELEASE;
    this.value += (this.held - this.value) * Math.min(1, dt * rate);
    return this.value;
  }
}
