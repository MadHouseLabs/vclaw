use anyhow::Result;
use rodio::{Decoder, OutputStream, Sink};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct AudioPlayer {
    interrupted: Arc<AtomicBool>,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.interrupted.store(false, Ordering::SeqCst);
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    /// Play MP3 audio bytes (e.g. from ElevenLabs) through the default output device.
    /// Blocks until playback completes or is interrupted.
    pub fn play_mp3(&self, mp3_bytes: Vec<u8>) -> Result<()> {
        let (_stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)?;

        let cursor = Cursor::new(mp3_bytes);
        let source = Decoder::new(cursor)?;
        sink.append(source);

        // Poll for completion or interrupt
        while !sink.empty() {
            if self.interrupted.load(Ordering::SeqCst) {
                sink.stop();
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        sink.sleep_until_end();
        Ok(())
    }
}
