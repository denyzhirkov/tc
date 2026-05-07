// Tiny global signal for the ⌘K palette. The palette UI subscribes to it.

import { createSignal } from "solid-js";

const [open, setOpen] = createSignal<{ initial?: string } | null>(null);

export const paletteOpen = open;

export function openPalette(initial?: string) {
  setOpen({ initial: initial ?? "" });
}

export function closePalette() {
  setOpen(null);
}
