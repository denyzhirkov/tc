// Bit stream derived from a string seed (hex pubkey or any free-form name).
// Used by the spiral identicon to decide which dots are filled.
//
// Strategy:
//   - If `seed` parses as hex bytes, walk those bytes — each byte yields 8 bits
//     and we wrap around if we need more.
//   - Otherwise (e.g. a peer name) hash the string with FNV-1a and expand it
//     with a fast LCG so we get a deterministic, uniform-looking bit stream.

export function seedBits(seed: string, count: number): boolean[] {
  const out = new Array<boolean>(count);
  const clean = seed.replace(/^0x/, "");
  const bytes: number[] = [];
  if (/^[0-9a-fA-F]+$/.test(clean) && clean.length >= 8 && clean.length % 2 === 0) {
    for (let i = 0; i < clean.length; i += 2) {
      bytes.push(parseInt(clean.substr(i, 2), 16));
    }
  }
  if (bytes.length > 0) {
    for (let i = 0; i < count; i++) {
      const byte = bytes[i % bytes.length];
      const bit = (i / bytes.length) | 0;
      out[i] = ((byte >> (bit % 8)) & 1) === 1;
    }
    return out;
  }
  // Free-form seed → FNV-1a + LCG expansion.
  let h = 2166136261 >>> 0;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  for (let i = 0; i < count; i++) {
    h = (Math.imul(h, 1103515245) + 12345) >>> 0;
    out[i] = ((h >> 16) & 1) === 1;
  }
  return out;
}
