// Where each name is currently standing. `Cast` fills this in as it renders;
// `Falling` reads it to launch a line of speech from the mouth that said it.
// A plain module-level map rather than context: there is exactly one stage,
// and the reader needs live DOM geometry, not a reactive value.

export const nameNodes = new Map<string, HTMLElement>();

/// Split a stored log line ("mara: text") into speaker and words. Returns null
/// for system lines, which have no author and never fall.
const CHAT_RE = /^([^:]{1,40}):\s(.*)$/s;

export function speakerOf(text: string): { who: string; said: string } | null {
  const m = text.match(CHAT_RE);
  return m ? { who: m[1], said: m[2] } : null;
}
