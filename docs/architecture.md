# vclaw Architecture

## Overview

vclaw is a monolithic Rust binary that runs as a background daemon alongside a tmux session. It connects a voice interface to Claude Code by:

1. Capturing audio and transcribing speech (STT)
2. Enriching voice commands into detailed prompts via Claude API
3. Typing those prompts into Claude Code's tmux pane
4. Monitoring Claude Code's JSONL transcript for state changes
5. Speaking results back via TTS

All communication between components flows through a tokio broadcast channel (the "event bus").

## System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      vclaw process                           │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │                   Event Bus (broadcast)               │   │
│  │  Events: UserSaid, VoiceStatus, MuteToggle,          │   │
│  │          Interrupt, ConversationEntry, Quit           │   │
│  └───┬──────────┬──────────┬──────────┬──────────┬──────┘   │
│      │          │          │          │          │           │
│      v          v          v          v          v           │
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐   │
│  │ Voice  │ │ Daemon │ │ Status │ │  IPC   │ │  tmux  │   │
│  │ Task   │ │  Loop  │ │  Bar   │ │ Server │ │ attach │   │
│  └───┬────┘ └───┬────┘ └───┬────┘ └────────┘ └────────┘   │
│      │          │          │                                 │
│      │          │          └─── tmux set-option + refresh    │
│      │          │                                            │
│      │          ├─── Brain (Claude API, streaming SSE)       │
│      │          ├─── TTS (ElevenLabs streaming)              │
│      │          ├─── Audio Player (rodio, blocking thread)   │
│      │          └─── JSONL transcript polling (1s interval)  │
│      │                                                       │
│      ├─── Audio capture (cpal, dedicated OS thread)          │
│      └─── STT (ElevenLabs WebSocket or local Whisper)        │
└─────────────────────────────────────────────────────────────┘
         │                    │
         v                    v
   ┌──────────┐       ┌─────────────┐
   │  Mic     │       │  tmux       │
   │ (cpal)   │       │ session     │
   └──────────┘       │ ┌─────────┐ │
                      │ │ Claude  │ │
                      │ │  Code   │ │
                      │ └─────────┘ │
                      └─────────────┘
```

## Task Structure

vclaw spawns several concurrent tasks from `main()`:

| Task | Description | Lifetime |
|------|-------------|----------|
| `voice_task` | Audio capture + STT management | Full session |
| `status_bar_task` | Event-driven tmux status bar updates | Full session |
| `ipc::start_server` | Unix socket IPC for `vclaw ctl` | Full session |
| `run_daemon_loop` | Brain orchestration, JSONL polling, tool execution | Full session |
| `tmux attach` | Child process — user's terminal attachment | Until detach |

Shutdown is triggered by any of: tmux detach (child exits), `Quit` event (from IPC), or `SIGINT`/`SIGTERM`.

## Event Flow

### Voice Command Flow

```
User speaks
  → cpal audio callback captures PCM frames
  → STT produces text (WebSocket or batch)
  → Event::UserSaid("run the tests")
  → daemon_loop receives event
  → Brain enriches + streams response
  → Tool calls: shell_input (types into Claude Code) + speak (TTS)
  → Event::VoiceStatus(Speaking) → status bar updates
  → Audio playback completes
  → Event::VoiceStatus(Idle)
```

### Permission Handling Flow

```
Claude Code waits for permission (tool use)
  → JSONL transcript shows WaitingForPermission
  → Poll 1: enters Debouncing state (avoids false positives)
  → Poll 2: still WaitingForPermission → confirmed
  → Brain generates spoken confirmation + types "y"
  → Enters Handled state (prevents re-sending "y")
  → Claude Code processes approval → state changes to Working
  → PermissionState resets to None
```

The permission state machine (`PermissionState` enum in `main.rs`) prevents a race condition where the brain sends "y" but Claude Code's JSONL hasn't updated yet, causing repeated approvals.

## Audio Pipeline

### ElevenLabs Realtime (streaming STT)

```
Mic → cpal callback → resample to 16kHz mono → PCM→i16 bytes
  → mpsc channel → WebSocket sender task → base64 encode
  → ElevenLabs realtime API → WebSocket receiver task
  → committed_transcript → Event::UserSaid
```

The WebSocket connection auto-reconnects on disconnect with a 1s backoff (5s on retry failure).

### Local Whisper (batch STT)

```
Mic → cpal callback → resample to 16kHz mono → buffer (Vec<f32>)
  → VAD detects silence → speech_done signal
  → whisper-rs transcription (blocking) → Event::UserSaid
