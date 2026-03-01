use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::event::VoiceStatus;
use crate::tmux::TmuxController;

/// Minimum interval between tmux refresh calls (ms).
const REFRESH_THROTTLE_MS: u64 = 100;

static LAST_REFRESH: AtomicU64 = AtomicU64::new(0);

pub struct StatusBar {
    path: std::path::PathBuf,
}

/// Center-out voice bar: grows symmetrically from the middle based on audio level.
/// Uses block characters for a smooth waveform look.
fn center_bars(level: u8) -> &'static str {
    match level.min(8) {
        0 => "    \u{2502}    ",       // just center line
        1 => "   \u{2581}\u{2502}\u{2581}   ",
        2 => "  \u{2581}\u{2582}\u{2502}\u{2582}\u{2581}  ",
        3 => " \u{2581}\u{2582}\u{2583}\u{2502}\u{2583}\u{2582}\u{2581} ",
        4 => "\u{2581}\u{2582}\u{2583}\u{2584}\u{2502}\u{2584}\u{2583}\u{2582}\u{2581}",
        5 => "\u{2582}\u{2583}\u{2584}\u{2585}\u{2502}\u{2585}\u{2584}\u{2583}\u{2582}",
        6 => "\u{2583}\u{2584}\u{2585}\u{2586}\u{2502}\u{2586}\u{2585}\u{2584}\u{2583}",
        7 => "\u{2584}\u{2585}\u{2586}\u{2587}\u{2502}\u{2587}\u{2586}\u{2585}\u{2584}",
        _ => "\u{2585}\u{2586}\u{2587}\u{2588}\u{2502}\u{2588}\u{2587}\u{2586}\u{2585}",
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl StatusBar {
    pub fn new() -> Result<Self> {
        let path = TmuxController::status_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, "#[fg=colour245,dim]\u{25c7} starting")?;
        Ok(Self { path })
    }

    pub fn update(&self, voice_status: &VoiceStatus, muted: bool, audio_level: u8, ptt_mode: bool) -> Result<()> {
        let content = if muted {
            "#[fg=colour203,bold]\u{25cf} muted".to_string()
        } else {
            match voice_status {
                VoiceStatus::Idle => {
                    if ptt_mode {
                        "#[fg=colour245]\u{25c7} F12 to talk".to_string()
                    } else {
                        "#[fg=colour114]\u{25c7} ready".to_string()
                    }
                }
                VoiceStatus::Listening => {
                    let bars = center_bars(audio_level);
                    if audio_level > 0 {
                        format!("#[fg=colour114]{}", bars)
                    } else {
                        format!("#[fg=colour114]    \u{2502}    ")
                    }
                }
                VoiceStatus::Thinking => {
                    "#[fg=colour221,bold]\u{25c6} thinking".to_string()
                }
                VoiceStatus::Speaking => {
                    "#[fg=colour117,bold]\u{25c6} speaking".to_string()
                }
            }
        };

        std::fs::write(&self.path, &content)?;

        // Throttle refresh calls to avoid hammering tmux
        let now = now_ms();
        let last = LAST_REFRESH.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= REFRESH_THROTTLE_MS {
            LAST_REFRESH.store(now, Ordering::Relaxed);
            let _ = std::process::Command::new("tmux")
                .args(["refresh-client", "-S"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }

        Ok(())
    }
}
