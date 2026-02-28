# vclaw Design Document

A Rust CLI/TUI application for hands-free coding via voice-controlled tmux management.

## Overview

vclaw wraps tmux sessions in a ratatui-based TUI, listens to voice input (local Whisper STT), speaks back (ElevenLabs TTS), and uses Claude API to interpret natural language into unrestricted tmux actions. It captures pane content to give the LLM context about what's on screen.

**Primary use case:** Hands-free coding — controlling terminal sessions by voice while hands are occupied.

## Architecture

Monolithic Rust binary. Four core modules connected by an async event bus (tokio mpsc channels):

```
┌─────────────────────────────────────────────────┐
│                   vclaw binary                   │
│                                                  │
│  ┌──────────┐  ┌───────────┐  ┌──────────────┐  │
│  │  TUI     │  │  Voice    │  │  Tmux        │  │
│  │ (ratatui)│  │  Engine   │  │  Controller  │  │
│  └────┬─────┘  └─────┬─────┘  └──────┬───────┘  │
│       └──────────┬───┴───────────────┘           │
│           ┌──────▼──────┐                        │
│           │  Brain      │                        │
│           │ (Claude API)│                        │
│           └─────────────┘                        │
│         ┌────────────────────┐                   │
│         │  Event Bus (mpsc)  │                   │
│         └────────────────────┘                   │
└─────────────────────────────────────────────────┘
```

**Event flow:** Voice Engine emits `UserSaid("...")` -> Brain receives it + pane context -> Brain returns actions -> Tmux Controller executes -> TUI updates.

## TUI Layout

```
┌─────────────────────────────────────────────────────────┐
│  vclaw                                    🎤 Listening  │
├────────────────────────────────┬────────────────────────┤
│                                │    Pane List           │
│      Active Pane Preview       │    [1] ~/dev/vclaw     │
│      (read-only mirror of      │    [2] cargo build     │
│       tmux pane content)       │    [3] git log         │
│                                │────────────────────────│
│                                │    Conversation        │
│                                │    You: "run tests"    │
│                                │    vclaw: Running...   │
├────────────────────────────────┴────────────────────────┤
│  > voice transcript appears here as you speak...        │
└─────────────────────────────────────────────────────────┘
```

- **Top bar:** app name + voice status (Idle / Listening / Thinking / Speaking)
- **Main area (left):** mirrors active tmux pane via `tmux capture-pane` (~500ms poll)
- **Pane list (right top):** all panes with labels, highlights active
- **Conversation log (right bottom):** transcribed speech and vclaw's responses
- **Bottom bar:** live transcript of current speech

**Keyboard fallback:** `q`/`Ctrl+C` quit, `Tab` cycle pane focus, `Enter` toggle push-to-talk, `Esc` cancel voice input / interrupt TTS playback.

## Voice Pipeline

```
Mic -> cpal -> VAD -> whisper-rs (local STT) -> Brain -> ElevenLabs API (TTS) -> cpal playback
```

### STT (Local)
- **Library:** whisper-rs (Whisper C bindings)
- **Models:** tiny (75MB/~1s), base (142MB/~2s, default), small (466MB/~4s)
- **VAD:** silero-vad or webrtc-vad for speech start/end detection

### TTS (Cloud)
- **Provider:** ElevenLabs streaming TTS API
- **Auth:** `ELEVENLABS_API_KEY` environment variable
- **Streaming:** audio chunks played as they arrive via cpal for low latency

### Voice Activation Modes
- **Always-on (default):** VAD detects speech, buffers until silence, transcribes
- **Push-to-talk:** `Enter` key to start/stop recording

### Interruption
Both voice (VAD detects user speaking) and keyboard (`Esc`) stop TTS playback immediately and return to listening state.

## Tmux Controller

Unrestricted, Claude-driven tmux control. The controller is a thin executor, not a fixed command set.

- **State Poller:** polls `tmux list-panes` and `tmux capture-pane` at ~500ms intervals
- **Executor:** runs any tmux command Claude requests against the vclaw tmux server
- **Scope:** limited to the vclaw tmux server session only

Claude receives full tmux state as context and decides what commands to run. No artificial restrictions on available tmux operations.

## Brain (Claude API)

### Tools

