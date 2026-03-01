use anyhow::{Context, Result};
use base64::Engine as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use crate::config::{SttProvider, VoiceMode};
use crate::event::{Event, VoiceStatus};

/// Minimum audio length in samples at 16kHz (1 second).
const MIN_AUDIO_SAMPLES: usize = 16000;

/// RAII guard that redirects fd 2 (stderr) to /dev/null while alive.
struct SuppressStderr {
    saved_fd: Option<i32>,
}

impl SuppressStderr {
    fn new() -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::io::IntoRawFd;
            unsafe {
                let saved = libc::dup(2);
                if saved >= 0 {
                    if let Ok(devnull) = std::fs::File::open("/dev/null") {
                        libc::dup2(devnull.into_raw_fd(), 2);
                    }
                    return Self { saved_fd: Some(saved) };
                }
            }
        }
        Self { saved_fd: None }
    }
}

impl Drop for SuppressStderr {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(fd) = self.saved_fd {
            unsafe {
                libc::dup2(fd, 2);
                libc::close(fd);
            }
        }
    }
}

/// RMS energy threshold for local VAD (Whisper / PushToTalk paths only).
const ENERGY_THRESHOLD: f32 = 0.03;
const SILENCE_FRAMES_REQUIRED: u32 = 120;
const MIN_SPEECH_FRAMES: u32 = 5;

/// Downloads the whisper model if not already cached, returns the local path.
pub async fn ensure_model(model_name: &str) -> Result<PathBuf> {
    let filename = format!("ggml-{}.en.bin", model_name);
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        filename
    );

    let model_dir = dirs::data_local_dir()
        .context("Could not determine local data directory")?
        .join("vclaw")
        .join("models");

    std::fs::create_dir_all(&model_dir)?;

    let model_path = model_dir.join(&filename);
    if model_path.exists() {
        return Ok(model_path);
    }

    println!("Downloading whisper model '{}'...", model_name);

    let response = reqwest::get(&url).await
        .context("Failed to start model download")?;

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;

    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut file = std::fs::File::create(&model_path)?;

    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Error during model download")?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if total_size > 0 {
            let pct = (downloaded as f64 / total_size as f64 * 100.0) as u32;
            print!("\rDownloading... {}% ({}/{}MB)", pct, downloaded / 1_000_000, total_size / 1_000_000);
            std::io::stdout().flush().ok();
        }
    }
    println!("\nModel downloaded to {}", model_path.display());

    Ok(model_path)
}

/// Scan $PATH directories and collect binary names for vocabulary hinting.
fn collect_path_binaries() -> Vec<String> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for dir in path_var.split(':') {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.len() >= 2
                        && name.len() <= 30
                        && name.is_ascii()
                        && !name.starts_with('.')
                        && !name.contains('.')
                    {
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }

    names.into_iter().take(500).collect()
}

/// Encode f32 PCM samples (16kHz mono) as a WAV byte vector.
fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let sample_rate: u32 = 16000;
    let bits_per_sample: u16 = 16;
    let num_channels: u16 = 1;
    let byte_rate = sample_rate * (bits_per_sample as u32 / 8) * num_channels as u32;
    let block_align = num_channels * (bits_per_sample / 8);
    let data_size = (samples.len() * 2) as u32;
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(44 + data_size as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&num_channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }
    buf
}

/// Convert f32 samples to 16-bit PCM bytes (little-endian).
fn f32_to_i16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }
    buf
}

pub struct VoiceEngine {
    whisper_ctx: Option<Arc<whisper_rs::WhisperContext>>,
    stt_provider: SttProvider,
    elevenlabs_key: String,
    http_client: reqwest::Client,
    event_tx: broadcast::Sender<Event>,
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    is_recording: Arc<Mutex<bool>>,
    speech_done_tx: tokio::sync::mpsc::Sender<()>,
    speech_done_rx: Option<tokio::sync::mpsc::Receiver<()>>,
    /// Shared channel for streaming audio chunks to the realtime WebSocket.
    /// Behind Arc<Mutex> so reconnection can swap the sender without restarting capture.
    audio_chunk_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    /// Set to true while TTS is playing to suppress audio streaming.
    pub is_speaking: Arc<AtomicBool>,
    /// Current audio input level (0-8 scale) for status bar visualization.
    pub audio_level: Arc<std::sync::atomic::AtomicU8>,
    /// Binary names from $PATH for vocabulary biasing.
    vocab_binaries: Vec<String>,
}

