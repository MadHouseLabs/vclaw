use anyhow::{Context, Result};
use tokio::process::Command;
use crate::event::PaneInfo;

pub struct TmuxController {
    session_name: String,
}

pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl TmuxController {
    pub fn new(session_name: &str) -> Self {
        Self {
            session_name: session_name.to_string(),
        }
    }

    pub async fn execute(&self, args: &str) -> Result<CommandResult> {
        let output = Command::new("tmux")
            .args(args.split_whitespace())
            .output()
            .await
            .context("Failed to execute tmux command")?;

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        })
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
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        })
    }

    pub async fn start_session(&self) -> Result<()> {
        self.execute_raw(&format!(
            "new-session -d -s {}",
            self.session_name
        )).await?;
        Ok(())
    }

    pub async fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let result = self.execute_raw(&format!(
            "list-panes -t {} -F '#{{pane_id}}\t#{{pane_title}}\t#{{pane_width}}x#{{pane_height}}\t#{{pane_active}}'",
            self.session_name
        )).await?;

        let panes = result.stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let parts: Vec<&str> = line.split('\t').collect();
                PaneInfo {
                    id: parts.first().unwrap_or(&"").to_string(),
                    title: parts.get(1).unwrap_or(&"").to_string(),
                    size: parts.get(2).unwrap_or(&"").to_string(),
                    active: parts.get(3).unwrap_or(&"0") == &"1",
                }
            })
            .collect();

        Ok(panes)
    }

    pub async fn capture_pane(&self, target: &str, lines: usize) -> Result<String> {
        let result = self.execute_raw(&format!(
            "capture-pane -t {} -p -S -{}",
            target, lines
        )).await?;
        Ok(result.stdout)
    }

    pub async fn send_keys(&self, target: &str, keys: &str, enter: bool) -> Result<()> {
        let mut cmd = format!("send-keys -t {} '{}' ", target, keys);
        if enter {
            cmd.push_str("Enter");
        }
        self.execute_raw(&cmd).await?;
        Ok(())
    }

    pub async fn session_exists(&self) -> bool {
        self.execute_raw(&format!("has-session -t {}", self.session_name))
            .await
            .map(|r| r.success)
            .unwrap_or(false)
    }
}
