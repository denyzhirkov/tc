// Mono icons. 16px default, 1.5 stroke. currentColor → easy to recolor.
//
// The room draws no icons at all — state is carried by words and by type. What
// remains is what the ⌘K palette needs to label its rows; the mic / phone /
// speaker / send set went out with the components that used them.

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

export const Search = (p: P) =>
  SVG(p, [<circle cx="7" cy="7" r="4" />, <path d="M10 10l3.5 3.5" />]);

export const Gear = (p: P) =>
  SVG(p, [
    <circle cx="8" cy="8" r="2.2" />,
    <path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8L3.4 3.4" />,
  ]);
