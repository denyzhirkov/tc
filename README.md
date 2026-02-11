# tc — terminal voice chat

Minimal voice + text chat that runs in the terminal. Built with Rust, Opus codec, low latency relay server. No accounts, no UI bloat — just open a channel and talk.

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
tc    # start client
```

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

## Build from source

```sh
cargo build --release
```

Requires Rust toolchain. On Linux: `sudo apt-get install libasound2-dev`.
