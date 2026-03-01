use anyhow::Result;
use rodio::{Decoder, OutputStream, Sink};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct AudioPlayer {
    interrupted: Arc<AtomicBool>,
    /// Direct handle to the currently playing sink so interrupt() can stop it immediately.
    active_sink: Arc<Mutex<Option<Arc<Sink>>>>,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
            active_sink: Arc::new(Mutex::new(None)),
        }
    }

    /// Stop any currently playing audio immediately.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
        // Directly stop the sink — no 50ms polling delay
        if let Ok(mut guard) = self.active_sink.lock() {
            if let Some(sink) = guard.take() {
                sink.stop();
            }
        }
    }

    pub fn reset(&self) {
        self.interrupted.store(false, Ordering::SeqCst);
    }

    /// Play MP3 audio bytes (e.g. from ElevenLabs) through the default output device.
    /// Blocks until playback completes or is interrupted.
    pub fn play_mp3(&self, mp3_bytes: Vec<u8>) -> Result<()> {
        let (_stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)?;

        let cursor = Cursor::new(mp3_bytes);
        let source = Decoder::new(cursor)?;
        sink.append(source);

        // Store sink so interrupt() can stop it directly from any thread
        let sink = Arc::new(sink);
        *self.active_sink.lock().unwrap() = Some(sink.clone());

        // Poll for completion or interrupt
        while !sink.empty() {
            if self.interrupted.load(Ordering::SeqCst) {
                sink.stop();
                *self.active_sink.lock().unwrap() = None;
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        *self.active_sink.lock().unwrap() = None;
        Ok(())
    }
}
