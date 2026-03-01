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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl StatusBar {
    pub fn new(session_name: &str) -> Result<Self> {
        let path = TmuxController::status_file_path_for_session(session_name);
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
                    // 3-diamond level meter: ◇◇◇ → ◆◇◇ → ◆◆◇ → ◆◆◆
                    let d1 = if audio_level > 1 { "\u{25c6}" } else { "\u{25c7}" };
                    let d2 = if audio_level > 3 { "\u{25c6}" } else { "\u{25c7}" };
                    let d3 = if audio_level > 5 { "\u{25c6}" } else { "\u{25c7}" };
                    format!("#[fg=colour114]{}{}{}", d1, d2, d3)
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
