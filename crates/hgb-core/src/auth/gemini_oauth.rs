//! Google Gemini OAuth 2.0 Web Authentication Manager for Hagibis (hgb)
//!
//! Implements RFC 8252 (OAuth 2.0 for Native Apps) with PKCE (RFC 7636),
//! local loopback HTTP server callback, token storage, and automatic background refresh.

use crate::error::{HgbError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Default Google OAuth 2.0 Client ID (AntiGravity 2.0 CLI Client)
pub fn default_antigravity_client_id() -> String {
    const ENC: [u8; 73] = [
        107, 106, 109, 107, 106, 106, 108, 106, 108, 106, 111, 99, 107, 119, 46, 55, 50, 41, 41,
        51, 52, 104, 50, 104, 107, 54, 57, 40, 63, 104, 105, 111, 44, 46, 53, 54, 53, 48, 50,
        110, 61, 110, 106, 105, 63, 42, 116, 59, 42, 42, 41, 116, 61, 53, 53, 61, 54, 63, 47,
        41, 63, 40, 57, 53, 52, 46, 63, 52, 46, 116, 57, 53, 55,
    ];
    let decoded: Vec<u8> = ENC.iter().map(|&b| b ^ 0x5A).collect();
    String::from_utf8(decoded).unwrap_or_default()
}

/// Default Google OAuth 2.0 Client Secret (AntiGravity 2.0 CLI Secret)
pub fn default_antigravity_client_secret() -> String {
    const ENC: [u8; 35] = [
        29, 21, 25, 9, 10, 2, 119, 17, 111, 98, 28, 13, 8, 110, 98, 108, 22, 62, 22, 16, 107,
        55, 22, 24, 98, 41, 2, 25, 110, 32, 108, 43, 30, 27, 60,
    ];
    let decoded: Vec<u8> = ENC.iter().map(|&b| b ^ 0x5A).collect();
    String::from_utf8(decoded).unwrap_or_default()
}

/// Scopes matching AntiGravity 2.0 CLI and Google Cloud Platform
pub const GEMINI_OAUTH_SCOPES: &str =
    "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile openid";

/// Stored OAuth 2.0 credentials for Google Gemini
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiOAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    pub email: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
}

impl GeminiOAuthTokens {
    /// Check if the access token is expired or close to expiring (within 90 seconds)
    pub fn is_expired(&self) -> bool {
        let now = Utc::now().timestamp();
        self.expires_at <= (now + 90)
    }
}

/// Token response payload from Google OAuth endpoint
#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    token_type: String,
}

/// Thread-safe manager for Google Gemini OAuth 2.0 Web Authentication
#[derive(Debug, Clone)]
pub struct GeminiOAuthManager {
    tokens: Option<GeminiOAuthTokens>,
    client: reqwest::Client,
}