| Tool | Purpose |
|------|---------|
| `tmux_execute(command)` | Run any tmux command, returns stdout/stderr |
| `shell_input(pane, text)` | Send keystrokes to a pane (handles escaping, Enter, Ctrl-C, etc.) |
| `read_pane(pane, lines)` | Capture N lines from a pane |
| `speak(message)` | Text to speak aloud via ElevenLabs |

### Context Per Request

```json
{
  "panes": [
    {"id": "%0", "title": "~/dev/vclaw", "size": "80x24", "active": true}
  ],
  "active_pane_content": "last 50 lines...",
  "user_said": "run the tests and tell me if anything fails"
}
```

Tmux state is injected into the latest user message (not system prompt) to avoid invalidating the system prompt cache.

### Model Selection
- Default: Claude Haiku 4.5 for speed on simple commands
- Configurable: Claude Sonnet 4.6 for complex reasoning tasks

### Prompt Caching Strategy

Three-tier caching to minimize costs in long-running voice sessions:

1. **Explicit breakpoint on tools** — tool definitions rarely change, always cached
2. **Explicit breakpoint on system prompt** — large static prompt, always cached
3. **Top-level `cache_control`** — automatically caches growing message history

```
Request structure:

  tools: [tmux_execute, shell_input, read_pane, speak]
    cache_control: {type: "ephemeral"}        ← BREAKPOINT 1

  system: [vclaw persona + tmux reference]
    cache_control: {type: "ephemeral"}        ← BREAKPOINT 2

  messages: [...growing conversation...]
    cache_control: {type: "ephemeral"}        ← TOP-LEVEL AUTO
```

**Critical:** append `response.content` (full object, not just text) to preserve compaction blocks.

### Compaction (Long Sessions)

Uses the `compact-2026-01-12` beta for sessions approaching 200K context. The API automatically summarizes older context server-side. Conversation history is preserved across compaction by storing full response content objects.

### Conversation Memory

Rolling window of messages. History saved to `~/.local/share/vclaw/history.json` periodically for crash recovery.

## Configuration

```toml
# ~/.config/vclaw/config.toml

[voice]
mode = "always_on"           # always_on | push_to_talk
whisper_model = "base"       # tiny | base | small
interrupt_key = "Escape"

[tts]
provider = "elevenlabs"
voice_id = "JBFqnCBsd6RMkjVDRZzb"
model_id = "eleven_turbo_v2"

[brain]
model = "claude-haiku-4-5"
complex_model = "claude-sonnet-4-6"
max_context_lines = 50

[tmux]
default_shell = "/bin/zsh"
poll_interval_ms = 500
```

**Required environment variables:**
- `ELEVENLABS_API_KEY`
- `ANTHROPIC_API_KEY`

## CLI Interface

```
vclaw                  # launch with defaults
vclaw --config <path>  # custom config
vclaw --whisper small  # override whisper model
vclaw --voice-mode ptk # push-to-talk
vclaw attach           # reattach to existing session
```

## Startup Sequence

1. Load config (`~/.config/vclaw/config.toml`)
2. Validate API keys present
3. Load Whisper model into memory
4. Start tmux server (`tmux new-session -d -s vclaw`)
5. Start audio capture (cpal)
6. Start TUI render loop
7. Speak "vclaw ready" -> listening

## Error Handling & Resilience

### Graceful Degradation

```
Full mode:  Voice in → Claude → tmux actions → Voice out
Degraded 1: Voice in → Claude → tmux actions → Text out (TTS down)
Degraded 2: Text in  → Claude → tmux actions → Text out (mic down)
Degraded 3: Text in  → tmux direct commands   (Claude API down)
```

### Failure Responses

| Failure | Behavior |
|---------|----------|
| STT fails | Speak "I didn't catch that, could you repeat?" |
| ElevenLabs unreachable | Show text response in TUI, no speech |
| Claude API unreachable | Queue command, retry with backoff (3 attempts) |
| Mic not available | Start in keyboard-only mode |
| tmux command fails | Return stderr to Claude as tool result |

### Session Persistence

- tmux server survives vclaw crash (`tmux ls` shows session)
- `vclaw attach` reconnects to existing session
- Conversation history saved periodically for crash recovery

## Key Rust Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | tokio |
| TUI | ratatui, crossterm |
| Audio capture/playback | cpal |
| STT | whisper-rs |
| HTTP client (Claude, ElevenLabs) | reqwest |
| JSON | serde, serde_json |
| Config | toml, dirs |
| CLI args | clap |
| tmux subprocess | tokio::process::Command |
