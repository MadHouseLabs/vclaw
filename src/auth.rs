use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const CLIENT_ID: &str = "REDACTED_OAUTH_CLIENT_ID";
const SCOPE: &str = "org:create_api_key user:profile user:inference";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Credentials {
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_expires: Option<i64>,
}

fn credentials_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("Could not determine config directory")?
        .join("vclaw");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("credentials.toml"))
}

fn load_credentials() -> Result<Credentials> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(Credentials::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let creds: Credentials = toml::from_str(&content)?;
    Ok(creds)
}

fn save_credentials(creds: &Credentials) -> Result<()> {
    let path = credentials_path()?;
    let content = toml::to_string_pretty(creds)?;
    std::fs::write(&path, content)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub fn generate_pkce() -> (String, String) {
    let bytes: Vec<u8> = (0..32).map(|_| rand::rng().random::<u8>()).collect();
    let verifier = URL_SAFE_NO_PAD.encode(&bytes);
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
    (verifier, challenge)
}

/// Store a direct API key.
pub fn store_api_key(key: &str) -> Result<()> {
    let creds = Credentials {
        auth_type: Some("api_key".into()),
        api_key: Some(key.into()),
        ..Default::default()
    };
    save_credentials(&creds)
}

/// Start OAuth flow: generate PKCE, build auth URL, open browser.
/// Returns the PKCE verifier (needed for complete_oauth).
pub fn start_oauth() -> Result<String> {
    let (verifier, challenge) = generate_pkce();

    let auth_url = url::Url::parse_with_params(
        "https://claude.ai/oauth/authorize",
        &[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", verifier.as_str()),
        ],
    )
    .context("Failed to build auth URL")?
    .to_string();

    println!("Opening browser for authentication...");
    println!("If the browser doesn't open, visit:\n{}\n", auth_url);

    // Open browser (macOS)
    let _ = std::process::Command::new("open")
        .arg(&auth_url)
        .spawn();

    Ok(verifier)
}

/// Complete OAuth flow: exchange code for tokens.
/// code_string format: "<code>#<state>" where state is the PKCE verifier.
pub async fn complete_oauth(code_string: &str) -> Result<()> {
    let (code, verifier) = code_string
        .split_once('#')
        .map(|(c, s)| (c.trim().to_string(), s.trim().to_string()))
        .context("Invalid code format. Expected '<code>#<state>'")?;

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
    }

    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "code": code,
            "state": verifier,
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .context("Token exchange request failed")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed: {}", body);
    }

    let t: TokenResponse = resp.json().await.context("Failed to parse token response")?;

    let creds = Credentials {
        auth_type: Some("oauth".into()),
        access_token: Some(t.access_token),
        refresh_token: Some(t.refresh_token),
        token_expires: Some(now_secs() + t.expires_in),
        ..Default::default()
    };
    save_credentials(&creds)
}

/// Get a valid token, refreshing if needed.
/// Returns (token, is_oauth).
/// Env var ANTHROPIC_API_KEY always takes priority.
pub async fn get_valid_token() -> Result<(String, bool)> {
    // Env var override
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        return Ok((key, false));
    }

    let creds = load_credentials().context("Failed to load credentials")?;
    let auth_type = creds.auth_type.as_deref()
        .context("Not authenticated")?;

    match auth_type {
        "api_key" => {
            let key = creds.api_key.context("No API key stored")?;
            Ok((key, false))
        }
        "oauth" => {
            let access = creds.access_token.context("No access token stored")?;
            let expires = creds.token_expires.context("No token expiry stored")?;

            // Return existing token if still valid (60s buffer)
            if now_secs() + 60 < expires {
                return Ok((access, true));
            }

            // Refresh
            let refresh = creds.refresh_token.context("No refresh token stored")?;
            refresh_token(&refresh).await
        }
        other => anyhow::bail!("Unknown auth type: {}", other),
    }
}

async fn refresh_token(refresh: &str) -> Result<(String, bool)> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
    }

    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .context("Token refresh request failed")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token refresh failed: {}", body);
    }

    let t: TokenResponse = resp.json().await.context("Failed to parse refresh response")?;

    let creds = Credentials {
        auth_type: Some("oauth".into()),
        access_token: Some(t.access_token.clone()),
        refresh_token: Some(t.refresh_token),
        token_expires: Some(now_secs() + t.expires_in),
        ..Default::default()
    };
    save_credentials(&creds)?;

    Ok((t.access_token, true))
}

// Test helpers — allow overriding credential path
#[cfg(test)]
pub mod test_helpers {
    use super::*;
    use std::path::Path;

    pub fn save_credentials_to(path: &Path, creds: &Credentials) -> Result<()> {
        let content = toml::to_string_pretty(creds)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn load_credentials_from(path: &Path) -> Result<Credentials> {
        let content = std::fs::read_to_string(path)?;
        let creds: Credentials = toml::from_str(&content)?;
        Ok(creds)
    }
}
