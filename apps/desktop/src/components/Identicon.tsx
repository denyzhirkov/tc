// Spiral identicon. Phyllotaxis layout (golden-angle increment + sqrt radial
// growth) produces a sunflower-like pattern of points fanning out from the
// centre. Each candidate point is filled or skipped based on a bit derived
// from the fingerprint, so the same key always renders the same shape.
//
// Pure SVG, monochrome (currentColor) — adopts whatever text colour the
// parent sets, so it just works in dark/light/accent contexts.

import { Show } from "solid-js";
import { seedBits } from "../lib/identicon";

const GOLDEN = Math.PI * (3 - Math.sqrt(5)); // ≈137.5°
const POINTS = 110;

export default function Identicon(props: {
  pubkeyHex: string | null | undefined;
  size?: number;
}) {
  const size = () => props.size ?? 28;
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
          <circle
            cx="50"
            cy="50"
            r="48"
            fill="none"
            stroke="currentColor"
            stroke-opacity="0.2"
            stroke-width="1.2"
          />
        </svg>
      }
    >
      {(seed) => {
        const s = size();
        const c = s / 2;
        // Dot radius scales with avatar size; capped so dots don't merge.
        const dotR = Math.max(0.55, s / 30);
        const maxR = c - dotR - 0.5;
        const bits = seedBits(seed(), POINTS);

        const dots: any[] = [];
        for (let i = 0; i < POINTS; i++) {
          if (!bits[i]) continue;
          // Skip the very centre (i=0) — leaves a small breathing hole that
          // makes the spiral arms read more clearly.
          const t = (i + 1) / POINTS;
          const radius = maxR * Math.sqrt(t);
          const angle = i * GOLDEN;
          const x = c + radius * Math.cos(angle);
          const y = c + radius * Math.sin(angle);
          // Inner dots ride a touch heavier than outer ones — gives the
          // sigil a soft optical centre instead of looking flat.
          const fade = 0.55 + 0.45 * (1 - t);
          dots.push(
            <circle
              cx={x.toFixed(2)}
              cy={y.toFixed(2)}
              r={dotR.toFixed(2)}
              fill="currentColor"
              opacity={fade.toFixed(2)}
            />,
          );
        }

        return (
          <svg
            width={s}
            height={s}
            viewBox={`0 0 ${s} ${s}`}
            style={{ display: "block", "flex-shrink": 0 }}
          >
            <circle
              cx={c}
              cy={c}
              r={c - 0.5}
              fill="none"
              stroke="currentColor"
              stroke-opacity="0.18"
              stroke-width="0.8"
            />
            {dots}
          </svg>
        );
      }}
    </Show>
  );
}
