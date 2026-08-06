// The whole control surface. There is exactly one kind of control in this
// interface — a row of words where the lit one is chosen — plus a row of
// ticks for the two or three things that are genuinely continuous.

import { For, Show, type JSX } from "solid-js";

export function Word(props: {
  children: JSX.Element;
  onClick?: () => void;
  current?: boolean;
  title?: string;
  tone?: "quiet" | "plain" | "loud" | "danger";
  style?: JSX.CSSProperties;
  disabled?: boolean;
}) {
  const cls = () => {
    switch (props.tone ?? "plain") {
      case "quiet":
        return "word word-quiet";
      case "loud":
        return "word word-loud";
      case "danger":
        return "word word-danger";
      default:
        return "word";
    }
  };
  return (
    <button
      type="button"
      class={cls()}
      classList={{ "word-on": props.current }}
      aria-current={props.current ? "true" : undefined}
      title={props.title}
      style={props.style}
      disabled={props.disabled}
      onClick={() => props.onClick?.()}
    >
      {props.children}
    </button>
  );
}

export function Field(props: {
  label?: string;
  from?: string;
  hint?: string;
  children: JSX.Element;
}) {
  return (
    <div class="field">
      <span class="field-label">
        {props.label ?? ""}
        <Show when={props.from}>
          <span class="field-from">{props.from}</span>
        </Show>
      </span>
      <span class="field-body">{props.children}</span>
      <Show when={props.hint}>
        <span class="field-hint">{props.hint}</span>
      </Show>
    </div>
  );
}

/// A row of words, one of which is the current value.
export function Opts<T>(props: {
  opts: readonly T[];
  label: (o: T) => string;
  current: (o: T) => boolean;
  pick: (o: T) => void;
  paint?: (o: T, on: boolean) => JSX.CSSProperties | undefined;
  glyphs?: boolean;
}) {
  return (
    <span class="opts" classList={{ "opts-glyphs": props.glyphs }}>
      <For each={props.opts}>
        {(o) => (
          <Word
            tone="quiet"
            current={props.current(o)}
            onClick={() => props.pick(o)}
            style={props.paint?.(o, props.current(o))}
          >
            {props.label(o)}
          </Word>
        )}
      </For>
    </span>
  );
}

const TICKS = 12;

/// The only continuous control: twelve ticks and the number itself.
export function Meter(props: {
  value: number;
  max: number;
  unit?: string;
  label: string;
  set: (v: number) => void;
}) {
  const step = () => props.max / TICKS;
  const nudge = (dir: number) =>
    props.set(
      Math.max(0, Math.min(props.max, Math.round(props.value + dir * step()))),
    );
  return (
    <span class="opts">
      <span
        class="meter"
        role="slider"
        tabindex="0"
        aria-label={props.label}
        aria-valuenow={props.value}
        aria-valuemin={0}
        aria-valuemax={props.max}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight" || e.key === "ArrowUp") {
            e.preventDefault();
            nudge(1);
          }
          if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
            e.preventDefault();
            nudge(-1);
          }
        }}
      >
        <For each={Array.from({ length: TICKS }, (_, i) => i)}>
          {(i) => (
            <i
              classList={{ on: props.value >= (i + 1) * step() - step() / 2 }}
              style={{ height: `${4 + i * 1.05}px` }}
              onClick={() => props.set(Math.round((i + 1) * step()))}
            />
          )}
        </For>
      </span>
      <span class="meter-val">
        {props.value}
        {props.unit ?? ""}
      </span>
    </span>
  );
}

export function Note(props: { children: JSX.Element }) {
  return <div class="note">{props.children}</div>;
}

/// Something the app needs an answer to, over the input line. Same shape as
/// everything else: a sentence, then the answers as words. Unlike the echo it
/// stays until answered.
export function Ask(props: {
  children: JSX.Element;
  answers: { label: string; onClick: () => void; loud?: boolean }[];
}) {
  return (
    <div class="ask">
      <span class="ask-text">{props.children}</span>
      <span class="ask-answers">
        <For each={props.answers}>
          {(a) => (
            <Word tone={a.loud ? "loud" : "quiet"} onClick={a.onClick}>
              {a.label}
            </Word>
          )}
        </For>
      </span>
    </div>
  );
}

/// Something worth reading in full — a fingerprint, an empty-state sentence.
export function Face(props: { children: JSX.Element }) {
  return <div class="face">{props.children}</div>;
}
