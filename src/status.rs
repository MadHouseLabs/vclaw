//! Push-based tmux status bar integration.
//!
//! The status bar shows the current voice state in tmux's status-right area.
//! Updates are event-driven — the [`StatusBar`] only touches tmux when the
//! rendered content actually changes (content-change guard).
//!
//! The client name for `refresh-client` is resolved lazily because
//! `StatusBar::new()` is called before `tmux attach-session` (no client
//! exists yet).

use anyhow::Result;

use crate::event::VoiceStatus;

/// Manages the tmux status-right indicator showing voice state.
///
/// Renders one of: starting, ready, F12 to talk, listening, thinking,
/// speaking, or muted. Uses tmux colour codes for visual distinction.
pub struct StatusBar {
    session_name: String,
    /// Cached tmux client name (e.g. "/dev/ttys005") for refresh-client calls.
    /// Resolved lazily since StatusBar is created before tmux attach.
    client_name: std::sync::Mutex<Option<String>>,
    /// Last rendered content — skip tmux calls when unchanged.
    last_content: std::sync::Mutex<String>,
}

// Status bar design system
//
// Format:  icon + space + label
//
// Icons:
//   ◇  outline diamond — inactive / waiting
//   ◆  filled diamond  — active / doing something
//   ●  filled circle   — muted (intentionally distinct from diamond family)
//
// Colors (tmux colour numbers):
//   245  grey       — inactive states (starting, idle PTT)
//   114  green      — voice-active states (ready, listening)
//   221  yellow     — processing (thinking)
//   117  blue       — output (speaking)
//   203  red        — muted
//
// Bold: active states that represent work in progress (thinking, speaking, muted).
//
// Updates are push-based: tmux set-option + refresh-client on every content
// change. The content-change guard eliminates redundant calls, so tmux is
// only touched when the status bar actually needs to look different.

/// Run a tmux command, discarding output. Used for set-option / refresh-client.
fn tmux_fire_and_forget(args: &[&str]) {
    let _ = std::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Resolve the tmux client name attached to a session (e.g. "/dev/ttys005").
/// Takes the first client if multiple are attached.
fn resolve_client(session: &str) -> Option<String> {
    std::process::Command::new("tmux")
        .args(["list-clients", "-t", session, "-F", "#{client_name}"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            let name = s.lines().next().unwrap_or("").trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
}

impl StatusBar {
    pub fn new(session_name: &str) -> Result<Self> {
        let bar = Self {
            session_name: session_name.to_string(),
            client_name: std::sync::Mutex::new(None),
            last_content: std::sync::Mutex::new(String::new()),
        };
        // Initial set-option (no refresh-client yet — no client attached)
        tmux_fire_and_forget(&[
            "set-option",
            "-t",
            session_name,
            "status-right",
            "#[fg=colour245]\u{25c7} starting",
        ]);
        Ok(bar)
    }

    /// Get or lazily resolve the tmux client name.
    fn get_client(&self) -> Option<String> {
        {
            let cached = self.client_name.lock().unwrap();
            if cached.is_some() {
                return cached.clone();
            }
        }
        // Lock dropped — resolve_client spawns a blocking process
        if let Some(name) = resolve_client(&self.session_name) {
            *self.client_name.lock().unwrap() = Some(name.clone());
            return Some(name);
        }
        None
    }

    /// Send content to tmux and force an immediate repaint.
    fn push_to_tmux(&self, content: &str) {
        tmux_fire_and_forget(&[
            "set-option",
            "-t",
            &self.session_name,
            "status-right",
            content,
        ]);
        if let Some(client) = self.get_client() {
            tmux_fire_and_forget(&["refresh-client", "-S", "-t", &client]);
        }
    }

    pub fn update(&self, voice_status: &VoiceStatus, muted: bool, ptt_mode: bool) -> Result<()> {
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
                VoiceStatus::Listening => "#[fg=colour114,bold]\u{25c6} listening".to_string(),
                VoiceStatus::Thinking => "#[fg=colour221,bold]\u{25c6} thinking".to_string(),
                VoiceStatus::Speaking => "#[fg=colour117,bold]\u{25c6} speaking".to_string(),
            }
        };

        // Skip tmux calls entirely if content hasn't changed
        let mut last = self.last_content.lock().unwrap();
        if *last == content {
            return Ok(());
        }
        *last = content.clone();
        drop(last);

        self.push_to_tmux(&content);
        Ok(())
    }
}
