//! vclaw — voice-controlled terminal assistant.
//!
//! vclaw wraps Claude Code in a tmux session, adds voice input/output, and
//! orchestrates the interaction through a Claude-powered "brain" that enriches
//! voice commands into detailed prompts.
//!
//! # Modules
//!
//! - [`voice`] — Audio capture and speech-to-text (ElevenLabs realtime or local Whisper)
//! - [`brain`] — Claude API client, JSONL transcript monitoring, prompt construction
//! - [`tmux`] — tmux session management, key sending, pane capture
//! - [`tts`] — ElevenLabs streaming text-to-speech client
//! - [`audio`] — MP3 playback with interrupt support (rodio)
//! - [`status`] — Push-based tmux status bar with voice state indicator
//! - [`ipc`] — Unix socket IPC server for `vclaw ctl` commands
//! - [`auth`] — OAuth PKCE flow and API key storage
//! - [`config`] — TOML config loading and CLI argument parsing
//! - [`event`] — Event types and broadcast bus connecting all components

pub mod auth;
pub mod audio;
pub mod config;
pub mod event;
pub mod ipc;
pub mod tmux;
pub mod brain;
pub mod tts;
pub mod status;
pub mod voice;
