mod auth;
mod audio;
mod config;
mod event;
mod tmux;
mod brain;
mod tts;
mod tui;
mod voice;

use anyhow::{Context, Result};
use crossterm::event::{self as crossterm_event, Event as CrosstermEvent};
use std::time::Duration;
use tokio::sync::broadcast;

use crate::config::{CliCommand, Config};
use crate::event::{Event, VoiceStatus};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let (config, cli) = Config::load_with_cli()?;

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
        let ctrl = tmux::TmuxController::new("vclaw");
        if ctrl.session_exists().await {
            #[cfg(unix)]
            {
                let err = std::process::Command::new("tmux")
                    .args(["attach-session", "-t", "vclaw"])
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
            eprintln!("No vclaw session found. Run 'vclaw' to start one.");
            std::process::exit(1);
        }
    }

    // Resolve Anthropic auth: env var takes priority, then stored credentials
    let (anthropic_token, is_oauth) = auth::get_valid_token().await
        .context("Not authenticated. Run 'vclaw auth' or set ANTHROPIC_API_KEY")?;
    let elevenlabs_key = std::env::var("ELEVENLABS_API_KEY")
        .context("ELEVENLABS_API_KEY environment variable not set")?;

    // Create event bus
    let bus = event::EventBus::new(256);
    let event_tx = bus.sender();

    // Initialize tmux session
    let tmux_ctrl = tmux::TmuxController::new("vclaw");
    if !tmux_ctrl.session_exists().await {
        tmux_ctrl.start_session().await?;
    }

    let mut brain = brain::Brain::new(anthropic_token, config.brain.model.clone(), is_oauth);
    let tts_client = tts::ElevenLabsClient::new(
        elevenlabs_key,
        config.tts.voice_id.clone(),
        config.tts.model_id.clone(),
    );
    let audio_player = std::sync::Arc::new(audio::AudioPlayer::new());

    // Start TUI
    let mut tui = tui::Tui::new(event_tx.clone())?;
    let mut event_rx = bus.subscribe();

    // Main event loop
    let result = run_main_loop(
        &mut tui,
        &mut event_rx,
        &event_tx,
        &tmux_ctrl,
        &mut brain,
        &tts_client,
        &audio_player,
        &config,
    ).await;

    // Cleanup
    tui.cleanup()?;
    result
}

async fn run_main_loop(
    tui: &mut tui::Tui,
    event_rx: &mut broadcast::Receiver<Event>,
    event_tx: &broadcast::Sender<Event>,
    tmux_ctrl: &tmux::TmuxController,
    brain: &mut brain::Brain,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    config: &Config,
) -> Result<()> {
    let mut poll_interval = tokio::time::interval(Duration::from_millis(config.tmux.poll_interval_ms));

    loop {
        tui.draw()?;

        tokio::select! {
            // Handle crossterm keyboard events
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if crossterm_event::poll(Duration::from_millis(0))? {
                    if let CrosstermEvent::Key(key) = crossterm_event::read()? {
                        if let Some(ev) = tui.handle_key_event(key) {
                            if matches!(ev, Event::Quit) {
                                return Ok(());
                            }
                            let _ = event_tx.send(ev);
                        }
                    }
                }
            }

            // Handle events from the bus
            Ok(event) = event_rx.recv() => {
                tui.update_state(&event);

                match event {
                    Event::Quit => return Ok(()),
                    Event::UserSaid(text) => {
                        let _ = event_tx.send(Event::ConversationEntry {
                            role: "You".into(),
                            text: text.clone(),
                        });

                        // Get tmux context
                        let panes = tmux_ctrl.list_panes().await.unwrap_or_default();
                        let active_pane = panes.iter().find(|p| p.active);
                        let content = if let Some(pane) = active_pane {
                            tmux_ctrl.capture_pane(&pane.id, config.brain.max_context_lines)
                                .await.unwrap_or_default()
                        } else {
                            String::new()
                        };

                        let user_msg = brain::build_user_message(&text, &panes, &content);
                        brain.add_user_message(&user_msg);

                        // Send to Claude and handle tool loop
                        handle_brain_response(brain, tmux_ctrl, tts_client, audio_player, event_tx).await?;
                    }
                    _ => {}
                }
            }

            // Poll tmux state
            _ = poll_interval.tick() => {
                if let Ok(panes) = tmux_ctrl.list_panes().await {
                    let _ = event_tx.send(Event::PaneListUpdated(panes.clone()));
                    if let Some(active) = panes.iter().find(|p| p.active) {
                        if let Ok(content) = tmux_ctrl.capture_pane(&active.id, 50).await {
                            let _ = event_tx.send(Event::ActivePaneContent(content));
                        }
                    }
                }
            }
        }
    }
}

