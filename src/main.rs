mod auth;
mod audio;
mod config;
mod event;
mod ipc;
mod tmux;
mod brain;
mod tts;
mod status;
mod voice;

use anyhow::Result;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

use crate::config::{CliCommand, Config, VoiceMode};
use crate::event::{Event, VoiceStatus};
use crate::ipc::SharedState;
use crate::voice::VoiceEngine;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Derive a tmux-safe session name from the current working directory.
/// e.g. /Users/karthik/dev/vclaw -> "vclaw-vclaw"
///      /Users/karthik/dev/my-app -> "vclaw-my-app"
fn session_name_for_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let dir_name = cwd.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    // tmux doesn't allow dots or colons in session names
    let sanitized: String = dir_name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("vclaw-{}", sanitized)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Log to file so it doesn't pollute the terminal.
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("vclaw");
    std::fs::create_dir_all(&log_dir).ok();
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("vclaw.log"))
        .expect("Failed to open log file");

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("vclaw=debug,warn")
        });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(log_file)
        .with_ansi(false)
        .init();
    tracing_log::LogTracer::init().ok();

    let (config, cli) = Config::load_with_cli()?;

    // Resolve session name: --session flag > derive from cwd
    let effective_session = cli.session.clone().unwrap_or_else(session_name_for_cwd);

    // Handle subcommands
    if let Some(CliCommand::Auth { api_key }) = cli.command {
        if let Some(key) = api_key {
            auth::store_api_key(&key)?;
            println!("API key stored successfully.");
        } else {
            let _verifier = auth::start_oauth()?;
            eprint!("Paste the code from the browser: ");
            let mut code = String::new();
            std::io::stdin().read_line(&mut code)?;
            let code = code.trim();
            if code.is_empty() {
                anyhow::bail!("No code provided");
            }
            auth::complete_oauth(code).await?;
            println!("Authenticated successfully.");
        }
        return Ok(());
    }

    if let Some(CliCommand::Attach) = cli.command {
        let ctrl = tmux::TmuxController::new(&effective_session);
        if ctrl.session_exists().await {
            #[cfg(unix)]
            {
                let err = std::process::Command::new("tmux")
                    .args(["attach-session", "-t", &effective_session])
                    .exec();
                // exec() only returns on error
                return Err(anyhow::anyhow!("Failed to attach: {}", err));
            }
            #[cfg(not(unix))]
            {
                eprintln!("Attach not supported on this platform");
                std::process::exit(1);
            }
        } else {
            eprintln!("No vclaw session found for this directory. Run 'vclaw' to start one.");
            std::process::exit(1);
        }
    }

    // Handle ctl subcommand — thin client, talks to daemon over socket
    if let Some(CliCommand::Ctl { command }) = cli.command {
        let response = ipc::send_command(&command, &effective_session).await?;
        if command == "conversation" {
            if let Some(data) = &response.data {
                print!("{}", ipc::format_conversation(data));
            }
        } else if command == "status" {
            if let Some(data) = &response.data {
                println!("{}", serde_json::to_string_pretty(data)?);
            }
        } else if !response.ok {
            eprintln!("Error: {}", response.error.unwrap_or_default());
            std::process::exit(1);
        }
        return Ok(());
    }

    // --- Daemon mode ---

    // Resolve Anthropic auth: env var > stored creds > interactive OAuth
    let (anthropic_token, is_oauth) = if auth::is_authenticated() {
        auth::get_valid_token().await?
    } else {
        println!("No Anthropic credentials found. Starting authentication...\n");
        let _verifier = auth::start_oauth()?;
        eprint!("Paste the code from the browser: ");
        let mut code = String::new();
        std::io::stdin().read_line(&mut code)?;
        let code = code.trim();
        if code.is_empty() {
            anyhow::bail!("No code provided");
        }
        auth::complete_oauth(code).await?;
        println!("Authenticated successfully.\n");
        auth::get_valid_token().await?
    };

    // Resolve ElevenLabs key: env var > stored creds > optional prompt
    let elevenlabs_key = if let Some(key) = auth::get_elevenlabs_key() {
        key
    } else {
        eprint!("ElevenLabs API key (optional, press Enter to skip): ");
        let mut key = String::new();
        std::io::stdin().read_line(&mut key)?;
        let key = key.trim().to_string();
        if !key.is_empty() {
            auth::store_elevenlabs_key(&key)?;
        }
        key
    };

    // Create event bus
    let bus = event::EventBus::new(256);
    let event_tx = bus.sender();

    // Initialize voice engine based on STT provider
    let (voice_engine, speech_rx, ws_disconnect_rx): (
        Option<Arc<VoiceEngine>>,
        Option<tokio::sync::mpsc::Receiver<()>>,
        Option<tokio::sync::mpsc::Receiver<()>>,
    ) = match config.voice.stt_provider {
            config::SttProvider::Elevenlabs => {
                match VoiceEngine::new_elevenlabs(elevenlabs_key.clone(), event_tx.clone()) {
                    Ok(mut engine) => {
                        if config.voice.mode == VoiceMode::PushToTalk {
                            // PTT uses batch transcription — no realtime WebSocket needed.
                            let rx = engine.take_speech_rx();
                            (Some(Arc::new(engine)), rx, None)
                        } else {
                            // AlwaysOn: start realtime WebSocket for streaming STT
                            match engine.start_realtime_stream().await {
                                Ok(disconnect_rx) => {
                                    let rx = engine.take_speech_rx();
                                    (Some(Arc::new(engine)), rx, Some(disconnect_rx))
                                }
                                Err(e) => {
                                    eprintln!("Warning: realtime STT init failed ({}). Starting without voice.", e);
                                    (None, None, None)
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: voice engine init failed ({}). Starting without voice.", e);
                        (None, None, None)
                    }
                }
            }
            config::SttProvider::Whisper => {
                match voice::ensure_model(&config.voice.whisper_model).await {
                    Ok(model_path) => {
                        match VoiceEngine::new_whisper(model_path.to_str().unwrap_or_default(), event_tx.clone()) {
                            Ok(mut engine) => {
                                let rx = engine.take_speech_rx();
                                (Some(Arc::new(engine)), rx, None)
                            }
                            Err(e) => {
                                eprintln!("Warning: voice engine init failed ({}). Starting without voice.", e);
                                (None, None, None)
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: model download failed ({}). Starting without voice.", e);
                        (None, None, None)
                    }
                }
            }
        };

    // Initialize tmux session (per-directory)
    let cwd = std::env::current_dir().unwrap_or_default();
    let session_name = effective_session;
    let tmux_ctrl = tmux::TmuxController::new(&session_name);
    if !tmux_ctrl.session_exists().await {
        // Check if there's an existing Claude Code session to continue
        let project_key = cwd.to_string_lossy().replace('/', "-");
        let has_existing_session = brain::find_latest_jsonl(&project_key).is_some();
        tmux_ctrl.start_session(has_existing_session, &cwd).await?;
    }
    tmux_ctrl.configure_session().await?;

    // Load CLAUDE.md for project context (check cwd, then home)
    let claude_md = {
        let local = std::path::Path::new("CLAUDE.md");
        let home = dirs::home_dir().map(|h| h.join(".claude/CLAUDE.md"));
        if local.exists() {
            std::fs::read_to_string(local).unwrap_or_default()
        } else if let Some(ref p) = home {
            if p.exists() { std::fs::read_to_string(p).unwrap_or_default() } else { String::new() }
        } else {
            String::new()
        }
    };
    // Load Claude Code's conversation history from its JSONL transcript
    let project_dir = cwd.to_string_lossy().replace('/', "-");
    let (claude_code_history, jsonl_offset) = brain::load_claude_code_history(&project_dir);
    let mut brain = brain::Brain::new(
        anthropic_token,
        config.brain.model.clone(),
        config.brain.complex_model.clone(),
        is_oauth,
        &claude_md,
        &claude_code_history,
    );
    let tts_client = tts::ElevenLabsClient::new(
        elevenlabs_key,
        config.tts.voice_id.clone(),
        config.tts.model_id.clone(),
    );
    let audio_player = Arc::new(audio::AudioPlayer::new());

    // Shared state for IPC
    let shared_state = Arc::new(RwLock::new(SharedState::default()));

    // Status bar — runs as its own task with a dedicated event subscription
    let status_bar = status::StatusBar::new()?;
    let status_audio_level = voice_engine.as_ref()
        .map(|e| e.audio_level.clone())
        .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicU8::new(0)));
    let mut status_event_rx = bus.subscribe();
    let is_ptt_mode = config.voice.mode == VoiceMode::PushToTalk;
    tokio::spawn(async move {
        status_bar_task(&mut status_event_rx, &status_bar, &status_audio_level, is_ptt_mode).await;
    });

    // Start IPC server (session-specific socket)
    let ipc_state = shared_state.clone();
    let ipc_event_tx = event_tx.clone();
    let ipc_session = session_name.clone();
    tokio::spawn(async move {
        if let Err(e) = ipc::start_server(ipc_state, ipc_event_tx, &ipc_session).await {
            tracing::error!("IPC server error: {}", e);
        }
    });

    // Subscribe to events
    let mut event_rx = bus.subscribe();

    // Spawn voice task
    if let Some(engine) = voice_engine.clone() {
        let voice_mode = config.voice.mode.clone();
        let voice_event_tx = event_tx.clone();
        let voice_event_rx = bus.subscribe();
        let voice_audio_player = audio_player.clone();
        tokio::spawn(voice_task(engine, voice_mode, speech_rx, voice_event_tx, voice_event_rx, ws_disconnect_rx, voice_audio_player));
    }

    // Spawn tmux attach as a child process — user interacts directly with tmux
    let mut child = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &session_name])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    // Run the daemon loop in a background task
    let daemon_event_tx = event_tx.clone();
    let daemon_state = shared_state.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_loop(
            &mut event_rx,
            &daemon_event_tx,
            &tmux_ctrl,
            &mut brain,
            &tts_client,
            &audio_player,
            &config,
            &daemon_state,
            &project_dir,
            jsonl_offset,
        ).await
    });

    // Wait for either child exit (user detached) or signals
    tokio::select! {
        // Child process (tmux attach) exited — user detached or tmux died
        _ = tokio::task::spawn_blocking(move || child.wait()) => {
            tracing::info!("tmux attach exited, shutting down");
            let _ = event_tx.send(Event::Quit);
        }
        // Daemon loop finished (e.g. got Quit event from IPC)
        result = daemon_handle => {
            if let Err(e) = result {
                tracing::error!("Daemon loop error: {}", e);
            }
        }
        // SIGTERM / SIGINT
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Signal received, shutting down");
            let _ = event_tx.send(Event::Quit);
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(ipc::socket_path(&session_name));
    status_bar_cleanup();

    Ok(())
}

