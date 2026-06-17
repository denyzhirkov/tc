// Deterministic avatar for a peer, keyed by Ed25519 pubkey hex (or any string
// seed). Three interchangeable styles, chosen per-client in Settings — the
// choice is pure local presentation and is never sent to peers:
//
//   • weave  — Truchet quarter-arc tiling, 2–3 colours from the key
//   • pixel  — mirrored 5×5 sigil, 2–3 colours from the key (GitHub-ish)
//   • faces  — DiceBear `fun-emoji` portraits (its own deterministic colours)
//
// All three render inside a squircle (rounded square, rx ≈ 28%). Same key
// always yields the same avatar.

import { Show, createMemo, createUniqueId } from "solid-js";
import { Avatar, Style } from "@dicebear/core";
import funEmoji from "@dicebear/styles/fun-emoji.json";
import { seedBits, seedCells, seedPalette } from "../lib/identicon";
import { tweaks, type AvatarStyle } from "../lib/theme";

// Parse the style definition once and reuse it across every face avatar.
const FACE_STYLE = new Style(funEmoji as any);

// Geometry lives in a 100-unit canvas; the <svg> is scaled to `size` px.
const RADIUS = 28; // squircle corner radius in canvas units (~28%)
const WEAVE_N = 4; // Truchet grid is N×N
const PIXEL_GRID = 5; // sigil is 5×5, mirrored on the vertical axis

export default function Identicon(props: {
  pubkeyHex: string | null | undefined;
  size?: number;
  /// Override the global style — used by the Settings preview to show all three.
  variant?: AvatarStyle;
}) {
  const size = () => props.size ?? 32;
  const style = () => props.variant ?? tweaks.avatarStyle;

  return (
    <Show
      when={props.pubkeyHex}
      fallback={
        <svg
          width={size()}
          height={size()}
          viewBox="0 0 100 100"
          style={{ display: "block", "flex-shrink": 0 }}
        >
          <rect
            x="1"
            y="1"
            width="98"
            height="98"
            rx={RADIUS}
            fill="none"
            stroke="currentColor"
            stroke-opacity="0.2"
            stroke-width="1.2"
          />
        </svg>
      }
    >
      {(seed) => (
        <Show when={style() === "faces"} fallback={<Sigil seed={seed()} size={size()} variant={style()} />}>
          <FaceAvatar seed={seed()} size={size()} />
        </Show>
      )}
    </Show>
  );
}

// ── DiceBear faces ──────────────────────────────────────────────────────────
// fun-emoji carries its own seed-derived palette; we only clip it to a squircle.
function FaceAvatar(props: { seed: string; size: number }) {
  const svg = createMemo(() =>
    new Avatar(FACE_STYLE, {
      seed: props.seed,
      size: props.size,
    }).toString(),
  );
  return (
    <div
      // eslint-disable-next-line solid/no-innerhtml
      innerHTML={svg()}
      style={{
        width: `${props.size}px`,
        height: `${props.size}px`,
        "border-radius": `${RADIUS}%`,
        overflow: "hidden",
        "flex-shrink": 0,
        display: "block",
      }}
    />
  );
}

// ── SVG sigils (weave / pixel) ────────────────────────────────────────────────
function Sigil(props: { seed: string; size: number; variant: AvatarStyle }) {
  const clipId = createUniqueId();
  const pal = createMemo(() => seedPalette(props.seed));
  const shapes = createMemo(() =>
    props.variant === "pixel"
      ? pixelShapes(props.seed, pal().colors)
      : weaveShapes(props.seed, pal().colors),
  );

  return (
    <svg
      width={props.size}
      height={props.size}
      viewBox="0 0 100 100"
      style={{ display: "block", "flex-shrink": 0 }}
    >
      <defs>
        <clipPath id={clipId}>
          <rect x="0" y="0" width="100" height="100" rx={RADIUS} />
        </clipPath>
      </defs>
      <g clip-path={`url(#${clipId})`}>
        <rect x="0" y="0" width="100" height="100" fill={pal().bg} />
        {shapes()}
      </g>
      <rect
        x="0.6"
        y="0.6"
        width="98.8"
        height="98.8"
        rx={RADIUS - 0.6}
        fill="none"
        stroke={pal().ink}
        stroke-opacity="0.12"
        stroke-width="1.2"
      />
    </svg>
  );
}

// Truchet weave: each cell draws two quarter-arcs whose orientation (flip) and
// colour come from the seed; adjacent arcs join into flowing ribbons.
function weaveShapes(seed: string, colors: string[]) {
  const cells = seedCells(seed, WEAVE_N * WEAVE_N);
  const c = 100 / WEAVE_N;
  const r = c / 2;
  const sw = c * 0.3;
  const out: any[] = [];
  for (let row = 0; row < WEAVE_N; row++) {
    for (let col = 0; col < WEAVE_N; col++) {
      const { flip, color } = cells[row * WEAVE_N + col];
      const x = col * c;
      const y = row * c;
      const d = flip
        ? // arcs hugging the top-right and bottom-left corners
          `M ${x + c - r} ${y} A ${r} ${r} 0 0 0 ${x + c} ${y + r} ` +
          `M ${x} ${y + c - r} A ${r} ${r} 0 0 1 ${x + r} ${y + c}`
        : // arcs hugging the top-left and bottom-right corners
          `M ${x + r} ${y} A ${r} ${r} 0 0 1 ${x} ${y + r} ` +
          `M ${x + c - r} ${y + c} A ${r} ${r} 0 0 1 ${x + c} ${y + c - r}`;
      out.push(
        <path
          d={d}
          fill="none"
          stroke={colors[color]}
          stroke-width={sw.toFixed(2)}
          stroke-linecap="round"
        />,
      );
    }
  }
  return out;
}

// Mirrored pixel sigil: fill bits + per-cell colour, reflected on the vertical
// axis so the half-grid (3 columns) mirrors to a symmetric 5×5.
function pixelShapes(seed: string, colors: string[]) {
  const half = Math.ceil(PIXEL_GRID / 2); // 3 columns drive the mirror
  const bits = seedBits(seed, PIXEL_GRID * half);
  const cells = seedCells(seed, PIXEL_GRID * half);
  const pad = 9;
  const cell = (100 - pad * 2) / PIXEL_GRID;
  const rr = cell * 0.24;
  const out: any[] = [];
  for (let row = 0; row < PIXEL_GRID; row++) {
    for (let col = 0; col < PIXEL_GRID; col++) {
      const srcCol = col < half ? col : PIXEL_GRID - 1 - col;
      const i = row * half + srcCol;
      if (!bits[i]) continue;
      out.push(
        <rect
          x={(pad + col * cell).toFixed(2)}
          y={(pad + row * cell).toFixed(2)}
          width={cell.toFixed(2)}
          height={cell.toFixed(2)}
          rx={rr.toFixed(2)}
          fill={colors[cells[i].color]}
        />,
      );
    }
  }
  return out;
}