async fn handle_brain_response(
    brain: &mut brain::Brain,
    tmux_ctrl: &tmux::TmuxController,
    tts_client: &tts::ElevenLabsClient,
    audio_player: &std::sync::Arc<audio::AudioPlayer>,
    event_tx: &broadcast::Sender<Event>,
) -> Result<()> {
    let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Thinking));

    loop {
        let response = brain.send().await?;

        // Store full response content for conversation history
        let content_json = serde_json::to_value(&response.content)?;
        brain.add_assistant_response(content_json);

        let mut has_tool_use = false;

        for block in &response.content {
            match block {
                brain::ContentBlock::Text { text } => {
                    let _ = event_tx.send(Event::ConversationEntry {
                        role: "vclaw".into(),
                        text: text.clone(),
                    });
                }
                brain::ContentBlock::ToolUse { id, name, input } => {
                    has_tool_use = true;
                    let result = execute_tool(name, input, tmux_ctrl, tts_client, audio_player, event_tx).await;
                    let (result_text, is_error) = match result {
                        Ok(text) => (text, false),
                        Err(e) => (e.to_string(), true),
                    };
                    brain.add_tool_result(id, &result_text, is_error);
                }
            }
        }

        if !has_tool_use || response.stop_reason.as_deref() == Some("end_turn") {
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
    match name {
        "tmux_execute" => {
            let command = input["command"].as_str().unwrap_or("");
            let result = tmux_ctrl.execute_raw(command).await?;
            Ok(format!("stdout: {}\nstderr: {}", result.stdout, result.stderr))
        }
        "shell_input" => {
            let pane = input["pane"].as_str().unwrap_or("%0");
            let text = input["text"].as_str().unwrap_or("");
            let enter = input["enter"].as_bool().unwrap_or(true);
            tmux_ctrl.send_keys(pane, text, enter).await?;
            Ok("Keys sent".into())
        }
        "read_pane" => {
            let pane = input["pane"].as_str().unwrap_or("%0");
            let lines = input["lines"].as_u64().unwrap_or(50) as usize;
            let content = tmux_ctrl.capture_pane(pane, lines).await?;
            Ok(content)
        }
        "speak" => {
            let message = input["message"].as_str().unwrap_or("");
            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Speaking));
            let _ = event_tx.send(Event::ConversationEntry {
                role: "vclaw".into(),
                text: message.to_string(),
            });
            // Fetch TTS audio and play through speakers
            match tts_client.speak(message).await {
                Ok(mp3_bytes) => {
                    tracing::info!("TTS generated {} bytes", mp3_bytes.len());
                    let player = audio_player.clone();
                    player.reset();
                    // Play on a blocking thread so we don't block the async runtime
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = player.play_mp3(mp3_bytes) {
                            tracing::error!("Audio playback failed: {}", e);
                        }
                    }).await?;
                }
                Err(e) => {
                    tracing::error!("TTS request failed: {}", e);
                }
            }
            let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Idle));
            Ok("Spoken".into())
        }
        _ => Ok(format!("Unknown tool: {}", name)),
    }
}
