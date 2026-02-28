use vclaw::tts::ElevenLabsClient;

#[test]
fn test_tts_client_creation() {
    let client = ElevenLabsClient::new(
        "test-key".into(),
        "test-voice".into(),
        "eleven_turbo_v2".into(),
    );
    assert_eq!(client.voice_id(), "test-voice");
}

#[test]
fn test_tts_url_construction() {
    let client = ElevenLabsClient::new(
        "test-key".into(),
        "my-voice-id".into(),
        "eleven_turbo_v2".into(),
    );
    let url = client.streaming_url();
    assert!(url.contains("my-voice-id"));
    assert!(url.contains("text-to-speech"));
}
