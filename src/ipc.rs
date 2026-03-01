//! Unix socket IPC server for `vclaw ctl` commands.
//!
//! The daemon listens on a per-session socket at
//! `~/.local/share/vclaw/<session>.sock`. The `vclaw ctl` thin client
//! connects, sends a JSON command, and receives a JSON response.
//! Protocol is line-delimited JSON (one JSON object per line).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, RwLock};

use crate::event::{Event, VoiceStatus};

/// Commands accepted over the IPC socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcCommand {
    Mute,
    Interrupt,
    VoiceToggle,
    Status,
    Conversation,
    Quit,
}

/// Response sent back to the `vclaw ctl` client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Payload for the `status` command response.
#[derive(Debug, Clone, Serialize)]
pub struct StatusData {
    pub voice_status: String,
    pub muted: bool,
}

/// Shared daemon state, read by IPC handlers and written by the daemon loop.
pub struct SharedState {
    pub voice_status: VoiceStatus,
    pub muted: bool,
    pub conversation: Vec<(String, String)>,
    pub live_transcript: String,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            voice_status: VoiceStatus::Idle,
            muted: false,
            conversation: Vec::new(),
            live_transcript: String::new(),
        }
    }
}

/// Compute the IPC socket path for a given session name.
pub fn socket_path(session_name: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vclaw")
        .join(format!("{}.sock", session_name))
}

/// Start the IPC server. Runs forever, accepting connections and dispatching commands.
pub async fn start_server(
    state: Arc<RwLock<SharedState>>,
    event_tx: broadcast::Sender<Event>,
    session_name: &str,
) -> Result<()> {
    let path = socket_path(session_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove stale socket
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    tracing::info!("IPC server listening on {:?}", path);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, state, event_tx).await {
                        tracing::debug!("IPC client error: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::error!("IPC accept error: {}", e);
            }
        }
    }
}

async fn handle_client(
    stream: UnixStream,
    state: Arc<RwLock<SharedState>>,
    event_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let response = match serde_json::from_str::<IpcCommand>(line.trim()) {
            Ok(cmd) => process_command(cmd, &state, &event_tx).await,
            Err(e) => IpcResponse {
                ok: false,
                data: None,
                error: Some(format!("parse error: {}", e)),
            },
        };

        let mut resp_json = serde_json::to_string(&response)?;
        resp_json.push('\n');
        writer.write_all(resp_json.as_bytes()).await?;
        line.clear();
    }

    Ok(())
}

async fn process_command(
    cmd: IpcCommand,
    state: &Arc<RwLock<SharedState>>,
    event_tx: &broadcast::Sender<Event>,
) -> IpcResponse {
    match cmd {
        IpcCommand::Mute => {
            let _ = event_tx.send(Event::MuteToggle);
            IpcResponse { ok: true, data: None, error: None }
        }
        IpcCommand::Interrupt => {
            let _ = event_tx.send(Event::Interrupt);
            IpcResponse { ok: true, data: None, error: None }
        }
        IpcCommand::VoiceToggle => {
            let _ = event_tx.send(Event::VoiceToggle);
            IpcResponse { ok: true, data: None, error: None }
        }
        IpcCommand::Quit => {
            let _ = event_tx.send(Event::Quit);
            IpcResponse { ok: true, data: None, error: None }
        }
        IpcCommand::Status => {
            let s = state.read().await;
            let status_str = match &s.voice_status {
                VoiceStatus::Idle => "idle",
                VoiceStatus::Listening => "listening",
                VoiceStatus::Thinking => "thinking",
                VoiceStatus::Speaking => "speaking",
            };
            let data = StatusData {
                voice_status: status_str.to_string(),
                muted: s.muted,
            };
            IpcResponse {
                ok: true,
                data: Some(serde_json::to_value(data).unwrap()),
                error: None,
            }
        }
        IpcCommand::Conversation => {
            let s = state.read().await;
            let data: Vec<serde_json::Value> = s.conversation.iter().map(|(role, text)| {
                serde_json::json!({"role": role, "text": text})
            }).collect();
            IpcResponse {
                ok: true,
                data: Some(serde_json::Value::Array(data)),
                error: None,
            }
        }
    }
}

/// Send a command to the daemon over the Unix socket and return the response.
pub async fn send_command(cmd: &str, session_name: &str) -> Result<IpcResponse> {
    let path = socket_path(session_name);
    let stream = UnixStream::connect(&path).await
        .map_err(|_| anyhow::anyhow!("Cannot connect to vclaw daemon. Is it running?"))?;

    let (reader, mut writer) = stream.into_split();

    let ipc_cmd = match cmd {
        "mute" => IpcCommand::Mute,
        "interrupt" => IpcCommand::Interrupt,
        "voice_toggle" => IpcCommand::VoiceToggle,
        "status" => IpcCommand::Status,
        "conversation" => IpcCommand::Conversation,
        "quit" => IpcCommand::Quit,
        other => return Err(anyhow::anyhow!("Unknown command: {}", other)),
    };

    let mut json = serde_json::to_string(&ipc_cmd)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.shutdown().await?;

    let mut reader = BufReader::new(reader);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await?;

    let response: IpcResponse = serde_json::from_str(response_line.trim())?;
    Ok(response)
}

/// Format conversation data for terminal display with ANSI colors.
pub fn format_conversation(data: &serde_json::Value) -> String {
    let mut output = String::new();
    if let Some(entries) = data.as_array() {
        for entry in entries {
            let role = entry["role"].as_str().unwrap_or("?");
            let text = entry["text"].as_str().unwrap_or("");
            if role == "You" {
                // Cyan for user
                output.push_str(&format!("\x1b[1;36m{}: \x1b[0;36m{}\x1b[0m\n\n", role, text));
            } else {
                // White/bold for vclaw
                output.push_str(&format!("\x1b[1;37m{}: \x1b[0;37m{}\x1b[0m\n\n", role, text));
            }
        }
    }
    output
}
