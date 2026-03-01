use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use vclaw::auth::{generate_pkce, Credentials};

#[test]
fn test_credentials_save_load_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");

    let creds = Credentials {
        auth_type: Some("api_key".into()),
        api_key: Some("sk-ant-test-key-123".into()),
        ..Default::default()
    };

    // Save
    let content = toml::to_string_pretty(&creds).unwrap();
    std::fs::write(&path, &content).unwrap();

    // Load
    let loaded_content = std::fs::read_to_string(&path).unwrap();
    let loaded: Credentials = toml::from_str(&loaded_content).unwrap();

    assert_eq!(loaded.auth_type.as_deref(), Some("api_key"));
    assert_eq!(loaded.api_key.as_deref(), Some("sk-ant-test-key-123"));
    assert!(loaded.access_token.is_none());
    assert!(loaded.refresh_token.is_none());
    assert!(loaded.token_expires.is_none());
}

#[test]
fn test_credentials_save_load_oauth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.toml");

    let creds = Credentials {
        auth_type: Some("oauth".into()),
        access_token: Some("access-tok-abc".into()),
        refresh_token: Some("refresh-tok-xyz".into()),
        token_expires: Some(1700000000),
        ..Default::default()
    };

    let content = toml::to_string_pretty(&creds).unwrap();
    std::fs::write(&path, &content).unwrap();

    let loaded_content = std::fs::read_to_string(&path).unwrap();
    let loaded: Credentials = toml::from_str(&loaded_content).unwrap();

    assert_eq!(loaded.auth_type.as_deref(), Some("oauth"));
    assert_eq!(loaded.access_token.as_deref(), Some("access-tok-abc"));
    assert_eq!(loaded.refresh_token.as_deref(), Some("refresh-tok-xyz"));
    assert_eq!(loaded.token_expires, Some(1700000000));
    assert!(loaded.api_key.is_none());
}

#[tokio::test]
async fn test_get_token_prefers_env_var() {
    // Set the env var for this test
    let key = "test-env-key-for-auth-test";
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", key);
    }

    let result = vclaw::auth::get_valid_token().await;

    // Clean up before asserting
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    let (token, is_oauth) = result.unwrap();
    assert_eq!(token, key);
    assert!(!is_oauth);
}

#[test]
fn test_pkce_generation() {
    let (verifier, challenge) = generate_pkce();

    // Verifier should be base64url encoding of 32 random bytes
    // 32 bytes -> 43 base64url chars (no padding)
    assert_eq!(verifier.len(), 43);

    // Challenge should be base64url encoding of SHA256 hash (32 bytes -> 43 chars)
    assert_eq!(challenge.len(), 43);

    // Both should be valid base64url (decode without error)
    assert!(URL_SAFE_NO_PAD.decode(&verifier).is_ok());
    assert!(URL_SAFE_NO_PAD.decode(&challenge).is_ok());

    // Verifier and challenge should differ
    assert_ne!(verifier, challenge);

    // Two calls should produce different values
    let (verifier2, _) = generate_pkce();
    assert_ne!(verifier, verifier2);
}
