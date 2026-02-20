# tc — terminal voice chat

Minimal voice + text chat that runs in the terminal. Built with Rust, Opus codec, low latency relay server. No accounts, no UI bloat — just open a channel and talk.

## Features

- **Voice chat** — Opus-encoded, encrypted (XChaCha20-Poly1305), adaptive quality
- **Text chat** — send messages to your channel
- **Terminal UI** — ratatui-based TUI with status bar, autocomplete, command history
- **Web UI** — embedded browser control panel at `http://127.0.0.1:17300` (audio stays in terminal)
- **TLS + TOFU** — trust-on-first-use certificate pinning
- **Auto-reconnect** — exponential backoff, automatic channel rejoin
- **VAD** — voice activity detection with configurable threshold
- **Version exchange** — client and server compare versions on connect, suggest updates on mismatch

## Install

**macOS / Linux:**

```sh
curl -fsSL https://raw.githubusercontent.com/denyzhirkov/tc/master/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/denyzhirkov/tc/master/install.ps1 | iex
```

## Usage

```
tc    # start client (web UI on http://127.0.0.1:17300)
```

### CLI flags

| Flag | Description |
|------|-------------|
| `--web-port <PORT>` | Web UI port (default: 17300) |
| `--no-web` | Disable the web UI |

### Commands

| Command | Description |
|---------|-------------|
| `/server [ip\|ip:port]` | Set or show server address |
| `/reconnect` | Reconnect to server |
| `/create` | Create a new channel |
| `/connect <id>` | Join a channel (alias: `/join`) |
| `/leave` | Leave current channel |
| `/name <name>` | Set your display name |
| `/mute` | Toggle mute (alias: `/m`) |
| `/config` | Show audio config |
| `/config input [N]` | Select input device |
| `/config output [N]` | Select output device |
| `/config vad [0-100\|off]` | Set voice activity detection level |
| `/web <port>` | Start web UI on port (if not started) |
| `/help` | Show all commands |
| `/quit` | Exit (alias: `/exit`) |

Any text without `/` prefix is sent as a chat message.

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+C` / `Esc` | Quit |
| `Ctrl+M` | Toggle mute |
| `Tab` | Autocomplete command |
| `Up` / `Down` | Command history |
| `PageUp` / `PageDown` | Scroll messages |

## Web UI

The client embeds a local HTTP + WebSocket server. Audio capture/playback stays in the terminal — the browser provides UI only (commands, chat, status).

```
Browser (UI) <--WS JSON--> tc-client (localhost) <--TLS/UDP--> tc-server
```

- Opens automatically at `http://127.0.0.1:17300`
- Status bar: connection, channel, voice/mute, quality, speakers
- Quick buttons: create, join, leave, mute, reconnect, help
- Text input with command history
- Multiple tabs supported (broadcast channel)
- Disable with `--no-web`, change port with `--web-port <PORT>`

## Architecture

```
tc/
  crates/
    shared/    # protocol, config constants, framing
    server/    # TLS+TCP control, UDP voice relay, rate limiting
    client/    # TUI, audio, voice pipeline, web UI
```

### Protocol

- **TCP control**: length-prefixed bincode frames over TLS
- **UDP voice**: Opus packets encrypted with XChaCha20-Poly1305
- **Version exchange**: client sends `Hello { version, protocol }`, server sends `Welcome { version, protocol }` immediately after TLS handshake. Protocol version is bumped on breaking changes; cargo package version tracks releases.

### Versioning

Both client and server embed their version (`CARGO_PKG_VERSION`) and protocol version (`PROTOCOL_VERSION` in `config.rs`). On connect:

- **Protocol mismatch** — hard warning, client should update
- **Version mismatch** (same protocol) — soft note, consider updating

Bump `PROTOCOL_VERSION` when changing the wire format. Bump workspace `version` in `Cargo.toml` for any release.

## Build from source

```sh
cargo build --release
```

Requires Rust toolchain. On Linux: `sudo apt-get install libasound2-dev`.

## Self-host

```sh
tc-server                        # start server (TLS cert auto-generated)
tc --no-web                      # connect from terminal only
tc --web-port 8080               # connect with web UI on custom port
```

## License

MIT
