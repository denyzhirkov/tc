// How a channel id is spoken about.
//
// The wire calls a public channel `pub-lobby`; the prefix is routing detail
// the server needs and a person does not. Rooms are shown by their name, and
// a private one — which has no prefix at all — is marked with a leading dot
// instead. Commands and `joinChannel` always take the full id.

const PUBLIC_PREFIX = "pub-"; // mirrors config::PUBLIC_CHANNEL_PREFIX

export const isPrivateRoom = (id: string) => !id.startsWith(PUBLIC_PREFIX);

export function roomLabel(id: string): string {
  return id.startsWith(PUBLIC_PREFIX) ? id.slice(PUBLIC_PREFIX.length) : `·${id}`;
}
