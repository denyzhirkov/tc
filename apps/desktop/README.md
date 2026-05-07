# tc_ desktop

Tauri 2 + SolidJS + TypeScript + Tailwind UI client.

## Layout

- `src/` — Solid frontend (Vite).
- `src-tauri/` — Tauri Rust backend (`tc-desktop` crate, in workspace).

## Dev

```sh
cd apps/desktop
pnpm install
pnpm tauri dev
```

`pnpm tauri dev` runs `vite` (frontend) and `cargo run` (Tauri shell) together,
hot-reloading both sides. Frontend dev server: `http://localhost:1420`.

## Build

```sh
pnpm tauri build
```

Output binaries land under `src-tauri/target/release/bundle/`.

## State of the world

Scaffold only. The single command wired up is `app_status` — it returns version
and identity fingerprint. Voice/network/channel commands come in subsequent
phases (see roadmap in repo root).