fn status_bar_cleanup() {
    let path = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("vclaw")
        .join("status.txt");
    let _ = std::fs::remove_file(path);
}

async fn voice_task(
    engine: Arc<VoiceEngine>,
    voice_mode: VoiceMode,
    speech_rx: Option<tokio::sync::mpsc::Receiver<()>>,
    event_tx: broadcast::Sender<Event>,
    mut event_rx: broadcast::Receiver<Event>,
    ws_disconnect_rx: Option<tokio::sync::mpsc::Receiver<()>>,
    audio_player: Arc<audio::AudioPlayer>,
) {
    let uses_realtime = engine.uses_realtime();

    // Start audio capture on a dedicated thread (cpal::Stream is !Send)
    let (stream_stop_tx, stream_stop_rx) = std::sync::mpsc::channel::<()>();
    let (capture_ready_tx, capture_ready_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let capture_engine = engine.clone();
    let capture_mode = voice_mode.clone();
    let stream_thread = std::thread::spawn(move || {
        let _stream = match capture_engine.start_capture(&capture_mode) {
            Ok(s) => {
                let _ = capture_ready_tx.send(Ok(()));
                s
            }
            Err(e) => {
                let _ = capture_ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        let _ = stream_stop_rx.recv();
    });

    match capture_ready_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = event_tx.send(Event::ConversationEntry {
                role: "vclaw".into(),
                text: format!("[Audio capture failed: {}]", e),
            });
            drop(stream_stop_tx);
            let _ = stream_thread.join();
            return;
        }
        Err(_) => {
            let _ = event_tx.send(Event::ConversationEntry {
                role: "vclaw".into(),
                text: "[Audio capture thread died]".into(),
            });
            return;
        }
    }

    if uses_realtime && voice_mode != VoiceMode::PushToTalk {
        // Realtime mode: transcripts arrive via Event::UserSaid from the WebSocket.
        // This task manages mute/speaking state and WebSocket reconnection.
        tracing::info!("Voice: realtime mode active");

        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
        let mut muted = false;
        let mut disconnect_rx = ws_disconnect_rx;

        loop {
            // Use a future that resolves when the disconnect receiver fires, or pends forever
            let disconnect_fut = async {
                if let Some(ref mut drx) = disconnect_rx {
                    drx.recv().await
                } else {
                    std::future::pending().await
                }
            };

            tokio::select! {
                _ = disconnect_fut => {
                    tracing::warn!("Realtime STT WebSocket disconnected, reconnecting...");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    match engine.start_realtime_stream().await {
                        Ok(new_drx) => {
                            disconnect_rx = Some(new_drx);
                            tracing::info!("Realtime STT reconnected");
                        }
                        Err(e) => {
                            tracing::error!("Realtime STT reconnect failed: {}", e);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            match engine.start_realtime_stream().await {
                                Ok(new_drx) => {
                                    disconnect_rx = Some(new_drx);
                                    tracing::info!("Realtime STT reconnected on retry");
                                }
                                Err(e) => {
                                    tracing::error!("Realtime STT reconnect retry failed: {}, giving up", e);
                                    disconnect_rx = None;
                                }
                            }
                        }
                    }
                }
                result = event_rx.recv() => {
                    match result {
                        Ok(Event::Quit) | Err(broadcast::error::RecvError::Closed) => break,
                        Ok(Event::MuteToggle) => {
                            muted = !muted;
                            engine.is_speaking.store(muted, Ordering::Relaxed);
                        }
                        Ok(Event::VoiceStatus(VoiceStatus::Speaking)) => {
                            engine.is_speaking.store(true, Ordering::Relaxed);
                        }
                        Ok(Event::VoiceStatus(VoiceStatus::Idle)) => {
                            if !muted {
                                engine.is_speaking.store(false, Ordering::Relaxed);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    } else if voice_mode == VoiceMode::AlwaysOn {
        // Local VAD mode (Whisper AlwaysOn)
        let mut speech_rx = match speech_rx {
            Some(rx) => rx,
            None => {
                tracing::error!("No speech_rx available for AlwaysOn mode");
                drop(stream_stop_tx);
                let _ = stream_thread.join();
                return;
            }
        };

        tracing::info!("Voice: AlwaysOn mode active (local VAD)");
        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
        let mut muted = false;

        loop {
            tokio::select! {
                Some(()) = speech_rx.recv() => {
                    if muted {
                        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                        continue;
                    }
                    match engine.transcribe().await {
                        Ok(text) if !text.is_empty() => {
                            let _ = event_tx.send(Event::UserSaid(text));
                        }
                        Ok(_) => {
                            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                        }
                        Err(e) => {
                            tracing::error!("Transcription failed: {}", e);
                            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                        }
                    }
                }
                Ok(event) = event_rx.recv() => {
                    match event {
                        Event::Quit => break,
                        Event::MuteToggle => {
                            muted = !muted;
                            engine.is_speaking.store(muted, Ordering::Relaxed);
                        }
                        Event::VoiceStatus(VoiceStatus::Speaking) => {
                            if !muted {
                                engine.is_speaking.store(true, Ordering::Relaxed);
                            }
                        }
                        Event::VoiceStatus(VoiceStatus::Idle) => {
                            if !muted {
                                engine.is_speaking.store(false, Ordering::Relaxed);
                                engine.clear_buffer();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    } else {
        // PushToTalk mode
        let mut is_recording = false;
        let mut current_status = VoiceStatus::Idle;
        tracing::debug!("Voice: PushToTalk mode active");
        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));

        loop {
            match event_rx.recv().await {
                Ok(Event::VoiceToggle) => {
                    if is_recording {
                        engine.stop_recording();
                        is_recording = false;
                        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Thinking));
                        match engine.transcribe().await {
                            Ok(text) if !text.is_empty() => {
                                let _ = event_tx.send(Event::UserSaid(text));
                            }
                            Ok(_) => {
                                let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                            }
                            Err(e) => {
                                tracing::error!("Transcription failed: {}", e);
                                let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                            }
                        }
                    } else if matches!(current_status, VoiceStatus::Speaking | VoiceStatus::Thinking) {
                        // F12 while speaking/processing = interrupt, don't start recording
                        let _ = event_tx.send(Event::Interrupt);
                        tracing::info!("F12 interrupt: was {:?}, sending interrupt", current_status);
                    } else {
                        // Idle — start recording
                        audio_player.interrupt();
                        engine.start_recording();
                        is_recording = true;
                        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Listening));
                    }
                }
                Ok(Event::VoiceStatus(status)) => {
                    match &status {
                        VoiceStatus::Speaking => engine.is_speaking.store(true, Ordering::Relaxed),
                        VoiceStatus::Idle => engine.is_speaking.store(false, Ordering::Relaxed),
                        _ => {}
                    }
                    current_status = status;
                }
                Ok(Event::Quit) | Err(broadcast::error::RecvError::Closed) => break,
                _ => {}
            }
        }
    }

    drop(stream_stop_tx);
    let _ = stream_thread.join();
}

/// Dedicated task that owns status bar updates. Has its own event subscription
/// so it's never blocked by brain processing in the daemon loop.
async fn status_bar_task(
    event_rx: &mut broadcast::Receiver<Event>,
    status_bar: &status::StatusBar,
    audio_level: &Arc<std::sync::atomic::AtomicU8>,
    ptt_mode: bool,
) {
    let mut voice_status = VoiceStatus::Idle;
    let mut muted = false;
    let mut level_tick = tokio::time::interval(Duration::from_millis(200));

    loop {
        tokio::select! {
            result = event_rx.recv() => {
                match result {
                    Ok(Event::VoiceStatus(s)) => {
                        voice_status = s;
                        let level = audio_level.load(std::sync::atomic::Ordering::Relaxed);
                        status_bar.update(&voice_status, muted, level, ptt_mode).ok();
                    }
                    Ok(Event::MuteToggle) => {
                        muted = !muted;
                        let level = audio_level.load(std::sync::atomic::Ordering::Relaxed);
                        status_bar.update(&voice_status, muted, level, ptt_mode).ok();
                    }
                    Ok(Event::Quit) | Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    _ => {}
                }
            }
            _ = level_tick.tick() => {
                // Only animate volume meter for states where mic input matters
                match voice_status {
                    VoiceStatus::Idle | VoiceStatus::Listening => {
                        let level = audio_level.load(std::sync::atomic::Ordering::Relaxed);
                        status_bar.update(&voice_status, muted, level, ptt_mode).ok();
                    }
                    _ => {}
                }
            }
        }
    }
}

#[allow(unused_assignments)]
async fn run_daemon_loop(
    event_rx: &mut broadcast::Receiver<Event>,
    event_tx: &broadcast::Sender<Event>,
    tmux_ctrl: &tmux::TmuxController,
    brain: &mut brain::Brain,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    config: &Config,
    shared_state: &Arc<RwLock<SharedState>>,
    project_dir: &str,
    initial_jsonl_offset: u64,
) -> Result<()> {
    // Poll Claude Code's JSONL transcript for new entries
    let mut jsonl_tick = tokio::time::interval(Duration::from_secs(1));
    jsonl_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut jsonl_offset = initial_jsonl_offset;
    // Debounce: only act on WaitingForPermission if seen on consecutive polls
    let mut pending_permission = false;
    let mut pending_tool_name = String::new();
    // Track last known Claude Code state for user message context
    let mut last_claude_state = brain::ClaudeCodeState::Unknown;
    // Debounce JSONL updates: accumulate entries, only act after quiet period
    let mut accumulated_entries = String::new();
    let mut last_activity_time: Option<tokio::time::Instant> = None;
    // Track whether brain/audio is busy to avoid overlapping responses
    // (reads and writes happen across different select! arms, so compiler warns incorrectly)
    #[allow(unused_assignments)]
    let mut brain_busy = false;

    // Find active pane ID once for shell_input
    let active_pane_id = tmux_ctrl.list_panes().await
        .unwrap_or_default()
        .iter()
        .find(|p| p.active)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "%0".into());

    loop {
        tokio::select! {
            result = event_rx.recv() => {
                match result {
                    Ok(event) => {
                        // Update shared state for IPC
                        {
                            let mut state = shared_state.write().await;
                            match &event {
                                Event::VoiceStatus(status) => state.voice_status = status.clone(),
                                Event::ConversationEntry { role, text } => {
                                    state.conversation.push((role.clone(), text.clone()));
                                }
                                Event::LiveTranscript(text) => state.live_transcript = text.clone(),
                                Event::UserSaid(_) => state.live_transcript.clear(),
                                Event::MuteToggle => state.muted = !state.muted,
                                _ => {}
                            }
                        }

                        // Handle events
                        match event {
                            Event::Quit => return Ok(()),
                            Event::Interrupt => {
                                audio_player.interrupt();
                                tmux_ctrl.send_raw_key(&active_pane_id, "Escape").await.ok();
                                tmux_ctrl.send_raw_key(&active_pane_id, "C-c").await.ok();
                                let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                                let fresh = brain::poll_claude_code_history(project_dir, 0);
                                jsonl_offset = fresh.1;
                                pending_permission = false;
                                brain_busy = false;
                                accumulated_entries.clear();
                                last_activity_time = None;
                                tracing::info!("Interrupt: stopped audio + sent Ctrl+C to {}", active_pane_id);
                            }
                            Event::UserSaid(text) => {
                                let clean = text.trim();
                                if is_noise(clean) {
                                    tracing::debug!("Filtered noise transcription: {:?}", clean);
                                    let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                                    continue;
                                }

                                let _ = event_tx.send(Event::ConversationEntry {
                                    role: "You".into(),
                                    text: text.clone(),
                                });

                                let is_complex = brain::is_complex_request(&text);
                                tracing::info!("Request complexity: {} (complex={})",
                                    if is_complex { "sonnet" } else { "haiku" }, is_complex);

                                let user_msg = brain::build_user_message(&text, &active_pane_id, &last_claude_state);
                                brain.add_user_message(&user_msg);

                                brain_busy = true;
                                interruptible_brain_response(
                                    brain, tmux_ctrl, tts_client, audio_player, event_tx,
                                    &active_pane_id, is_complex,
                                ).await?;
                                brain_busy = false;
                            }
                            _ => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Event bus lagged, skipped {} events", n);
                    }
                }
            }

            // Poll Claude Code's JSONL for new activity
            _ = jsonl_tick.tick() => {
                let (new_entries, new_offset, state) = brain::poll_claude_code_history(project_dir, jsonl_offset);
                jsonl_offset = new_offset;

                // When file unchanged and we have a pending permission, confirm it
                let effective_state = if new_entries.is_empty() && pending_permission {
                    brain::ClaudeCodeState::WaitingForPermission { tool_name: pending_tool_name.clone() }
                } else {
                    state
                };

                // Update last known state for user message context
                if effective_state != brain::ClaudeCodeState::Unknown {
                    last_claude_state = effective_state.clone();
                }

                // Accumulate new entries
                if !new_entries.is_empty() {
                    if !accumulated_entries.is_empty() {
                        accumulated_entries.push('\n');
                    }
                    accumulated_entries.push_str(&new_entries);
                    last_activity_time = Some(tokio::time::Instant::now());
                    tracing::debug!("Accumulated JSONL entries, state={:?}", effective_state);
                }

                // Skip Working/Unknown — let Claude Code do its thing
                if matches!(effective_state, brain::ClaudeCodeState::Working | brain::ClaudeCodeState::Unknown) {
                    if !matches!(effective_state, brain::ClaudeCodeState::Unknown) {
                        pending_permission = false;
                    }
                    continue;
                }

                // Debounce permission: first time we see it, just flag it.
                if matches!(effective_state, brain::ClaudeCodeState::WaitingForPermission { .. }) {
                    if !pending_permission {
                        if let brain::ClaudeCodeState::WaitingForPermission { ref tool_name } = effective_state {
                            pending_tool_name = tool_name.clone();
                        }
                        tracing::debug!("Permission state detected, waiting one more poll to confirm");
                        pending_permission = true;
                        continue;
                    }
                    // Second consecutive poll — it's really waiting, act now
                    tracing::info!("Permission confirmed after debounce, acting");
                } else if matches!(effective_state, brain::ClaudeCodeState::Idle) {
                    // Debounce Idle: wait for 3s of no new JSONL entries before acting
                    // This batches rapid-fire screen changes into one update
                    if let Some(last_time) = last_activity_time {
                        if last_time.elapsed() < Duration::from_secs(3) {
                            tracing::debug!("Idle detected but activity was {}ms ago, waiting to settle",
                                last_time.elapsed().as_millis());
                            continue;
                        }
                    } else if accumulated_entries.is_empty() && !pending_permission {
                        continue;
                    }
                }

                // Don't overlap: skip if brain/audio still busy from last response
                if brain_busy {
                    tracing::debug!("Brain still busy, deferring JSONL update");
                    continue;
                }

                // Drain accumulated entries
                let entries_to_send = std::mem::take(&mut accumulated_entries);
                last_activity_time = None;

                if entries_to_send.is_empty() && !pending_permission {
                    continue;
                }

                // Read screen when Claude Code is waiting for input or idle
                let screen_content = if matches!(effective_state, brain::ClaudeCodeState::WaitingForPermission { .. } | brain::ClaudeCodeState::Idle) {
                    tmux_ctrl.capture_pane(&active_pane_id, config.brain.max_context_lines).await.ok()
                } else {
                    None
                };

                tracing::info!("Claude Code state: {:?}, entries_len={}", effective_state, entries_to_send.len());
                let msg = brain::build_history_update_message(
                    &entries_to_send,
                    &effective_state,
                    screen_content.as_deref(),
                );
                brain.add_user_message(&msg);

                brain_busy = true;
                interruptible_brain_response(
                    brain, tmux_ctrl, tts_client, audio_player, event_tx,
                    &active_pane_id, false,
                ).await.ok();
                brain_busy = false;
                pending_permission = false;
            }
        }
    }
}

/// Wraps handle_brain_response with interrupt support.
/// Races brain processing against interrupt/quit events so the user can cancel at any time.
/// Returns true if interrupted, false if completed normally.
async fn interruptible_brain_response(
    brain: &mut brain::Brain,
    tmux_ctrl: &tmux::TmuxController,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    event_tx: &broadcast::Sender<Event>,
    active_pane_id: &str,
    is_complex: bool,
) -> Result<bool> {
    let mut interrupt_rx = event_tx.subscribe();
    tokio::select! {
        result = handle_brain_response(brain, tmux_ctrl, tts_client, audio_player, event_tx, is_complex) => {
            result?;
            Ok(false)
        }
        _ = async {
            loop {
                match interrupt_rx.recv().await {
                    Ok(Event::Interrupt) => break,
                    Ok(Event::Quit) => break,
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => continue,
                }
            }
        } => {
            // Interrupt received — cancel brain processing, stop audio, send Ctrl+C
            audio_player.interrupt();
            tmux_ctrl.send_raw_key(active_pane_id, "Escape").await.ok();
            tmux_ctrl.send_raw_key(active_pane_id, "C-c").await.ok();
            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
            tracing::info!("Brain response interrupted by user");
            Ok(true)
        }
    }
}

async fn handle_brain_response(
    brain: &mut brain::Brain,
    tmux_ctrl: &tmux::TmuxController,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    event_tx: &broadcast::Sender<Event>,
    is_complex: bool,
) -> Result<()> {
    let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Thinking));
    let mut loop_iteration = 0u32;

    loop {
        loop_iteration += 1;
        let mut rx = match brain.send_streaming(is_complex).await {
            Ok(rx) => rx,
            Err(e) => {
                if loop_iteration == 1 {
                    tracing::error!("Brain request failed: {}, clearing history", e);
                    brain.clear_messages();
                } else {
                    tracing::error!("Brain follow-up failed: {}, stopping", e);
                }
                let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                return Ok(());
            }
        };

        // Accumulate content blocks as stream events arrive
        struct BlockAccum {
            block_type: String,
            id: Option<String>,
            name: Option<String>,
            text: String,
            json_buf: String,
        }
        let mut blocks: Vec<BlockAccum> = Vec::new();
        let mut stop_reason = String::new();
        let mut tool_results: Vec<(String, String, bool)> = Vec::new(); // (id, result, is_error)

        while let Some(event) = rx.recv().await {
            match event {
                brain::StreamEvent::ContentBlockStart { index, block_type, id, name } => {
                    // Ensure blocks vec is large enough
                    while blocks.len() <= index {
                        blocks.push(BlockAccum {
                            block_type: String::new(),
                            id: None,
                            name: None,
                            text: String::new(),
                            json_buf: String::new(),
                        });
                    }
                    blocks[index].block_type = block_type;
                    blocks[index].id = id;
                    blocks[index].name = name;
                }
                brain::StreamEvent::TextDelta { index, text } => {
                    if let Some(block) = blocks.get_mut(index) {
                        block.text.push_str(&text);
                        // Live feedback via transcript
                        let _ = event_tx.send(Event::LiveTranscript(block.text.clone()));
                    }
                }
                brain::StreamEvent::InputJsonDelta { index, partial_json } => {
                    if let Some(block) = blocks.get_mut(index) {
                        block.json_buf.push_str(&partial_json);
                    }
                }
                brain::StreamEvent::ContentBlockStop { index } => {
                    if let Some(block) = blocks.get(index) {
                        if block.block_type == "text" && !block.text.is_empty() {
                            // Clear any live transcript indicator
                            let _ = event_tx.send(Event::LiveTranscript(String::new()));
                            // Don't add ConversationEntry here — the speak tool does it
                            // to avoid duplicates
                        }
                        if block.block_type == "tool_use" {
                            // Tool block complete — execute immediately
                            let tool_id = block.id.clone().unwrap_or_default();
                            let tool_name = block.name.clone().unwrap_or_default();
                            let input: serde_json::Value = serde_json::from_str(&block.json_buf)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                            let result = execute_tool(
                                &tool_name, &input, tmux_ctrl, tts_client, audio_player, event_tx,
                            ).await;
                            let (result_text, is_error) = match result {
                                Ok(t) => (t, false),
                                Err(e) => (e.to_string(), true),
                            };
                            tool_results.push((tool_id, result_text, is_error));
                        }
                    }
                }
                brain::StreamEvent::MessageDelta { stop_reason: sr } => {
                    stop_reason = sr;
                }
                brain::StreamEvent::Done => break,
            }
        }

        // If stream produced no content, the API likely returned an error (e.g. 400).
        if blocks.is_empty() || (blocks.iter().all(|b| b.text.is_empty() && b.json_buf.is_empty())) {
            if loop_iteration == 1 {
                // First call failed — clear history to recover
                tracing::warn!("Brain returned empty response, clearing history to recover");
                brain.clear_messages();
            } else {
                // Follow-up after tools already executed — just stop, keep history
                tracing::warn!("Brain follow-up returned empty, stopping tool loop");
            }
            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
            return Ok(());
        }

        // Build the assembled content blocks for conversation history
        let mut content_blocks: Vec<brain::ContentBlock> = Vec::new();
        for block in &blocks {
            match block.block_type.as_str() {
                "text" => {
                    content_blocks.push(brain::ContentBlock::Text { text: block.text.clone() });
                }
                "tool_use" => {
                    let input: serde_json::Value = serde_json::from_str(&block.json_buf)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    content_blocks.push(brain::ContentBlock::ToolUse {
                        id: block.id.clone().unwrap_or_default(),
                        name: block.name.clone().unwrap_or_default(),
                        input,
                    });
                }
                _ => {}
            }
        }

        // Store in conversation history
        let content_json = serde_json::to_value(&content_blocks)?;
        brain.add_assistant_response(content_json);

        // Add tool results to conversation history
        for (id, result_text, is_error) in &tool_results {
            brain.add_tool_result(id, result_text, *is_error);
        }

        // Stop if: no tools used, model said end_turn, or only tools were speak/shell_input
        // (after sending a prompt + speaking, there's nothing to follow up on)
        let only_terminal_tools = tool_results.len() > 0 && blocks.iter().all(|b| {
            b.block_type != "tool_use" || matches!(b.name.as_deref(), Some("speak") | Some("shell_input"))
        });
        if tool_results.is_empty() || stop_reason == "end_turn" || only_terminal_tools {
            break;
        }
    }

    let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
    Ok(())
}

async fn execute_tool(
    name: &str,
    input: &serde_json::Value,
    tmux_ctrl: &tmux::TmuxController,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    event_tx: &broadcast::Sender<Event>,
) -> Result<String> {
    tracing::info!("Tool call: {} with input: {}", name, input);
    match name {
        "tmux_execute" => {
            tracing::warn!("tmux_execute called but no longer supported — use shell_input instead");
            Ok("tmux_execute is no longer available. Use shell_input to type into Claude Code.".into())
        }
        "shell_input" => {
            let pane = input["pane"].as_str().unwrap_or("%0");
            let text = input["text"].as_str().unwrap_or("");
            let enter = input["enter"].as_bool().unwrap_or(true);

            // Reject empty text — pressing Enter with nothing sends a blank prompt
            if text.trim().is_empty() {
                tracing::warn!("Blocked empty shell_input to {}", pane);
                return Ok("Error: empty text not allowed — would submit a blank prompt to Claude Code.".into());
            }

            tracing::info!("Shell input to {}: {:?} (enter={})", pane, text, enter);
            tmux_ctrl.send_keys(pane, text, enter).await?;
            Ok("Keys sent.".into())
        }
        "clear_pane" => {
            tracing::warn!("clear_pane called but no longer supported");
            Ok("clear_pane is no longer available. Claude Code manages its own screen.".into())
        }
        "read_pane" => {
            tracing::warn!("read_pane called but no longer supported — context comes from JSONL history");
            Ok("read_pane is no longer available. Use Claude Code's conversation history for context.".into())
        }
        "speak" => {
            let message = input["message"].as_str().unwrap_or("");
            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Speaking));
            let _ = event_tx.send(Event::ConversationEntry {
                role: "vclaw".into(),
                text: message.to_string(),
            });
            // Fetch TTS audio via streaming endpoint and play
            if tts_client.has_key() {
                match tts_client.speak_streaming(message).await {
                    Ok(stream) => {
                        use futures::StreamExt;
                        let mut stream = std::pin::pin!(stream);
                        let mut mp3_bytes = Vec::new();
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => mp3_bytes.extend_from_slice(&bytes),
                                Err(e) => {
                                    tracing::error!("TTS stream chunk error: {}", e);
                                    break;
                                }
                            }
                        }
                        tracing::debug!("TTS streamed {} bytes", mp3_bytes.len());
                        let player = audio_player.clone();
                        let idle_tx = event_tx.clone();
                        // Stop any currently playing audio before starting new playback
                        player.interrupt();
                        player.reset();
                        // Fire-and-forget: play in background so the event loop stays responsive
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = player.play_mp3(mp3_bytes) {
                                tracing::error!("Audio playback failed: {}", e);
                            }
                            // Signal idle when playback finishes (or is interrupted)
                            let _ = idle_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
                        });
                    }
                    Err(e) => {
                        tracing::error!("TTS request failed: {}", e);
                        let _ = event_tx.send(Event::ConversationEntry {
                            role: "vclaw".into(),
                            text: format!("[TTS error: {}]", e),
                        });
                    }
                }
            } else {
                tracing::debug!("No ElevenLabs key — skipping TTS");
                let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
            }
            Ok("Spoken".into())
        }
        _ => Ok(format!("Unknown tool: {}", name)),
    }
}

