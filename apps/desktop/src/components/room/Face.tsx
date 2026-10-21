// A face above a name, in the heads look. Its features come from the name
// (`lib/face.ts`); its motion comes from Cast's one frame loop, through the
// painter this registers — a mouth that opens with the voice, a blink now and
// then, brows on a loud syllable, a head that rises with the voice.
//
// The painter writes attributes straight to the SVG, and only when what it
// would write has visibly changed. Transforms go through the SVG transform
// lists, which are numbers in place, so a frame allocates nothing but the
// mouth's path string — and that only while the mouth is moving.

import { For, onMount } from "solid-js";
import {
  BLINK_MS,
  BROW_RISE,
  MOUTH_ATTACK_CALM,
  MOUTH_RELEASE_CALM,
  blinkAmount,
  bobTilt,
  bobY,
  browLift,
  faceOf,
  mouthPath,
  nextBlink,
  rng,
  smoothLevel,
  type Part,
} from "../../lib/face";
import { tint } from "../../lib/tint";

/// `level` is the name's envelope (it sets presence); `goal` is the voice
/// right now, which the mouth smooths on its own, much faster terms.
export type Painter = (level: number, now: number, goal: number) => void;

/// Presence of a silent face. A face that faded as far as a silent name does
/// would stop reading as a face; this one only steps back.
const REST = 0.62;
/// Brows rise quickly and settle slowly, like a raised voice does.
const BROW_ATTACK = 14;
const BROW_RELEASE = 5;
/// How far a blink closes the eyes: a sliver is left, so it reads as a lid.
const LID = 0.9;

const PartPath = (p: { part: Part }) => <path class={`face-${p.part.ink}`} d={p.part.d} />;

export default function Face(props: {
  name: string;
  hushed: boolean;
  /// Reduced motion: no blink, no bob — the mouth still moves, softer.
  calm: boolean;
  own: (paint: Painter) => void;
}) {
  // The roster renders one Face per name and a rename remounts it, so the
  // features are decided once, here.
  const face = faceOf(props.name);
  let svg: SVGSVGElement | undefined;
  let head: SVGGElement | undefined;
  let brows: SVGGElement | undefined;
  let mouth: SVGPathElement | undefined;
  const lids: SVGGElement[] = [];

  onMount(() => {
    if (!svg || !head || !brows || !mouth) return;
    const calm = props.calm;
    const shift = svg.createSVGTransform();
    const tilt = svg.createSVGTransform();
    const rise = svg.createSVGTransform();
    head.transform.baseVal.appendItem(shift);
    head.transform.baseVal.appendItem(tilt);
    brows.transform.baseVal.appendItem(rise);
    const squash = lids.map((g) => {
      const t = svg!.createSVGTransform();
      g.transform.baseVal.appendItem(t);
      return t;
    });

    const rand = rng(face.blinkSeed);
    let open = 0;
    let lift = 0;
    let last = 0;
    let blinkAt = -1;
    let blinkNext = -1;
    let shownOpen = 0;
    let shownLid = 1;
    let shownLift = 0;
    let shownY = 0;
    let shownTilt = 0;
    let shownAlpha = -1;

    props.own((level, now, goal) => {
      const dt = last ? Math.min(0.05, (now - last) / 1000) : 0;
      last = now;

      open = calm
        ? smoothLevel(open, goal, dt, MOUTH_ATTACK_CALM, MOUTH_RELEASE_CALM)
        : smoothLevel(open, goal, dt);
      // 50 steps between shut and wide open: finer than a stroke can show.
      const o = Math.round(open * 50) / 50;
      if (o !== shownOpen) {
        shownOpen = o;
        mouth!.setAttribute("d", mouthPath(o, face.mouth, face.mouthWidth, face.mouthOpen));
      }

      // Reduced motion: the mouth alone moves. Presence stays at rest — a
      // face brightening with each syllable is motion too.
      const alpha = calm ? REST : Math.round((REST + (1 - REST) * Math.min(1, level)) * 50) / 50;
      if (alpha !== shownAlpha) {
        shownAlpha = alpha;
        head!.style.opacity = String(alpha);
      }

      if (calm) return;

      lift = smoothLevel(lift, browLift(open), dt, BROW_ATTACK, BROW_RELEASE);
      const l = Math.round(lift * BROW_RISE * 20) / 20;
      if (l !== shownLift) {
        shownLift = l;
        rise.setTranslate(0, -l);
      }

      if (blinkNext < 0) blinkNext = nextBlink(rand, now);
      if (blinkAt < 0 && now >= blinkNext) blinkAt = now;
      let shut = 0;
      if (blinkAt >= 0) {
        shut = blinkAmount(now - blinkAt);
        if (now - blinkAt >= BLINK_MS) {
          blinkAt = -1;
          blinkNext = nextBlink(rand, now);
        }
      }
      const sy = Math.round((1 - LID * shut) * 20) / 20;
      if (sy !== shownLid) {
        shownLid = sy;
        for (const t of squash) t.setScale(1, sy);
      }

      const y = Math.round(bobY(open) * 20) / 20;
      const a = Math.round(bobTilt(open, now / 1000, face.phase) * 20) / 20;
      if (y !== shownY || a !== shownTilt) {
        shownY = y;
        shownTilt = a;
        shift.setTranslate(0, y);
        tilt.setRotate(a, face.box.cx, face.box.cy);
      }
    });
  });

  return (
    <svg
      ref={svg}
      class="face-art"
      classList={{ "face-hushed": props.hushed }}
      viewBox="0 0 100 100"
      style={{ color: tint(props.name) }}
      aria-hidden="true"
    >
      <g ref={head}>
        <For each={face.back}>{(part) => <PartPath part={part} />}</For>
        <For each={face.eyeAt}>
          {(at) => (
            <g transform={`translate(${at.x.toFixed(2)} ${at.y.toFixed(2)})`}>
              <g ref={(g) => lids.push(g)}>
                <For each={face.eye}>{(part) => <PartPath part={part} />}</For>
              </g>
            </g>
          )}
        </For>
        <g ref={brows}>
          <For each={face.brow}>{(part) => <PartPath part={part} />}</For>
        </g>
        <path
          ref={mouth}
          class="face-mouth"
          transform={`translate(${face.mouthAt.x.toFixed(2)} ${face.mouthAt.y.toFixed(2)})`}
          d={mouthPath(0, face.mouth, face.mouthWidth, face.mouthOpen)}
        />
        <For each={face.front}>{(part) => <PartPath part={part} />}</For>
      </g>
    </svg>
  );
}
