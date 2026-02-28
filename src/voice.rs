use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use crate::event::{Event, VoiceStatus};

pub struct VoiceEngine {
    whisper_ctx: Arc<WhisperContext>,
    event_tx: broadcast::Sender<Event>,
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    is_recording: Arc<Mutex<bool>>,
}

impl VoiceEngine {
    pub fn new(model_path: &str, event_tx: broadcast::Sender<Event>) -> Result<Self> {
        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(model_path, params)
            .context("Failed to load Whisper model")?;

        Ok(Self {
            whisper_ctx: Arc::new(ctx),
            event_tx,
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(Mutex::new(false)),
        })
    }

    pub fn start_capture(&self) -> Result<cpal::Stream> {
        let host = cpal::default_host();
        let device = host.default_input_device()
            .context("No input device available")?;

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(16000),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer = self.audio_buffer.clone();
        let is_recording = self.is_recording.clone();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if *is_recording.lock().unwrap() {
                    buffer.lock().unwrap().extend_from_slice(data);
                }
            },
            |err| {
                tracing::error!("Audio capture error: {}", err);
            },
            None,
        )?;

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

    pub fn transcribe(&self) -> Result<String> {
        let _ = self.event_tx.send(Event::VoiceStatus(VoiceStatus::Thinking));

        let audio = self.audio_buffer.lock().unwrap().clone();
        if audio.is_empty() {
            return Ok(String::new());
        }

        let mut state = self.whisper_ctx.create_state()
            .context("Failed to create Whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, &audio)
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
}
