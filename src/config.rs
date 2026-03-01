//! Configuration loading and CLI argument parsing.
//!
//! Configuration is loaded from `~/.config/vclaw/config.toml` with defaults
//! for all fields. CLI arguments override config file values.

use anyhow::Result;
use clap::Parser;
use serde::Deserialize;
use std::path::Path;

/// Voice activation mode.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceMode {
    AlwaysOn,
    PushToTalk,
}

/// Speech-to-text provider.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    Whisper,
    Elevenlabs,
}

/// Voice input settings: activation mode, STT provider, Whisper model.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    #[serde(default = "default_voice_mode")]
    pub mode: VoiceMode,
    #[serde(default = "default_whisper_model")]
    pub whisper_model: String,
    #[serde(default = "default_stt_provider")]
    pub stt_provider: SttProvider,
}

/// Text-to-speech settings: ElevenLabs voice and model selection.
#[derive(Debug, Clone, Deserialize)]
pub struct TtsConfig {
    #[serde(default = "default_voice_id")]
    pub voice_id: String,
    #[serde(default = "default_tts_model_id")]
    pub model_id: String,
}

/// Brain (Claude API) settings: model selection and context window size.
#[derive(Debug, Clone, Deserialize)]
pub struct BrainConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_complex_model")]
    pub complex_model: String,
    #[serde(default = "default_max_context_lines")]
    pub max_context_lines: usize,
}

/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub voice: VoiceConfig,
    #[serde(default)]
    pub tts: TtsConfig,
    #[serde(default)]
    pub brain: BrainConfig,
}

fn default_voice_mode() -> VoiceMode {
    VoiceMode::PushToTalk
}
fn default_whisper_model() -> String {
    "base".into()
}
fn default_stt_provider() -> SttProvider {
    SttProvider::Elevenlabs
}
fn default_voice_id() -> String {
    "cgSgspJ2msm6clMCkdW9".into()
}
fn default_tts_model_id() -> String {
    "eleven_turbo_v2".into()
}
fn default_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_complex_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_max_context_lines() -> usize {
    50
}
impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            mode: default_voice_mode(),
            whisper_model: default_whisper_model(),
            stt_provider: default_stt_provider(),
        }
    }
}
impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            voice_id: default_voice_id(),
            model_id: default_tts_model_id(),
        }
    }
}
impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            complex_model: default_complex_model(),
            max_context_lines: default_max_context_lines(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = dirs::config_dir().map(|d| d.join("vclaw").join("config.toml"));

        match config_path {
            Some(path) if path.exists() => Self::from_file(&path),
            _ => Ok(Self::default()),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load config from file (or defaults) and merge CLI overrides.
    pub fn load_with_cli() -> Result<(Self, Cli)> {
        let cli = Cli::parse();

        let mut config = match &cli.config {
            Some(path) => Self::from_file(Path::new(path))?,
            None => Self::load()?,
        };

        if let Some(ref model) = cli.whisper {
            config.voice.whisper_model = model.clone();
        }
        if let Some(ref mode) = cli.voice_mode {
            config.voice.mode = match mode.as_str() {
                "ptk" | "push_to_talk" => VoiceMode::PushToTalk,
                _ => VoiceMode::AlwaysOn,
            };
        }

        Ok((config, cli))
    }
}

#[derive(Parser, Debug)]
#[command(name = "vclaw", about = "Voice-controlled tmux manager")]
pub struct Cli {
    /// Path to config file
    #[arg(long)]
    pub config: Option<String>,

    /// Whisper model size (tiny, base, small)
    #[arg(long)]
    pub whisper: Option<String>,

    /// Voice mode (always_on, ptk)
    #[arg(long, value_name = "MODE")]
    pub voice_mode: Option<String>,

    /// Override session name (used by key bindings to target the right daemon)
    #[arg(long)]
    pub session: Option<String>,

    /// Subcommand
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

/// Subcommands for the vclaw CLI.
#[derive(clap::Subcommand, Debug)]
pub enum CliCommand {
    /// Reattach to an existing vclaw tmux session
    Attach,
    /// Authenticate with Anthropic (OAuth or API key)
    Auth {
        /// Set a direct API key instead of using OAuth
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Send a control command to the running daemon
    Ctl {
        /// Command: mute, interrupt, voice_toggle, status, conversation, quit
        command: String,
    },
}
