use anyhow::{Context, Result};
use tokio::process::Command;
use crate::event::PaneInfo;
use std::path::PathBuf;

pub struct TmuxController {
    session_name: String,
}

pub struct CommandResult {
    pub stdout: String,
    pub success: bool,
}

impl TmuxController {
    pub fn new(session_name: &str) -> Self {
        Self {
            session_name: session_name.to_string(),
        }
    }

    pub async fn execute_raw(&self, full_command: &str) -> Result<CommandResult> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("tmux {}", full_command))
            .output()
            .await
            .context("Failed to execute tmux command")?;

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            success: output.status.success(),
        })
    }

    /// Execute tmux with properly separated arguments (no shell escaping needed).
    async fn execute_args(&self, args: &[&str]) -> Result<CommandResult> {
        let output = Command::new("tmux")
            .args(args)
            .output()
            .await
            .context("Failed to execute tmux command")?;

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            success: output.status.success(),
        })
    }

    pub async fn start_session(&self, continue_session: bool, cwd: &std::path::Path) -> Result<()> {
        let claude_cmd = if continue_session { "claude -c" } else { "claude" };
        let cwd_str = cwd.to_string_lossy();
        self.execute_raw(&format!(
            "new-session -d -s {} -c '{}' {}",
            self.session_name, cwd_str, claude_cmd
        )).await?;
        Ok(())
    }

    pub async fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let result = self.execute_raw(&format!(
            "list-panes -t {} -F '#{{pane_id}}\t#{{pane_active}}'",
            self.session_name
        )).await?;

        let panes = result.stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let parts: Vec<&str> = line.split('\t').collect();
                PaneInfo {
                    id: parts.first().unwrap_or(&"").to_string(),
                    active: parts.get(1).unwrap_or(&"0") == &"1",
                }
            })
            .collect();

        Ok(panes)
    }

    pub async fn capture_pane(&self, target: &str, lines: usize) -> Result<String> {
        // -S -N: start N lines before cursor, -E: end at last line of visible pane
        // This captures the most recent content including the current cursor line
        let result = self.execute_raw(&format!(
            "capture-pane -t {} -p -e -S -{} -E -",
            target, lines
        )).await?;
        // Trim trailing empty lines but keep the content near the cursor
        let trimmed = result.stdout.trim_end().to_string();
        Ok(trimmed)
    }

    /// Send a tmux key name (e.g., "C-c", "Escape") without the -l literal flag.
    pub async fn send_raw_key(&self, target: &str, key: &str) -> Result<()> {
        self.execute_args(&["send-keys", "-t", target, key]).await?;
        Ok(())
    }

    pub async fn send_keys(&self, target: &str, keys: &str, enter: bool) -> Result<()> {
        // Use execute_args (direct process args) instead of execute_raw (sh -c)
        // to avoid shell escaping issues with special characters in the text.
        // The -l flag sends text literally without tmux interpreting key names.
        self.execute_args(&["send-keys", "-t", target, "-l", keys]).await?;
        if enter {
            self.execute_args(&["send-keys", "-t", target, "Enter"]).await?;
        }
        Ok(())
    }

    pub async fn session_exists(&self) -> bool {
        self.execute_raw(&format!("has-session -t {}", self.session_name))
            .await
            .map(|r| r.success)
            .unwrap_or(false)
    }

    pub fn status_file_path_for_session(session_name: &str) -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("vclaw")
            .join(format!("status-{}.txt", session_name))
    }

    /// Configure the tmux session with vclaw status bar and key bindings.
    pub async fn configure_session(&self) -> Result<()> {
        let status_file = Self::status_file_path_for_session(&self.session_name);
        let status_path = status_file.to_string_lossy();

        // Use shell quoting inside #() to handle paths with spaces
        let cat_cmd = format!("#(cat \"{}\")", status_path);

        // Status bar — use execute_args to avoid shell escaping issues
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-right", &cat_cmd,
        ]).await?;
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-right-length", "40",
        ]).await?;
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-interval", "1",
        ]).await?;
        // Style the status bar
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-style", "bg=default",
        ]).await?;

        // Left: brand pill + project name
        let project_name = self.session_name
            .strip_prefix("vclaw-")
            .unwrap_or(&self.session_name);
        let status_left = format!(
            "#[fg=colour16,bg=colour39,bold] \u{25c6} vclaw #[default] #[fg=colour245]{}#[default] ",
            project_name
        );
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-left", &status_left,
        ]).await?;
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "status-left-length", "40",
        ]).await?;

        // Window list styling
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "window-status-current-format", "#[fg=colour39,bold]#W#[default]",
        ]).await?;
        self.execute_args(&[
            "set-option", "-t", &self.session_name,
            "window-status-format", "#[fg=colour245]#W#[default]",
        ]).await?;

        // Key bindings use #{session_name} so tmux resolves the CURRENT session
        // at keypress time. This way multiple vclaw instances don't fight over bindings —
        // F12 always talks to whichever session you're in.
        let ctl_voice = "vclaw --session #{session_name} ctl voice_toggle";
        let ctl_mute = "vclaw --session #{session_name} ctl mute";
        let ctl_interrupt = "vclaw --session #{session_name} ctl interrupt";
        let ctl_conversation = "vclaw --session #{session_name} ctl conversation";

        // Root table — no prefix needed
        self.execute_args(&[
            "bind-key", "-T", "root", "F12",
            "run-shell", ctl_voice,
        ]).await?;
        self.execute_args(&[
            "bind-key", "-T", "root", "M-m",
            "run-shell", ctl_mute,
        ]).await?;
        // Interrupt is handled by F12 toggle (press while speaking/processing).
        // Escape in prefix table as explicit fallback.
        self.execute_args(&[
            "bind-key", "Escape",
            "run-shell", ctl_interrupt,
        ]).await?;

        // Prefix table
        self.execute_args(&[
            "bind-key", "Space",
            "run-shell", ctl_voice,
        ]).await?;
        self.execute_args(&[
            "bind-key", "C",
            "display-popup", "-w", "80%", "-h", "80%",
            "sh", "-c", ctl_conversation,
        ]).await?;

        Ok(())
    }
}
