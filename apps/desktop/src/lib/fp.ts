// Stable 6-hex faux-fingerprint from any string. We don't have per-peer
// pubkeys at the UI layer, so this gives each peer a deterministic identifier
// chip until real fingerprints are wired through.

export function fpFor(s: string, len = 6): string {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0).toString(16).padStart(8, "0").slice(0, len);
}
