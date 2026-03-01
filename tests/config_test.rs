use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_default_config() {
    let config = vclaw::config::Config::default();
    assert_eq!(config.voice.mode, vclaw::config::VoiceMode::PushToTalk);
    assert_eq!(config.voice.whisper_model, "base");
    assert_eq!(config.brain.model, "claude-sonnet-4-6");
}

#[test]
fn test_config_from_toml() {
    let toml_str = r#"
[voice]
mode = "push_to_talk"
whisper_model = "small"

[tts]
voice_id = "test-voice"

[brain]
model = "claude-sonnet-4-6"
max_context_lines = 100
"#;
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(toml_str.as_bytes()).unwrap();
    let config = vclaw::config::Config::from_file(f.path()).unwrap();
    assert_eq!(config.voice.mode, vclaw::config::VoiceMode::PushToTalk);
    assert_eq!(config.voice.whisper_model, "small");
    assert_eq!(config.brain.model, "claude-sonnet-4-6");
    assert_eq!(config.brain.max_context_lines, 100);
}
