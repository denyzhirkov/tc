import { JSX, Show, createSignal, onCleanup, onMount } from "solid-js";

/**
 * Minimal click-outside popover. Kept intentionally barebones — no animation,
 * no portal: the popover renders absolute relative to the trigger's parent.
 */
export default function Popover(props: {
  trigger: (open: () => void, isOpen: () => boolean) => JSX.Element;
  children: (close: () => void) => JSX.Element;
  align?: "left" | "right";
}) {
  const [open, setOpen] = createSignal(false);
  let panelRef: HTMLDivElement | undefined;

  const onDocClick = (e: MouseEvent) => {
    if (!open()) return;
    if (panelRef && !panelRef.contains(e.target as Node)) setOpen(false);
  };
  const onEsc = (e: KeyboardEvent) => {
    if (e.key === "Escape") setOpen(false);
  };

  onMount(() => {
    document.addEventListener("mousedown", onDocClick);
    window.addEventListener("keydown", onEsc);
    onCleanup(() => {
      document.removeEventListener("mousedown", onDocClick);
      window.removeEventListener("keydown", onEsc);
    });
  });

  return (
    <span class="relative inline-block">
      {props.trigger(() => setOpen((v) => !v), open)}
      <Show when={open()}>
        <div
          ref={panelRef}
          class={`absolute top-full mt-1 z-20 bg-surface border border-line rounded-lg shadow-lg px-1.5 py-1.5 min-w-48 ${
            props.align === "right" ? "right-0" : "left-0"
          }`}
        >
          {props.children(() => setOpen(false))}
        </div>
      </Show>
    </span>
  );
}
