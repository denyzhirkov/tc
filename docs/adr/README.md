# Architecture Decision Records

One file per significant, hard-to-reverse decision: `NNNN-short-title.md`. Smaller truths live as bullets in `docs/state/decisions.md`; promote one to an ADR when it's contested, costly to reverse, or shapes a public contract.

Format: **Context → Decision → Consequences**, plus a status line.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-shared-core-multiple-frontends.md) | Shared `tc-client` core, multiple frontends | Accepted |
| [0002](0002-relay-never-decrypts-voice.md) | Relay never decrypts voice (E2E) | Accepted |