```

VAD uses RMS energy thresholding: `ENERGY_THRESHOLD = 0.03`, requiring `MIN_SPEECH_FRAMES = 5` frames above threshold to start recording and `SILENCE_FRAMES_REQUIRED = 120` frames below to stop.

### Push-to-Talk (F12)

```
F12 press → Event::VoiceToggle → start recording (buffer audio)
F12 press → Event::VoiceToggle → stop recording → batch transcribe
```

In PTT mode, F12 while speaking/thinking sends `Event::Interrupt` instead.

### TTS Playback

```
Brain tool call: speak("Done!")
  → ElevenLabs streaming TTS API → collect MP3 bytes
  → spawn_blocking → rodio Sink playback
  → interrupt() stops Sink directly via Arc<Sink>
  → Event::VoiceStatus(Idle) on completion
```

## Brain & Claude API

The brain (`brain.rs`) manages a conversation with Claude's Messages API:

- **System prompt**: static persona + CLAUDE.md project context + Claude Code history
- **Tools**: `shell_input` (type into tmux) and `speak` (TTS output)
- **Streaming**: SSE via `reqwest-eventsource`, parsed into `StreamEvent` enum
- **Prompt caching**: ephemeral cache_control on tools and system prompt
- **History compaction**: rolling window of 10 messages, cut at safe user-message boundaries
- **Complexity routing**: simple commands → default model, complex queries → complex_model

### JSONL Transcript Monitoring

The daemon polls Claude Code's `.claude/projects/` JSONL files at 1-second intervals:

1. `find_latest_jsonl()` — finds the most recent transcript file
2. `poll_claude_code_history()` — reads new bytes since last offset
3. `parse_jsonl_entries()` — extracts conversation entries and detects state
4. State detection: `WaitingForPermission` / `Idle` / `Working` / `Unknown`

Idle detection is debounced: waits 3 seconds of no new JSONL entries before acting, to batch rapid-fire tool results into a single brain update.

## Status Bar

The status bar (`status.rs`) uses a push-based model:

1. `status_bar_task` receives `VoiceStatus` and `MuteToggle` events
2. Renders state to a tmux format string (e.g., `#[fg=colour114,bold]◆ listening`)
3. Content-change guard skips tmux calls when the string hasn't changed
4. `push_to_tmux()` runs `tmux set-option status-right` + `refresh-client -S`
5. Client name is lazily resolved (StatusBar is created before tmux attach)

States: `◇ starting` → `◇ ready` / `◇ F12 to talk` → `◆ listening` → `◆ thinking` → `◆ speaking` → `● muted`

## IPC

Unix domain socket at `~/.local/share/vclaw/<session>.sock`. Line-delimited JSON protocol:

```
→ {"cmd":"status"}
← {"ok":true,"data":{"voice_status":"idle","muted":false}}

→ {"cmd":"mute"}
← {"ok":true}
```

Commands: `mute`, `interrupt`, `voice_toggle`, `status`, `conversation`, `quit`.

## Key Bindings

Configured in `tmux.rs::configure_session()` using `#{session_name}` so each vclaw instance's bindings target the correct daemon:

```
F12        → vclaw --session #{session_name} ctl voice_toggle
Alt-M      → vclaw --session #{session_name} ctl mute
Prefix+Esc → vclaw --session #{session_name} ctl interrupt
Prefix+Spc → vclaw --session #{session_name} ctl voice_toggle
Prefix+C   → display-popup: vclaw --session #{session_name} ctl conversation
```

## Noise Filtering

`is_noise()` in `main.rs` filters STT hallucinations before they reach the brain:

- Whisper artifacts (`[BLANK_AUDIO]`, `(blank audio)`)
- Bracketed descriptions (`[music]`, `(typing sounds)`)
- Too-short utterances (< 2 alphabetic characters)
- Common filler words when alone (`um`, `uh`, `hmm`)
- STT hallucinations (`thank you`, `please subscribe`, `foreign`)
- Keyboard noise (very short, <= 4 chars and <= 2 words)

## Error Recovery

- **Brain API failure**: clears message history on first failure, stops tool loop on follow-up failure
- **WebSocket disconnect**: auto-reconnects with 1s then 5s backoff
- **Audio capture failure**: reports via ConversationEntry, continues without voice
- **TTS failure**: logs error, emits Idle status, continues without audio
- **Empty brain response**: clears history to recover from 400 errors
