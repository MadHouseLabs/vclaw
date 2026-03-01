//! ElevenLabs streaming text-to-speech client.
//!
//! Sends text to the ElevenLabs TTS API and returns a byte stream of MP3
//! audio chunks for low-latency playback.

use anyhow::Result;
use reqwest::Client;

/// Client for the ElevenLabs text-to-speech streaming API.
pub struct ElevenLabsClient {
    client: Client,
    api_key: String,
    voice_id: String,
    model_id: String,
}

impl ElevenLabsClient {
    pub fn new(api_key: String, voice_id: String, model_id: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            voice_id,
            model_id,
        }
    }

    /// Whether an API key is configured (TTS is optional).
    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub fn streaming_url(&self) -> String {
        format!(
            "https://api.elevenlabs.io/v1/text-to-speech/{}/stream",
            self.voice_id
        )
    }

    /// Stream audio bytes as they arrive for low-latency playback
    pub async fn speak_streaming(
        &self,
        text: &str,
    ) -> Result<impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>> {
        let url = self.streaming_url();
        let body = serde_json::json!({
            "text": text,
            "model_id": self.model_id,
            "voice_settings": {
                "stability": 0.5,
                "similarity_boost": 0.75
            }
        });

        let response = self.client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "audio/mpeg")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("ElevenLabs TTS failed ({}): {}", status, body);
        }

        Ok(response.bytes_stream())
    }
}
