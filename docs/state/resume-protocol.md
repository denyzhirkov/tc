# Resume & maintenance protocol

## Start of a new session

```sh
cat docs/STATE.md          # phase, recently closed, up-next
tsk list --inprogress      # anything mid-flight
tsk list                   # full pending queue
git log --oneline -20      # recent commits
# deep-dive on demand:
#   cat docs/state/decisions.md      # architectural truths & gotchas
#   cat docs/state/closed-desktop.md # detailed history
```

For code/context retrieval use `kungfu` first (see `~/.claude/CLAUDE.md`), `Read`/`grep` as fallback.

## After closing a non-trivial story

1. `tsk done <id>` (or `tsk update` if scope shifted).
2. `docs/STATE.md` → move the story into "Recently closed" (cap ~5; older entries spill into the matching `closed-*.md`), refresh "Up next" and "In progress", bump the date.
3. Append a full paragraph to the matching `docs/state/closed-*.md` (`closed-cli-core.md` for relay/TUI/protocol work, `closed-desktop.md` for the Tauri app).
4. New architectural truth or gotcha → bullet in `docs/state/decisions.md`. A big/contested decision → a dedicated ADR in `docs/adr/`.
5. Protocol changed → bump `PROTOCOL_VERSION`, update `PROTOCOL.md`. Release → bump version in all three files (root `Cargo.toml`, desktop `package.json`, `tauri.conf.json`) and add a `CHANGELOG.md` entry.

`docs/STATE.md` may go at most one closed story stale. If you sit down and find it stale, fix it first.
