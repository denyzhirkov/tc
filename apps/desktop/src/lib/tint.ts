// Colour is identity, not decoration: a peer's hue is derived from their name
// and can never be picked. Two people are told apart the same way in every
// client, with no palette to configure.
//
// Caveat worth knowing: the roster the server hands us (`participants`) carries
// names, not public keys, so the hue follows the *name*. Renaming recolours a
// peer, and two identities sharing a name share a hue. The fingerprint — shown
// in settings — remains the thing that actually proves who someone is.

const HUES = new Map<string, number>();

export function hueOf(seed: string): number {
  const cached = HUES.get(seed);
  if (cached !== undefined) return cached;
  let h = 2166136261;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  const hue = (h >>> 0) % 360;
  HUES.set(seed, hue);
  return hue;
}

/// A peer's colour, resolved through the theme's saturation/lightness vars so
/// it stays legible on both grounds.
export function tint(seed: string): string {
  return `hsl(${hueOf(seed)} var(--sat) var(--lum))`;
}