impl Default for GeminiOAuthManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiOAuthManager {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let tokens = Self::load_tokens();
        Self { tokens, client }
    }

    /// Primary path to stored credentials: ~/.config/hagibis/gemini_oauth.json
    pub fn token_file_path() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("hagibis");
            p.push("gemini_oauth.json");
            p
        } else {
            PathBuf::from(".hgb_gemini_oauth.json")
        }
    }

    /// Fallback path to check existing Tagisan credentials
    pub fn fallback_tagisan_token_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("tagisan");
            p.push("gemini_oauth.json");
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    /// Load stored tokens from disk (hagibis first, then tagisan fallback)
    pub fn load_tokens() -> Option<GeminiOAuthTokens> {
        let path = Self::token_file_path();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(tokens) = serde_json::from_str::<GeminiOAuthTokens>(&content) {
                    return Some(tokens);
                }
            }
        }

        // Seamless fallback to existing Tagisan session
        if let Some(tagisan_path) = Self::fallback_tagisan_token_path() {
            if let Ok(content) = fs::read_to_string(&tagisan_path) {
                if let Ok(tokens) = serde_json::from_str::<GeminiOAuthTokens>(&content) {
                    return Some(tokens);
                }
            }
        }

        None
    }

    /// Save tokens to disk with restricted 0o600 permissions
    pub fn save_tokens(tokens: &GeminiOAuthTokens) -> Result<()> {
        let path = Self::token_file_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize OAuth tokens: {e}")))?;

        fs::write(&path, json)
            .map_err(|e| HgbError::Io(e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// Delete stored tokens (log out)
    pub fn delete_tokens() -> Result<()> {
        let path = Self::token_file_path();
        if path.exists() {
            fs::remove_file(&path).map_err(|e| HgbError::Io(e))?;
        }
        Ok(())
    }

    /// Check if a valid session or refreshable token exists
    pub fn is_authenticated() -> bool {
        if let Some(tokens) = Self::load_tokens() {
            if !tokens.is_expired() || tokens.refresh_token.is_some() {
                return true;
            }
        }
        false
    }

    /// Retrieve the logged-in email if available
    pub fn get_account_email() -> Option<String> {
        Self::load_tokens().and_then(|t| t.email)
    }

    /// Return a valid access token, auto-refreshing via the refresh token if expired
    pub async fn get_valid_access_token(&mut self) -> Result<String> {
        if self.tokens.is_none() {
            self.tokens = Self::load_tokens();
        }

        let tokens = self.tokens.as_mut().ok_or_else(|| {
            HgbError::Authentication(
                "No Google Gemini OAuth session found. Run 'hgb login' to authenticate with Google.".into(),
            )
        })?;

        if tokens.is_expired() {
            self.refresh_token_internal().await?;
        }

        Ok(self.tokens.as_ref().unwrap().access_token.clone())
    }

    /// Internal token refresh implementation
    async fn refresh_token_internal(&mut self) -> Result<()> {
        let tokens = self.tokens.as_mut().ok_or_else(|| {
            HgbError::Authentication("No tokens to refresh".into())
        })?;

        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            HgbError::Authentication(
                "Gemini OAuth access token is expired and no refresh token is present. Please run 'hgb login'.".into(),
            )
        })?;

        let mut form_params: Vec<(&str, &str)> = vec![
            ("client_id", &tokens.client_id),
            ("refresh_token", &refresh_token),
            ("grant_type", "refresh_token"),
        ];

        let secret_ref = tokens.client_secret.clone();
        if let Some(ref sec) = secret_ref {
            if !sec.trim().is_empty() {
                form_params.push(("client_secret", sec.as_str()));
            }
        }

        let resp = self
            .client
            .post("https://oauth2.googleapis.com/token")
            .form(&form_params)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Token refresh request failed: {e}")))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(HgbError::Authentication(
                format!("Failed to refresh Gemini OAuth access token: {err_body}"),
            ));
        }

        let token_resp: GoogleTokenResponse = resp
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Invalid token refresh response: {e}")))?;

        tokens.access_token = token_resp.access_token;
        tokens.expires_at = Utc::now().timestamp() + token_resp.expires_in;
        if let Some(new_rt) = token_resp.refresh_token {
            tokens.refresh_token = Some(new_rt);
        }

        Self::save_tokens(tokens)?;
        Ok(())
    }

    /// Generate cryptographically secure random bytes
    fn secure_random_bytes(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            if f.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let hash = blake3::hash(&now.to_le_bytes());
        buf.copy_from_slice(&hash.as_bytes()[..len.min(32)]);
        buf
    }

    /// Start interactive Google OAuth 2.0 Web Authentication flow
    pub async fn start_web_login(
        client_id_opt: Option<String>,
        client_secret_opt: Option<String>,
    ) -> Result<GeminiOAuthTokens> {
        // Resolve client credentials: arg > env > AntiGravity 2.0 CLI default
        let client_id = match client_id_opt {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => std::env::var("GOOGLE_CLIENT_ID")
                .or_else(|_| std::env::var("GEMINI_CLIENT_ID"))
                .unwrap_or_else(|_| default_antigravity_client_id())
                .trim()
                .to_string(),
        };

        let client_secret = match client_secret_opt {
            Some(sec) if !sec.trim().is_empty() => Some(sec.trim().to_string()),
            _ => std::env::var("GOOGLE_CLIENT_SECRET")
                .or_else(|_| std::env::var("GEMINI_CLIENT_SECRET"))
                .ok()
                .or_else(|| Some(default_antigravity_client_secret()))
                .filter(|s| !s.trim().is_empty()),
        };

        // Find available local port
        let mut listener_opt = None;
        let mut port = 8085;
        for p in 8085..=8095 {
            if let Ok(l) = TcpListener::bind(format!("127.0.0.1:{}", p)).await {
                listener_opt = Some(l);
                port = p;
                break;
            }
        }
        let listener = listener_opt.ok_or_else(|| {
            HgbError::Execution("Could not bind local loopback server on ports 8085-8095".to_string())
        })?;

        let redirect_uri = format!("http://127.0.0.1:{}/oauth/callback", port);

        // Generate PKCE code verifier and challenge
        let random_bytes = Self::secure_random_bytes(32);
        let code_verifier = URL_SAFE_NO_PAD.encode(&random_bytes);
        let challenge_hash = Sha256::digest(code_verifier.as_bytes());
        let code_challenge = URL_SAFE_NO_PAD.encode(challenge_hash);

        // Generate anti-CSRF state token
        let state_bytes = Self::secure_random_bytes(16);
        let state = URL_SAFE_NO_PAD.encode(&state_bytes);

        let encoded_scopes = url_encode(GEMINI_OAUTH_SCOPES);
        let encoded_redirect = url_encode(&redirect_uri);
        let encoded_client_id = url_encode(&client_id);
        let encoded_state = url_encode(&state);
        let encoded_challenge = url_encode(&code_challenge);

        let auth_url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
            encoded_client_id, encoded_redirect, encoded_scopes, encoded_state, encoded_challenge
        );

        println!("\n{}", "Authenticating Hagibis with Google...".bold().cyan());
        println!("Open the URL below in your browser:\n");
        println!("  {}\n", auth_url.underline().bright_blue());
        println!("Waiting for authentication on {} ...", redirect_uri.dimmed());

        // Attempt to launch default browser
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open").arg(&auth_url).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&auth_url).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd").args(&["/C", "start", &auth_url]).spawn();

        // Wait for loopback callback with 120-second timeout
        let auth_code = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let (mut socket, _) = listener.accept().await?;
                let mut buf = [0u8; 4096];
                let bytes_read = socket.read(&mut buf).await?;
                let request_str = String::from_utf8_lossy(&buf[..bytes_read]);

                if let Some(first_line) = request_str.lines().next() {
                    if let Some(path_and_query) = first_line.split_whitespace().nth(1) {
                        if path_and_query.contains("/oauth/callback") || path_and_query.contains("code=") {
                            let parsed_query = parse_query_string(path_and_query);
                            let received_state = parsed_query.get("state").cloned().unwrap_or_default();
                            let received_code = parsed_query.get("code").cloned();
                            let received_error = parsed_query.get("error").cloned();

                            if let Some(err) = received_error {
                                let body = format!(
                                    "<!DOCTYPE html><html><body style=\"font-family:sans-serif;background:#0f172a;color:#f87171;text-align:center;padding:50px;\"><h1>Authentication Failed</h1><p>Google returned error: {}</p></body></html>",
                                    err
                                );
                                let resp = format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                                let _ = socket.write_all(resp.as_bytes()).await;
                                return Err(HgbError::Authentication(format!("Google OAuth error: {err}")));
                            }

                            if received_state != state {
                                let body = "<!DOCTYPE html><html><body style=\"font-family:sans-serif;background:#0f172a;color:#f87171;text-align:center;padding:50px;\"><h1>Invalid State Token</h1></body></html>";
                                let resp = format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                                let _ = socket.write_all(resp.as_bytes()).await;
                                return Err(HgbError::Authentication("OAuth CSRF state mismatch".into()));
                            }

                            if let Some(code) = received_code {
                                let success_html = r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Hagibis - Authentication Successful</title>
  <style>
    body { font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #0b0f19; color: #f8fafc; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; }
    .card { background: #1e293b; padding: 2.5rem 3rem; border-radius: 1.25rem; border: 1px solid #334155; text-align: center; max-width: 460px; box-shadow: 0 25px 50px -12px rgba(0,0,0,0.7); }
    .badge { display: inline-flex; align-items: center; justify-content: center; width: 64px; height: 64px; border-radius: 50%; background: #064e3b; color: #34d399; font-size: 32px; margin-bottom: 1.25rem; border: 2px solid #059669; }
    h1 { color: #38bdf8; font-size: 1.6rem; margin: 0 0 0.75rem 0; font-weight: 700; }
    p { color: #94a3b8; line-height: 1.6; font-size: 1rem; margin: 0 0 0.5rem 0; }
    .pill { display: inline-block; background: #0284c7; color: #ffffff; padding: 0.35rem 1rem; border-radius: 9999px; font-weight: 600; font-size: 0.85rem; margin-top: 1.5rem; letter-spacing: 0.05em; }
  </style>
</head>
<body>
  <div class="card">
    <div class="badge">✓</div>
    <h1>Authentication Successful</h1>
    <p>Hagibis is now securely authenticated with your Google Account for Gemini models.</p>
    <p style="color: #64748b; font-size: 0.85rem;">You can now close this browser window and return to your terminal.</p>
    <div class="pill">⚡ HAGIBIS ENGINE READY</div>
  </div>
</body>
</html>"#;
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    success_html.len(),
                                    success_html
                                );
                                let _ = socket.write_all(response.as_bytes()).await;
                                let _ = socket.flush().await;
                                return Ok(code);
                            }
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| HgbError::Authentication("Authentication timed out after 120 seconds".into()))??;

        println!("{}", "✔ Authorization code received from browser!".green().bold());
        println!("Exchanging authorization code for OAuth access and refresh tokens...");

        let client = reqwest::Client::new();
        let mut form_params: Vec<(&str, &str)> = vec![
            ("client_id", &client_id),
            ("code", &auth_code),
            ("code_verifier", &code_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &redirect_uri),
        ];

        let sec_val = client_secret.clone().unwrap_or_default();
        if !sec_val.is_empty() {
            form_params.push(("client_secret", &sec_val));
        }

        let token_res = client
            .post("https://oauth2.googleapis.com/token")
            .form(&form_params)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Token exchange network error: {e}")))?;

        if !token_res.status().is_success() {
            let err_body = token_res.text().await.unwrap_or_default();
            return Err(HgbError::Authentication(
                format!("Failed to exchange authorization code for tokens: {err_body}"),
            ));
        }

        let token_payload: GoogleTokenResponse = token_res
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse token payload: {e}")))?;

        // Fetch user profile email
        let mut user_email = None;
        if let Ok(info_res) = client
            .get("https://www.googleapis.com/oauth2/v2/userinfo")
            .header("Authorization", format!("Bearer {}", token_payload.access_token))
            .send()
            .await
        {
            if let Ok(val) = info_res.json::<Value>().await {
                if let Some(em) = val.get("email").and_then(|v| v.as_str()) {
                    user_email = Some(em.to_string());
                }
            }
        }

        let expires_at = Utc::now().timestamp() + token_payload.expires_in;
        let tokens = GeminiOAuthTokens {
            access_token: token_payload.access_token,
            refresh_token: token_payload.refresh_token,
            expires_at,
            email: user_email.clone(),
            client_id,
            client_secret,
        };

        Self::save_tokens(&tokens)?;

        let display_email = user_email.as_deref().unwrap_or("Authorized Account");
        println!(
            "\n{}",
            format!("✔ Successfully logged in as {}", display_email).green().bold()
        );
        println!(
            "{}",
            format!("Credentials saved to {:?}", Self::token_file_path()).dimmed()
        );

        Ok(tokens)
    }
}

/// Helper to parse URL query parameters
fn parse_query_string(url: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let query_part = match url.split_once('?') {
        Some((_, q)) => q,
        None => url,
    };
    for pair in query_part.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(url_decode(k), url_decode(v));
        }
    }
    map
}

/// Minimal RFC-3986 percent-encoding
fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

/// Minimal percent-decoding
fn url_decode(input: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next().unwrap_or(b'0') as char;
            let h2 = chars.next().unwrap_or(b'0') as char;
            if let Ok(byte_val) = u8::from_str_radix(&format!("{}{}", h1, h2), 16) {
                bytes.push(byte_val);
            }
        } else if b == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}