/// Filter out transcriptions that are clearly noise, not real speech.
fn is_noise(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    // Whisper artifacts
    if text == "[BLANK_AUDIO]" || text == "(blank audio)" {
        return true;
    }
    // Bracketed/parenthesized descriptions like [music], (typing sounds), etc.
    if (text.starts_with('[') && text.ends_with(']'))
        || (text.starts_with('(') && text.ends_with(')'))
    {
        return true;
    }
    // Too short to be intentional speech (single char, random syllable)
    let word_count = text.split_whitespace().count();
    let alpha_chars: usize = text.chars().filter(|c| c.is_alphabetic()).count();
    if alpha_chars < 2 {
        return true;
    }
    // Common filler / noise transcriptions
    let lower = text.to_lowercase();
    let noise_phrases = [
        "um", "uh", "hmm", "hm", "ah", "oh", "eh",
        "...", "you", "the", "a", "i", "it",
        "thank you.", "thanks for watching",
        "bye.", "goodbye.", "see you.",
        "subtitles by", "translated by",
        "music", "applause", "laughter",
    ];
    if word_count <= 1 && noise_phrases.contains(&lower.trim_end_matches('.').trim()) {
        return true;
    }
    // Common STT hallucinations (model outputs for silence/noise)
    let hallucinations = [
        "foreign", "silence", "inaudible",
        "please subscribe", "like and subscribe",
        "subtitles by", "translated by",
    ];
    let trimmed_lower = lower.trim_end_matches(|c: char| !c.is_alphabetic()).trim();
    if hallucinations.contains(&trimmed_lower) {
        return true;
    }
    // "thank you" variants that are hallucinations
    if trimmed_lower.starts_with("thank you") || trimmed_lower.starts_with("thanks for watching") {
        return true;
    }
    // Repeated single characters/syllables (keyboard noise)
    if word_count <= 2 && text.len() <= 4 {
        return true;
    }
    false
}
