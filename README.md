# vclaw

Voice-controlled terminal assistant. Talk to your terminal, and vclaw translates your voice into prompts for [Claude Code](https://docs.anthropic.com/en/docs/claude-code) running inside tmux.

```
Mic -> STT -> Claude API -> tmux (Claude Code) -> TTS -> Speaker
```

vclaw listens to your voice, enriches your commands with project context, types them into Claude Code, monitors the output, handles permission prompts, and speaks results back to you.

## Features

- **Voice input** via ElevenLabs realtime STT (streaming) or local Whisper (offline)
- **Voice output** via ElevenLabs TTS with streaming playback and interrupt support
- **Claude-powered prompt enrichment** — brief voice commands become detailed, context-aware prompts
- **Automatic permission handling** — approves Claude Code's tool-use prompts with spoken confirmation
- **tmux status bar** with live voice status indicator (waveform animation while listening)
- **IPC control** — mute, interrupt, toggle voice, view conversation from any terminal
- **Key bindings** — F12 push-to-talk / interrupt, Alt-M mute, Prefix-C conversation popup
- **Per-project sessions** — each directory gets its own tmux session and daemon
- **Graceful degradation** — works without voice (text-only), without TTS, or without mic

## Requirements

- macOS (uses CoreAudio via cpal)
- [tmux](https://github.com/tmux/tmux)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI installed
- Rust toolchain (for building from source)

## Installation

```sh
cargo install --path .
```

Or build directly:

```sh
cargo build --release
# Binary at target/release/vclaw
```

### Whisper model (local STT only)

If using the Whisper STT provider, vclaw automatically downloads the model on first run. Models are cached in `~/.local/share/vclaw/models/`.

## Authentication

vclaw needs an Anthropic API key (for the brain) and optionally an ElevenLabs API key (for voice).

**Environment variables** (simplest):
```sh
export ANTHROPIC_API_KEY="sk-..."
export ELEVENLABS_API_KEY="..."
```

**OAuth** (interactive):
```sh
vclaw auth          # Opens browser for Anthropic OAuth
```

**Direct API key**:
```sh
vclaw auth --api-key sk-ant-...
```

On first run without credentials, vclaw prompts for authentication interactively. ElevenLabs key is optional — without it, vclaw runs in text-only mode (no voice output).

Credentials are stored in `~/.config/vclaw/credentials.toml`.

## Usage

```sh
vclaw                          # Start in current directory
vclaw --voice-mode ptk         # Push-to-talk mode (F12 to record)
vclaw --voice-mode always_on   # Always listening (default with ElevenLabs STT)
vclaw --whisper base           # Use local Whisper model (tiny/base/small)
vclaw --session my-project     # Custom session name
vclaw attach                   # Reattach to existing session
vclaw ctl status               # Query daemon status
vclaw ctl mute                 # Toggle mute
vclaw ctl interrupt            # Interrupt current action
vclaw ctl conversation         # View conversation history
```

### Key bindings (inside tmux)

| Key | Action |
|-----|--------|
| `F12` | Toggle push-to-talk / interrupt if speaking |
| `Alt-M` | Toggle mute |
| `Prefix + Space` | Toggle voice |
| `Prefix + Escape` | Interrupt |
| `Prefix + C` | View conversation in popup |

### How it works

1. vclaw starts a tmux session running Claude Code in the current directory
2. You attach to the tmux session and see Claude Code's terminal
3. vclaw runs as a background daemon — listening to your mic, watching Claude Code's output
4. When you speak, vclaw transcribes your voice, enriches it with project context, and types a well-formed prompt into Claude Code
5. vclaw monitors Claude Code's JSONL transcript for permission prompts, completion, and errors
6. Results are spoken back via TTS

## Configuration

Optional config file at `~/.config/vclaw/config.toml`:

```toml
[voice]
mode = "push_to_talk"      # push_to_talk | always_on
whisper_model = "base"      # tiny | base | small (local STT only)
stt_provider = "elevenlabs" # elevenlabs | whisper

[tts]
voice_id = "cgSgspJ2msm6clMCkdW9"
model_id = "eleven_turbo_v2"

[brain]
model = "claude-sonnet-4-6"
complex_model = "claude-sonnet-4-6"
max_context_lines = 50
```

## Architecture

Monolithic Rust binary with async event bus (tokio broadcast channels):

- **Voice Engine** — audio capture (cpal), STT (ElevenLabs WebSocket or local Whisper), VAD
- **Brain** — Claude API client with streaming, tool definitions, prompt caching, conversation management
- **Tmux Controller** — session management, pane capture, key sending, status bar
- **TTS** — ElevenLabs streaming text-to-speech
- **Audio Player** — rodio MP3 playback with interrupt support
- **IPC** — Unix socket server for `vclaw ctl` commands
- **Status Bar** — tmux status-right integration with voice state visualization

## License

MIT
