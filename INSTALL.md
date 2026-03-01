# Installing vclaw

## Pre-built binary (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/MadHouseLabs/vclaw/master/install.sh | sh
```

Downloads the latest release for your platform and installs to `/usr/local/bin` (or `~/.local/bin` if no write access). Supports Linux x86_64, macOS Intel, and macOS Apple Silicon.

Windows users: download `vclaw-x86_64-pc-windows-msvc.exe` from [Releases](https://github.com/MadHouseLabs/vclaw/releases).

## Build from source

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable)
- cmake (required by whisper-rs C++ bindings)
- Platform audio libraries (Linux only)

### macOS

```sh
brew install cmake
cargo build --release
cp target/release/vclaw /usr/local/bin/
```

### Linux (Debian/Ubuntu)

```sh
sudo apt install cmake build-essential libasound2-dev
cargo build --release
sudo cp target/release/vclaw /usr/local/bin/
```

### OAuth client ID

Release binaries have the Anthropic OAuth client ID baked in at compile time. For local builds, OAuth login won't work unless you provide it:

```sh
ANTHROPIC_OAUTH_CLIENT_ID="<id>" cargo build --release
```

Without it, use an API key directly instead:

```sh
export ANTHROPIC_API_KEY="sk-ant-..."
```

## Runtime dependencies

- [tmux](https://github.com/tmux/tmux) — vclaw runs Claude Code inside a tmux session
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) — must be installed and on your `PATH`

## Authentication

Run `vclaw auth` to authenticate via Anthropic OAuth (opens browser), or set the env var:

```sh
export ANTHROPIC_API_KEY="sk-ant-..."
```

For voice features (TTS/STT), optionally set:

```sh
export ELEVENLABS_API_KEY="..."
```

Credentials are stored in `~/.config/vclaw/credentials.toml`.

## Configuration

Optional config at `~/.config/vclaw/config.toml`:

```toml
[voice]
mode = "push_to_talk"       # push_to_talk | always_on
stt_provider = "elevenlabs"  # elevenlabs | whisper
whisper_model = "base"       # tiny | base | small (local STT only)

[tts]
voice_id = "cgSgspJ2msm6clMCkdW9"
model_id = "eleven_turbo_v2"

[brain]
model = "claude-sonnet-4-6"
```

## File locations

| Path | Purpose |
|------|---------|
| `~/.config/vclaw/config.toml` | Configuration |
| `~/.config/vclaw/credentials.toml` | API keys / OAuth tokens |
| `~/.local/share/vclaw/models/` | Cached Whisper models |
| `~/.local/share/vclaw/logs/` | Log files (daily rotation) |

## Verify

```sh
vclaw --help
```