impl VoiceEngine {
    /// Create a voice engine with ElevenLabs STT (no local model needed).
    pub fn new_elevenlabs(elevenlabs_key: String, event_tx: broadcast::Sender<Event>) -> Result<Self> {
        let (speech_done_tx, speech_done_rx) = tokio::sync::mpsc::channel(8);
        let vocab_binaries = collect_path_binaries();
        tracing::info!("ElevenLabs STT engine created");

        Ok(Self {
            whisper_ctx: None,
            stt_provider: SttProvider::Elevenlabs,
            elevenlabs_key,
            http_client: reqwest::Client::new(),
            event_tx,
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(Mutex::new(false)),
            speech_done_tx,
            speech_done_rx: Some(speech_done_rx),
            audio_chunk_tx: Arc::new(Mutex::new(None)),
            is_speaking: Arc::new(AtomicBool::new(false)),
            audio_level: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            vocab_binaries,
        })
    }

    /// Create a voice engine with local Whisper model.
    pub fn new_whisper(model_path: &str, event_tx: broadcast::Sender<Event>) -> Result<Self> {
        let _stderr_guard = SuppressStderr::new();
        let params = whisper_rs::WhisperContextParameters::default();
        let ctx = whisper_rs::WhisperContext::new_with_params(model_path, params)
            .context("Failed to load Whisper model")?;
        let (speech_done_tx, speech_done_rx) = tokio::sync::mpsc::channel(8);
        let vocab_binaries = collect_path_binaries();

        Ok(Self {
            whisper_ctx: Some(Arc::new(ctx)),
            stt_provider: SttProvider::Whisper,
            elevenlabs_key: String::new(),
            http_client: reqwest::Client::new(),
            event_tx,
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(Mutex::new(false)),
            speech_done_tx,
            speech_done_rx: Some(speech_done_rx),
            audio_chunk_tx: Arc::new(Mutex::new(None)),
            is_speaking: Arc::new(AtomicBool::new(false)),
            audio_level: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            vocab_binaries,
        })
    }

    /// Take the speech-done receiver (can only be called once).
    pub fn take_speech_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<()>> {
        self.speech_done_rx.take()
    }

    /// Whether this engine uses the realtime WebSocket (ElevenLabs AlwaysOn).
    pub fn uses_realtime(&self) -> bool {
        self.stt_provider == SttProvider::Elevenlabs
    }

    /// Start (or reconnect) the ElevenLabs realtime WebSocket STT session.
    /// Spawns background tasks for sending audio and receiving transcripts.
    /// Returns a receiver that signals when the WebSocket disconnects (for reconnection).
    /// Safe to call multiple times — swaps the audio sender so the capture callback
    /// seamlessly routes to the new WebSocket.
    pub async fn start_realtime_stream(&self) -> Result<tokio::sync::mpsc::Receiver<()>> {
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        *self.audio_chunk_tx.lock().unwrap() = Some(audio_tx);
        let (disconnect_tx, disconnect_rx) = tokio::sync::mpsc::channel::<()>(1);

        let url = format!(
            "wss://api.elevenlabs.io/v1/speech-to-text/realtime?\
             model_id=scribe_v2_realtime&\
             language_code=en&\
             audio_format=pcm_16000&\
             commit_strategy=vad&\
             vad_silence_threshold_secs=1.0&\
             vad_threshold=0.3&\
             min_speech_duration_ms=300&\
             min_silence_duration_ms=200"
        );

        let mut request = url.into_client_request()
            .context("Failed to build WebSocket request")?;

        request.headers_mut().insert(
            "xi-api-key",
            self.elevenlabs_key.parse().context("Invalid API key header")?,
        );

        tracing::info!("Connecting to ElevenLabs realtime STT WebSocket...");
        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await
            .context("Failed to connect to ElevenLabs realtime WebSocket")?;
        tracing::info!("ElevenLabs realtime WebSocket connected");

        let (mut ws_write, mut ws_read) = futures::StreamExt::split(ws_stream);

        // Sender task: reads PCM chunks from channel, base64-encodes, sends to WS
        let is_speaking = self.is_speaking.clone();
        tokio::spawn(async move {
            while let Some(pcm_bytes) = audio_rx.recv().await {
                if is_speaking.load(Ordering::Relaxed) {
                    continue;
                }
                let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_bytes);
                let msg = serde_json::json!({
                    "message_type": "input_audio_chunk",
                    "audio_base_64": b64,
                });
                if let Err(e) = ws_write.send(WsMessage::Text(msg.to_string().into())).await {
                    tracing::error!("WebSocket send error: {}", e);
                    break;
                }
            }
            tracing::info!("Realtime audio sender task ended");
        });

