// Mono icons. 16px default, 1.5 stroke. currentColor → easy to recolor.

type P = { class?: string; size?: number };

const SVG = (p: P, children: any, vb = "0 0 16 16") => (
  <svg
    width={p.size ?? 16}
    height={p.size ?? 16}
    viewBox={vb}
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    class={p.class}
  >
    {children}
  </svg>
);

export const Hash = (p: P) =>
  SVG(p, <path d="M2.5 6h11M2.5 10h11M6.5 2.5l-1 11M10.5 2.5l-1 11" />);

export const Mic = (p: P) =>
  SVG(p, [
    <rect x="6" y="2" width="4" height="8" rx="2" />,
    <path d="M3.5 8a4.5 4.5 0 009 0M8 12.5V14" />,
  ]);

export const MicOff = (p: P) =>
  SVG(p, [
    <rect x="6" y="2" width="4" height="8" rx="2" />,
    <path d="M3.5 8a4.5 4.5 0 009 0M8 12.5V14M2 2l12 12" />,
  ]);

export const Phone = (p: P) =>
  SVG(
    p,
    <path d="M3.5 3.5h2l1 3-1.5 1a7 7 0 003.5 3.5l1-1.5 3 1v2a1 1 0 01-1 1A10 10 0 012.5 4.5a1 1 0 011-1z" />,
  );

export const PhoneOff = (p: P) =>
  SVG(p, [
    <path d="M2 8a8 8 0 0112 0l-1.5 1.5-1.5-1V7a7 7 0 00-6 0v1.5l-1.5 1z" />,
    <path d="M2 14L14 2" />,
  ]);

export const Send = (p: P) =>
  SVG(p, <path d="M2.5 8L13.5 3 11 13.5 7.5 9.5 2.5 8z" />);

export const Plus = (p: P) => SVG(p, <path d="M8 3.5v9M3.5 8h9" />);

export const Search = (p: P) =>
  SVG(p, [<circle cx="7" cy="7" r="4" />, <path d="M10 10l3.5 3.5" />]);

export const Gear = (p: P) =>
  SVG(p, [
    <circle cx="8" cy="8" r="2.2" />,
    <path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8L3.4 3.4" />,
  ]);

export const Chev = (p: P & { open?: boolean }) =>
  SVG(
    { ...p, size: p.size ?? 10 },
    <path d={p.open ? "M2 4l3 3 3-3" : "M4 2l3 3-3 3"} />,
    "0 0 10 10",
  );

export const SpeakerOff = (p: P) =>
  SVG(p, [
    <path d="M3 6h2.5L9 3v10L5.5 10H3z" />,
    <path d="M11 6l3 3M14 6l-3 3" />,
  ]);

export const Speaker = (p: P) =>
  SVG(p, [
    <path d="M3 6h2.5L9 3v10L5.5 10H3z" />,
    <path d="M11.5 5.5a3.5 3.5 0 010 5M13 4a6 6 0 010 8" />,
  ]);

export const Download = (p: P) =>
  SVG(p, [
    <path d="M8 2v8M5 7l3 3 3-3" />,
    <path d="M3 12.5h10" />,
  ]);