        // Receiver task: reads transcripts from WS, emits events
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            // Track last committed text to deduplicate — ElevenLabs can send both
            // committed_transcript and committed_transcript_with_timestamps for
            // the same utterance, which would fire UserSaid twice.
            let mut last_committed = String::new();
            while let Some(msg_result) = ws_read.next().await {
                match msg_result {
                    Ok(WsMessage::Text(text)) => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            let msg_type = json["message_type"].as_str().unwrap_or("");
                            match msg_type {
                                "session_started" => {
                                    tracing::info!("Realtime STT session started: {}", json["session_id"]);
                                }
                                "partial_transcript" => {
                                    let text = json["text"].as_str().unwrap_or("").to_string();
                                    if !text.is_empty() {
                                        let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Listening));
                                        let _ = event_tx.send(Event::LiveTranscript(text));
                                    }
                                }
                                "committed_transcript" | "committed_transcript_with_timestamps" => {
                                    let text = json["text"].as_str().unwrap_or("").trim().to_string();
                                    if !text.is_empty() && text != last_committed {
                                        tracing::info!("Realtime STT: {:?}", text);
                                        let _ = event_tx.send(Event::UserSaid(text.clone()));
                                        last_committed = text;
                                    } else if text == last_committed {
                                        tracing::debug!("Skipping duplicate committed transcript: {:?}", text);
                                    }
                                }
                                "error" | "auth_error" | "quota_exceeded" | "rate_limited" => {
                                    let err = json["error"].as_str().unwrap_or("unknown error");
                                    tracing::error!("Realtime STT {}: {}", msg_type, err);
                                    let _ = event_tx.send(Event::ConversationEntry {
                                        role: "vclaw".into(),
                                        text: format!("[STT error: {}]", err),
                                    });
                                }
                                _ => {
                                    tracing::warn!("Realtime STT unknown msg: {}", text);
                                }
                            }
                        }
                    }
                    Ok(WsMessage::Close(_)) => {
                        tracing::info!("Realtime WebSocket closed by server");
                        break;
                    }
                    Err(e) => {
                        tracing::error!("WebSocket receive error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            tracing::info!("Realtime transcript receiver task ended");
            let _ = disconnect_tx.send(()).await;
        });

        Ok(disconnect_rx)
    }

    /// Start audio capture. For ElevenLabs realtime, streams directly to WebSocket.
    /// For Whisper/PushToTalk, buffers locally with VAD.
    pub fn start_capture(&self, voice_mode: &VoiceMode) -> Result<cpal::Stream> {
        let host = cpal::default_host();
        let device = host.default_input_device()
            .context("No input device available")?;

        let (config, native_rate, native_channels) = {
            let ideal = cpal::StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(16000),
                buffer_size: cpal::BufferSize::Default,
            };
            if device.supported_input_configs()
                .map(|mut cfgs| cfgs.any(|r| {
                    r.channels() >= 1
                        && r.min_sample_rate().0 <= 16000
                        && r.max_sample_rate().0 >= 16000
                }))
                .unwrap_or(false)
            {
                (ideal, 16000u32, 1u16)
            } else {
                let default = device.default_input_config()
                    .context("No supported input config")?;
                let rate = default.sample_rate().0;
                let ch = default.channels();
                (cpal::StreamConfig {
                    channels: ch,
                    sample_rate: default.sample_rate(),
                    buffer_size: cpal::BufferSize::Default,
                }, rate, ch)
            }
        };

        let needs_resample = native_rate != 16000 || native_channels != 1;
        let use_realtime = self.audio_chunk_tx.lock().unwrap().is_some();

        let stream = if use_realtime {
            // Realtime mode: stream audio to WebSocket with local energy gate.
            // Uses Arc<Mutex> so reconnection can swap the sender transparently.
            let audio_tx_holder = self.audio_chunk_tx.clone();
            let is_speaking = self.is_speaking.clone();
            let audio_level = self.audio_level.clone();

            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if is_speaking.load(Ordering::Relaxed) {
                        audio_level.store(0, Ordering::Relaxed);
                        return;
                    }
                    let mono16k = if needs_resample {
                        resample_to_16k_mono(data, native_rate, native_channels)
                    } else {
                        data.to_vec()
                    };
                    // Compute RMS and map to 0-8 level
                    let rms = (mono16k.iter().map(|s| s * s).sum::<f32>() / mono16k.len().max(1) as f32).sqrt();
                    let level = (rms * 80.0).min(8.0) as u8;
                    audio_level.store(level, Ordering::Relaxed);

                    let pcm_bytes = f32_to_i16_bytes(&mono16k);
                    if let Some(ref tx) = *audio_tx_holder.lock().unwrap() {
                        let _ = tx.try_send(pcm_bytes);
                    }
                },
                |err| tracing::error!("Audio capture error: {}", err),
                None,
            )?
        } else {
            // Local VAD mode (Whisper or PushToTalk)
            let buffer = self.audio_buffer.clone();
            let is_recording = self.is_recording.clone();

            match voice_mode {
                VoiceMode::PushToTalk => {
                    let audio_level = self.audio_level.clone();
                    device.build_input_stream(
                        &config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            let mono16k = if needs_resample {
                                resample_to_16k_mono(data, native_rate, native_channels)
                            } else {
                                data.to_vec()
                            };
                            let rms = (mono16k.iter().map(|s| s * s).sum::<f32>() / mono16k.len().max(1) as f32).sqrt();
                            let level = (rms * 80.0).min(8.0) as u8;
                            audio_level.store(level, Ordering::Relaxed);

                            if *is_recording.lock().unwrap() {
                                buffer.lock().unwrap().extend_from_slice(&mono16k);
                            }
                        },
                        |err| tracing::error!("Audio capture error: {}", err),
                        None,
                    )?
                }
                VoiceMode::AlwaysOn => {
                    let speech_done_tx = self.speech_done_tx.clone();
                    let is_speaking = self.is_speaking.clone();
                    let event_tx = self.event_tx.clone();
                    let speech_frames = Arc::new(Mutex::new(0u32));
                    let silence_frames = Arc::new(Mutex::new(0u32));
                    let audio_level = self.audio_level.clone();

                    device.build_input_stream(
                        &config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if is_speaking.load(Ordering::Relaxed) {
                                audio_level.store(0, Ordering::Relaxed);
                                return;
                            }

                            let mono16k = if needs_resample {
                                resample_to_16k_mono(data, native_rate, native_channels)
                            } else {
                                data.to_vec()
                            };

                            let rms = (mono16k.iter().map(|s| s * s).sum::<f32>() / mono16k.len().max(1) as f32).sqrt();
                            let level = (rms * 80.0).min(8.0) as u8;
                            audio_level.store(level, Ordering::Relaxed);
                            let is_speech = rms > ENERGY_THRESHOLD;

                            let mut sf = speech_frames.lock().unwrap();
                            let mut silf = silence_frames.lock().unwrap();
                            let mut rec = is_recording.lock().unwrap();

                            if is_speech {
                                *sf += 1;
                                *silf = 0;
                                if !*rec && *sf >= MIN_SPEECH_FRAMES {
                                    *rec = true;
                                    buffer.lock().unwrap().clear();
                                    let _ = event_tx.send(Event::VoiceStatus(VoiceStatus::Listening));
                                }
                            } else {
                                *silf += 1;
                                if *silf > SILENCE_FRAMES_REQUIRED && *rec {
                                    *rec = false;
                                    *sf = 0;
                                    let _ = speech_done_tx.try_send(());
                                }
                            }

                            if *rec {
                                buffer.lock().unwrap().extend_from_slice(&mono16k);
                            }
                        },
                        |err| tracing::error!("Audio capture error: {}", err),
                        None,
                    )?
                }
            }
        };

        stream.play()?;
        Ok(stream)
    }

    pub fn start_recording(&self) {
        self.audio_buffer.lock().unwrap().clear();
        *self.is_recording.lock().unwrap() = true;
        let _ = self.event_tx.send(Event::VoiceStatus(VoiceStatus::Listening));
    }

    pub fn stop_recording(&self) {
        *self.is_recording.lock().unwrap() = false;
    }

    /// Clear the audio buffer (e.g. after TTS playback to discard echo).
    pub fn clear_buffer(&self) {
        self.audio_buffer.lock().unwrap().clear();
    }

    /// Transcribe using the configured STT provider (batch mode only).
    /// Used for Whisper and PushToTalk. Not used for realtime mode.
    pub async fn transcribe(&self) -> Result<String> {
        let _ = self.event_tx.send(Event::VoiceStatus(VoiceStatus::Thinking));

        let mut audio = self.audio_buffer.lock().unwrap().clone();
        if audio.is_empty() {
            return Ok(String::new());
        }

        if audio.len() < MIN_AUDIO_SAMPLES {
            audio.resize(MIN_AUDIO_SAMPLES, 0.0);
        }

        match self.stt_provider {
            SttProvider::Elevenlabs => self.transcribe_elevenlabs(&audio).await,
            SttProvider::Whisper => self.transcribe_whisper(&audio),
        }
    }

    fn transcribe_whisper(&self, audio: &[f32]) -> Result<String> {
        let ctx = self.whisper_ctx.as_ref()
            .context("Whisper context not initialized")?;

        let _stderr_guard = SuppressStderr::new();

        let mut state = ctx.create_state()
            .context("Failed to create Whisper state")?;

        let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        let vocab_prompt: String = self.vocab_binaries.join(", ");
        params.set_initial_prompt(&vocab_prompt);

        state.full(params, audio)
            .context("Whisper transcription failed")?;

        let num_segments = state.full_n_segments()
            .context("Failed to get segment count")?;

        let mut text = String::new();
        for i in 0..num_segments {
            if let Ok(segment) = state.full_get_segment_text(i) {
                text.push_str(&segment);
            }
        }

        Ok(text.trim().to_string())
    }

    async fn transcribe_elevenlabs(&self, audio: &[f32]) -> Result<String> {
        let wav_bytes = encode_wav(audio);

        tracing::info!("ElevenLabs batch STT: {} samples ({:.1}s)",
            audio.len(), audio.len() as f32 / 16000.0);

        let file_part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;

        let form = reqwest::multipart::Form::new()
            .text("model_id", "scribe_v2")
            .text("language_code", "en")
            .text("tag_audio_events", "false")
            .part("file", file_part);

        let response = self.http_client
            .post("https://api.elevenlabs.io/v1/speech-to-text")
            .header("xi-api-key", &self.elevenlabs_key)
            .multipart(form)
            .send()
            .await
            .context("ElevenLabs STT request failed")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            tracing::error!("ElevenLabs STT error {}: {}", status, body);
            anyhow::bail!("ElevenLabs STT error {}: {}", status, body);
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .context("Failed to parse ElevenLabs STT response")?;

        let text = json["text"].as_str().unwrap_or("").trim().to_string();
        tracing::info!("ElevenLabs STT result: {:?}", text);
        Ok(text)
    }
}

/// Downmix to mono and resample to 16kHz using linear interpolation.
fn resample_to_16k_mono(data: &[f32], src_rate: u32, channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    let mono: Vec<f32> = data.chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect();

    if src_rate == 16000 {
        return mono;
    }

    let ratio = src_rate as f64 / 16000.0;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f64;

        let sample = if idx + 1 < mono.len() {
            mono[idx] * (1.0 - frac as f32) + mono[idx + 1] * frac as f32
        } else if idx < mono.len() {
            mono[idx]
        } else {
            0.0
        };
        out.push(sample);
    }

    out
}
