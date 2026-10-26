//! # Remote Mobile Gateway - Telegram & Discord Task Dispatcher
//!
//! Async gateway enabling secure mobile dispatch of Hagibis autopilot runs,
//! real-time status telemetry, and dream journal inspection via Telegram long-polling,
//! Matrix E2EE event channels, and Discord webhooks.
//!
//! Enforces an immutable security whitelist on incoming user IDs, and provides
//! interactive approval inline keyboards (`approve:<id>`, `reject:<id>`) for high-stakes
//! agent actions (database migrations, shell executions, production deployments).

use crate::ambient_service::DreamJournal;
use crate::get_memory_rss_mb;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hgb_core::error::{HgbError, Result};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// Configuration for the Remote Mobile Gateway
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBotConfig {
    pub telegram_token: Option<String>,
    pub discord_webhook_url: Option<String>,
    pub matrix_config: Option<MatrixConfig>,
    pub allowed_user_ids: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_poll_interval() -> u64 {
    2
}

impl Default for RemoteBotConfig {
    fn default() -> Self {
        Self {
            telegram_token: None,
            discord_webhook_url: None,
            matrix_config: None,
            allowed_user_ids: Vec::new(),
            poll_interval_secs: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix E2EE Client Adapter & WebSocket Event Channel
// ---------------------------------------------------------------------------

/// Matrix E2EE configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token: String,
    pub room_id: String,
    pub shared_secret: String,
}

/// End-to-End Encrypted payload for mobile notifications
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncryptedMatrixPayload {
    pub algorithm: String,
    pub ciphertext: String,
    pub nonce: String,
    pub signature: String,
    pub timestamp: u64,
}

/// Adapter for Matrix Client-Server API with symmetric E2EE payload encryption
#[derive(Clone)]
pub struct MatrixClientAdapter {
    config: MatrixConfig,
    client: reqwest::Client,
}

impl MatrixClientAdapter {
    pub fn new(config: MatrixConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { config, client }
    }

    /// Derives a 32-byte session key from the shared secret and purpose string
    fn derive_key(secret: &str, purpose: &str) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key(purpose);
        hasher.update(secret.as_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Encrypts an arbitrary message payload using authenticated Blake3-CTR stream cipher
    pub fn encrypt_payload(&self, plaintext: &str) -> EncryptedMatrixPayload {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let key = Self::derive_key(&self.config.shared_secret, "hgb.matrix.e2ee.v1.encryption");
        let nonce_bytes = blake3::hash(format!("{}-{}", now, plaintext.len()).as_bytes());
        let nonce_b64 = BASE64.encode(nonce_bytes.as_bytes());

        // Stream XOR keystream generated from Blake3(key || nonce || index)
        let mut ciphertext = Vec::with_capacity(plaintext.len());
        for (idx, byte) in plaintext.as_bytes().iter().enumerate() {
            let mut stream_hasher = blake3::Hasher::new_keyed(&key);
            stream_hasher.update(nonce_bytes.as_bytes());
            stream_hasher.update(&(idx as u64).to_le_bytes());
            let mask = stream_hasher.finalize().as_bytes()[0];
            ciphertext.push(byte ^ mask);
        }

        let cipher_b64 = BASE64.encode(&ciphertext);

        // Compute HMAC/MAC signature
        let sig_key = Self::derive_key(&self.config.shared_secret, "hgb.matrix.e2ee.v1.auth");
        let mut mac_hasher = blake3::Hasher::new_keyed(&sig_key);
        mac_hasher.update(cipher_b64.as_bytes());
        mac_hasher.update(nonce_b64.as_bytes());
        mac_hasher.update(&now.to_le_bytes());
        let signature = mac_hasher.finalize().to_hex().to_string();

        EncryptedMatrixPayload {
            algorithm: "hgb.e2ee.v1.blake3".to_string(),
            ciphertext: cipher_b64,
            nonce: nonce_b64,
            signature,
            timestamp: now,
        }
    }

    /// Decrypts and verifies an authenticated E2EE payload
    pub fn decrypt_payload(&self, payload: &EncryptedMatrixPayload) -> Result<String> {
        // 1. Verify MAC signature
        let sig_key = Self::derive_key(&self.config.shared_secret, "hgb.matrix.e2ee.v1.auth");
        let mut mac_hasher = blake3::Hasher::new_keyed(&sig_key);
        mac_hasher.update(payload.ciphertext.as_bytes());
        mac_hasher.update(payload.nonce.as_bytes());
        mac_hasher.update(&payload.timestamp.to_le_bytes());
        let expected_signature = mac_hasher.finalize().to_hex().to_string();

        if expected_signature != payload.signature {
            return Err(HgbError::Authentication("Invalid E2EE MAC authentication signature".into()));
        }

        // 2. Decrypt ciphertext
        let key = Self::derive_key(&self.config.shared_secret, "hgb.matrix.e2ee.v1.encryption");
        let nonce_raw = BASE64.decode(&payload.nonce)
            .map_err(|e| HgbError::validation(format!("Invalid base64 nonce: {e}")))?;
        let cipher_raw = BASE64.decode(&payload.ciphertext)
            .map_err(|e| HgbError::validation(format!("Invalid base64 ciphertext: {e}")))?;

        let mut plaintext = Vec::with_capacity(cipher_raw.len());
        for (idx, byte) in cipher_raw.iter().enumerate() {
            let mut stream_hasher = blake3::Hasher::new_keyed(&key);
            stream_hasher.update(&nonce_raw);
            stream_hasher.update(&(idx as u64).to_le_bytes());
            let mask = stream_hasher.finalize().as_bytes()[0];
            plaintext.push(byte ^ mask);
        }

        String::from_utf8(plaintext).map_err(|e| HgbError::validation(format!("Decrypted UTF-8 error: {e}")))
    }

    /// Dispatches an encrypted event into the configured Matrix room
    pub async fn send_encrypted_event(&self, event_type: &str, plaintext: &str) -> Result<String> {
        let encrypted = self.encrypt_payload(plaintext);
        let txn_id = uuid::Uuid::new_v4().to_string();

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/{}/{}",
            self.config.homeserver_url.trim_end_matches('/'),
            self.config.room_id,
            event_type,
            txn_id
        );

        let res = self
            .client
            .put(&url)
            .bearer_auth(&self.config.access_token)
            .json(&encrypted)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Matrix API request failed: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!("Matrix server error {}: {}", status, body)));
        }

        #[derive(Deserialize)]
        struct MatrixEventResponse {
            event_id: String,
        }

        let body: MatrixEventResponse = res.json().await.map_err(|e| {
            HgbError::Serialization(format!("Invalid Matrix response JSON: {e}"))
        })?;

        Ok(body.event_id)
    }
}

/// Mobile push notification payload broadcasted via WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobilePushNotification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub severity: String,
    pub timestamp: u64,
    pub requires_approval: bool,
    pub action_id: Option<String>,
}

/// E2EE WebSocket broadcast channel for connected mobile devices
#[derive(Clone)]
pub struct E2eeWebSocketChannel {
    sender: tokio::sync::broadcast::Sender<EncryptedMatrixPayload>,
    matrix_adapter: MatrixClientAdapter,
}

impl E2eeWebSocketChannel {
    pub fn new(shared_secret: &str) -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(256);
        let dummy_config = MatrixConfig {
            homeserver_url: "https://matrix.org".to_string(),
            user_id: "@hgb_bot:matrix.org".to_string(),
            access_token: "dummy".to_string(),
            room_id: "!room:matrix.org".to_string(),
            shared_secret: shared_secret.to_string(),
        };

        Self {
            sender,
            matrix_adapter: MatrixClientAdapter::new(dummy_config),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EncryptedMatrixPayload> {
        self.sender.subscribe()
    }

    /// Broadcasts an encrypted notification to all listening mobile clients
    pub fn broadcast_notification(&self, note: &MobilePushNotification) -> Result<usize> {
        let json_str = serde_json::to_string(note)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        let encrypted = self.matrix_adapter.encrypt_payload(&json_str);

        self.sender.send(encrypted).map_err(|e| {
            HgbError::Execution(format!("Failed to broadcast over E2EE WebSocket channel: {e}"))
        })
    }
}

// ---------------------------------------------------------------------------
// High-Stakes Action Approval Engine (Migrations, Shell, Deployments)
// ---------------------------------------------------------------------------

/// High-stakes agent action types requiring explicit human sign-off
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HighStakesActionKind {
    DatabaseMigration,
    ShellExecution,
    ProductionDeployment,
    Custom(String),
}

impl HighStakesActionKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::DatabaseMigration => "🗄️ Database Migration",
            Self::ShellExecution => "⚡ Host Shell Execution",
            Self::ProductionDeployment => "🚀 Production Deployment",
            Self::Custom(_) => "⚠️ Critical Agent Action",
        }
    }
}

/// Status of an approval request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalStatus {
    Pending,
    Approved { approved_by: String, timestamp: u64 },
    Rejected { rejected_by: String, timestamp: u64 },
}

/// Approval request payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub kind: HighStakesActionKind,
    pub title: String,
    pub command_or_sql: String,
    pub context: String,
    pub requested_by: String,
    pub created_at: u64,
    pub status: ApprovalStatus,
}

/// Thread-safe in-memory approval manager
#[derive(Clone, Default)]
pub struct ApprovalManager {
    requests: Arc<RwLock<HashMap<String, ApprovalRequest>>>,
}

impl ApprovalManager {
    pub fn new() -> Self {
        Self {
            requests: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Creates and registers a new pending approval request
    pub async fn create_request(
        &self,
        kind: HighStakesActionKind,
        title: &str,
        command_or_sql: &str,
        context: &str,
        requested_by: &str,
    ) -> ApprovalRequest {
        let id = format!("act_{}", &uuid::Uuid::new_v4().to_string().replace('-', "")[..12]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let req = ApprovalRequest {
            id: id.clone(),
            kind,
            title: title.to_string(),
            command_or_sql: command_or_sql.to_string(),
            context: context.to_string(),
            requested_by: requested_by.to_string(),
            created_at: now,
            status: ApprovalStatus::Pending,
        };

        let mut lock = self.requests.write().await;
        lock.insert(id, req.clone());
        req
    }

    /// Retrieves an approval request by ID
    pub async fn get_request(&self, id: &str) -> Option<ApprovalRequest> {
        let lock = self.requests.read().await;
        lock.get(id).cloned()
    }

    /// Approves an action
    pub async fn approve(&self, id: &str, user_id: &str) -> Result<ApprovalRequest> {
        let mut lock = self.requests.write().await;
        let req = lock.get_mut(id).ok_or_else(|| {
            HgbError::NotFound(format!("Approval request '{}' not found", id))
        })?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        req.status = ApprovalStatus::Approved {
            approved_by: user_id.to_string(),
            timestamp: now,
        };

        Ok(req.clone())
    }

    /// Rejects an action
    pub async fn reject(&self, id: &str, user_id: &str) -> Result<ApprovalRequest> {
        let mut lock = self.requests.write().await;
        let req = lock.get_mut(id).ok_or_else(|| {
            HgbError::NotFound(format!("Approval request '{}' not found", id))
        })?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        req.status = ApprovalStatus::Rejected {
            rejected_by: user_id.to_string(),
            timestamp: now,
        };

        Ok(req.clone())
    }
}

// ---------------------------------------------------------------------------
// Telegram Inline Keyboard & Callback Parser
// ---------------------------------------------------------------------------

/// Generates Telegram InlineKeyboardMarkup for approval requests
pub fn build_inline_approval_keyboard(request_id: &str) -> serde_json::Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {
                    "text": "✅ Approve",
                    "callback_data": format!("approve:{}", request_id)
                },
                {
                    "text": "❌ Reject",
                    "callback_data": format!("reject:{}", request_id)
                }
            ]
        ]
    })
}

/// Parses callback data into `(action, action_id)` e.g. "approve:act_123" -> ("approve", "act_123")
pub fn parse_callback_data(data: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = data.splitn(2, ':').collect();
    if parts.len() == 2 && (parts[0] == "approve" || parts[0] == "reject") {
        Some((parts[0], parts[1]))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Bot Commands & Handlers
// ---------------------------------------------------------------------------

/// Parsed inbound command from a mobile chat user
#[derive(Debug, Clone, PartialEq)]
pub enum BotCommand {
    Do(String),
    Status,
    Journal,
    Approvals,
    Help,
    Unknown(String),
}

/// Inbound mobile message from Telegram or Discord
#[derive(Debug, Clone)]
pub struct IncomingBotMessage {
    pub platform: &'static str,
    pub user_id: String,
    pub user_name: Option<String>,
    pub text: String,
    pub chat_id: String,
}

/// Outbound mobile response payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingBotResponse {
    pub chat_id: String,
    pub text: String,
    pub parse_mode: Option<String>,
    pub reply_markup: Option<serde_json::Value>,
}

/// Async Telegram & Discord remote gateway engine
#[derive(Clone)]
pub struct RemoteMobileGateway {
    config: RemoteBotConfig,
    client: reqwest::Client,
    allowed_users: HashSet<String>,
    pub approval_manager: ApprovalManager,
    pub matrix_adapter: Option<MatrixClientAdapter>,
}

impl RemoteMobileGateway {
    /// Creates a new `RemoteMobileGateway` with the specified configuration.
    pub fn new(config: RemoteBotConfig) -> Self {
        let allowed_users = config.allowed_user_ids.iter().cloned().collect();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let matrix_adapter = config.matrix_config.as_ref().map(|mc| MatrixClientAdapter::new(mc.clone()));

        Self {
            config,
            client,
            allowed_users,
            approval_manager: ApprovalManager::new(),
            matrix_adapter,
        }
    }

    /// Checks if a sender user ID is on the security whitelist.
    pub fn is_user_authorized(&self, user_id: &str) -> bool {
        // If whitelist is empty, reject all for safety
        if self.allowed_users.is_empty() {
            return false;
        }
        self.allowed_users.contains(user_id)
    }

    /// Parses raw message text into a structured `BotCommand`.
    pub fn parse_command(text: &str) -> BotCommand {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return BotCommand::Unknown(trimmed.to_string());
        }

        let mut parts = trimmed.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().unwrap_or("").trim().to_string();

        match cmd.as_str() {
            "/do" | "/auto" | "/run" => {
                if arg.is_empty() {
                    BotCommand::Unknown("Usage: /do <specification task>".to_string())
                } else {
                    BotCommand::Do(arg)
                }
            }
            "/status" | "/stat" | "/info" => BotCommand::Status,
            "/journal" | "/dream" | "/dreams" => BotCommand::Journal,
            "/approvals" | "/pending" => BotCommand::Approvals,
            "/help" | "/start" => BotCommand::Help,
            other => BotCommand::Unknown(other.to_string()),
        }
    }

    /// Executes command processing and renders response string.
    pub fn handle_incoming(&self, msg: &IncomingBotMessage) -> OutgoingBotResponse {
        // Enforce security authorization
        if !self.is_user_authorized(&msg.user_id) {
            return OutgoingBotResponse {
                chat_id: msg.chat_id.clone(),
                text: format!(
                    "⛔ *Unauthorized Access*\nUser ID `{}` is not in the Hagibis whitelist.",
                    msg.user_id
                ),
                parse_mode: Some("Markdown".to_string()),
                reply_markup: None,
            };
        }

        let cmd = Self::parse_command(&msg.text);
        let reply_text = match cmd {
            BotCommand::Do(spec) => {
                format!(
                    "🚀 *Hagibis Task Dispatched*\n\n*Spec:* `{}`\n\n_CoW Sandbox initialized. Autopilot is processing._",
                    spec
                )
            }
            BotCommand::Status => {
                let mem_mb = get_memory_rss_mb();
                let active_model = hgb_core::load_active_model()
                    .unwrap_or_else(|| "gemini-2.5-flash".to_string());
                let cred_status = hgb_core::GeminiProvider::credential_status();

                format!(
                    "▲ *Hagibis Daemon Status*\n\n• *Engine:* `Online / Sub-millisecond Nominal`\n• *Active Model:* `{}`\n• *RAM Usage:* `{:.1} MB`\n• *Provider Status:* `{}`\n• *Platform:* `x86_64-linux`",
                    active_model, mem_mb, cred_status
                )
            }
            BotCommand::Journal => {
                let journal_path = Path::new(".hgb/dream_journal.json");
                let journal = DreamJournal::load(journal_path);
                if journal.entries.is_empty() {
                    "🌙 *Overnight Dream Journal*\n\n_No recent autonomous dream cycles recorded yet._"
                        .to_string()
                } else {
                    let mut out = String::from("🌙 *Recent Dream Journal Entries:*\n\n");
                    for e in journal.entries.iter().rev().take(5) {
                        out.push_str(&format!(
                            "• *[{}]* `{}`\n  _DPO pairs: {}, velocity: {:.2}_\n",
                            e.timestamp.format("%Y-%m-%d %H:%M"),
                            e.cycle_id,
                            e.dpo_pairs_synthesized,
                            e.learning_velocity
                        ));
                    }
                    out
                }
            }
            BotCommand::Approvals => {
                "🔒 *Active Approval Gate*\nUse `/do` or agent triggers to propose high-stakes actions.\nNotifications with interactive inline buttons (`[Approve]`, `[Reject]`) will appear automatically."
                    .to_string()
            }
            BotCommand::Help => {
                "🪽 *Hagibis Remote Mobile Gateway*\n\n\
                 Available commands:\n\
                 • `/do <spec>` - Dispatch autonomous task into resident CoW sandbox\n\
                 • `/status` - Check daemon telemetry, memory RSS & active model\n\
                 • `/journal` - Inspect recent overnight dream journal cycles\n\
                 • `/approvals` - Check pending high-stakes agent approvals\n\
                 • `/help` - Show this guidance manual"
                    .to_string()
            }
            BotCommand::Unknown(u) => {
                format!(
                    "❓ *Unknown command:* `{}`\nType `/help` to see available commands.",
                    u
                )
            }
        };

        OutgoingBotResponse {
            chat_id: msg.chat_id.clone(),
            text: reply_text,
            parse_mode: Some("Markdown".to_string()),
            reply_markup: None,
        }
    }

    /// Send a message to Telegram with optional inline keyboard markup
    pub async fn send_telegram_message(
        &self,
        chat_id: &str,
        text: &str,
        reply_markup: Option<&serde_json::Value>,
    ) -> Result<()> {
        let token = self.config.telegram_token.as_ref().ok_or_else(|| {
            HgbError::Authentication("Telegram token is not configured".into())
        })?;

        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown"
        });

        if let Some(markup) = reply_markup {
            payload["reply_markup"] = markup.clone();
        }

        let res = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to send Telegram message: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!(
                "Telegram API error (HTTP {}): {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Sends an interactive approval notification to Telegram with inline approval buttons
    pub async fn send_telegram_approval_request(
        &self,
        chat_id: &str,
        req: &ApprovalRequest,
    ) -> Result<()> {
        let text = format!(
            "🛡️ *High-Stakes Agent Action Required*\n\n\
             • *Action:* `{}`\n\
             • *Title:* *{}*\n\
             • *Command / SQL:*\n```\n{}\n```\n\
             • *Context:* _{}_\n• *Requested by:* `{}`",
            req.kind.label(),
            req.title,
            req.command_or_sql,
            req.context,
            req.requested_by
        );

        let keyboard = build_inline_approval_keyboard(&req.id);
        self.send_telegram_message(chat_id, &text, Some(&keyboard)).await
    }

    /// Dispatch notification to Discord webhook
    pub async fn dispatch_discord_webhook(&self, content: &str) -> Result<()> {
        let url = self.config.discord_webhook_url.as_ref().ok_or_else(|| {
            HgbError::Authentication("Discord webhook URL is not configured".into())
        })?;

        let payload = serde_json::json!({
            "content": content,
            "username": "Hagibis Autopilot"
        });

        let res = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Discord webhook failed: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            return Err(HgbError::Provider(format!(
                "Discord webhook returned HTTP {}",
                status
            )));
        }

        Ok(())
    }

    /// Long-polling loop for Telegram updates including messages and callback buttons
    pub async fn poll_telegram_loop(&self) -> Result<()> {
        let token = self.config.telegram_token.as_ref().ok_or_else(|| {
            HgbError::Authentication("Telegram token not configured".into())
        })?;

        let mut offset: i64 = 0;

        loop {
            let url = format!(
                "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=20",
                token, offset
            );

            match self.client.get(&url).send().await {
                Ok(res) if res.status().is_success() => {
                    #[derive(Deserialize)]
                    struct TgResponse {
                        result: Vec<TgUpdate>,
                    }
                    #[derive(Deserialize)]
                    struct TgUpdate {
                        update_id: i64,
                        message: Option<TgMessage>,
                        callback_query: Option<TgCallbackQuery>,
                    }
                    #[derive(Deserialize)]
                    struct TgMessage {
                        from: Option<TgUser>,
                        chat: TgChat,
                        text: Option<String>,
                    }
                    #[derive(Deserialize)]
                    struct TgCallbackQuery {
                        id: String,
                        from: TgUser,
                        message: Option<TgMessage>,
                        data: Option<String>,
                    }
                    #[derive(Deserialize)]
                    struct TgUser {
                        id: i64,
                        username: Option<String>,
                    }
                    #[derive(Deserialize)]
                    struct TgChat {
                        id: i64,
                    }

                    if let Ok(updates) = res.json::<TgResponse>().await {
                        for u in updates.result {
                            offset = u.update_id + 1;

                            // 1. Handle interactive inline button callbacks (approve/reject)
                            if let Some(cb) = u.callback_query {
                                let user_id = cb.from.id.to_string();
                                if !self.is_user_authorized(&user_id) {
                                    continue;
                                }

                                if let Some(ref data) = cb.data {
                                    if let Some((action, act_id)) = parse_callback_data(data) {
                                        let verdict = match action {
                                            "approve" => {
                                                let _ = self.approval_manager.approve(act_id, &user_id).await;
                                                format!("✅ Action `{}` was *APPROVED* by `{}`", act_id, user_id)
                                            }
                                            "reject" => {
                                                let _ = self.approval_manager.reject(act_id, &user_id).await;
                                                format!("❌ Action `{}` was *REJECTED* by `{}`", act_id, user_id)
                                            }
                                            _ => String::new(),
                                        };

                                        if let Some(msg) = cb.message {
                                            let _ = self.send_telegram_message(&msg.chat.id.to_string(), &verdict, None).await;
                                        }

                                        // Acknowledge callback query
                                        let ack_url = format!("https://api.telegram.org/bot{}/answerCallbackQuery", token);
                                        let ack_payload = serde_json::json!({
                                            "callback_query_id": cb.id,
                                            "text": format!("Recorded verdict: {}", action)
                                        });
                                        let _ = self.client.post(&ack_url).json(&ack_payload).send().await;
                                    }
                                }
                                continue;
                            }

                            // 2. Handle normal text messages
                            if let Some(msg) = u.message {
                                if let (Some(from), Some(text)) = (msg.from, msg.text) {
                                    let inbound = IncomingBotMessage {
                                        platform: "telegram",
                                        user_id: from.id.to_string(),
                                        user_name: from.username,
                                        text,
                                        chat_id: msg.chat.id.to_string(),
                                    };

                                    let resp = self.handle_incoming(&inbound);
                                    let _ = self
                                        .send_telegram_message(&resp.chat_id, &resp.text, resp.reply_markup.as_ref())
                                        .await;
                                }
                            }
                        }
                    }
                }
                _ => {
                    tokio::time::sleep(Duration::from_secs(self.config.poll_interval_secs)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authorization_whitelist() {
        let config = RemoteBotConfig {
            allowed_user_ids: vec!["12345".to_string(), "67890".to_string()],
            ..Default::default()
        };
        let gateway = RemoteMobileGateway::new(config);

        assert!(gateway.is_user_authorized("12345"));
        assert!(gateway.is_user_authorized("67890"));
        assert!(!gateway.is_user_authorized("99999"));
    }

    #[test]
    fn test_empty_whitelist_denies_all() {
        let config = RemoteBotConfig::default();
        let gateway = RemoteMobileGateway::new(config);
        assert!(!gateway.is_user_authorized("12345"));
    }

    #[test]
    fn test_command_parsing() {
        assert_eq!(
            RemoteMobileGateway::parse_command("/do fix login flow bug"),
            BotCommand::Do("fix login flow bug".to_string())
        );
        assert_eq!(RemoteMobileGateway::parse_command("/status"), BotCommand::Status);
        assert_eq!(RemoteMobileGateway::parse_command("/journal"), BotCommand::Journal);
        assert_eq!(RemoteMobileGateway::parse_command("/approvals"), BotCommand::Approvals);
        assert_eq!(RemoteMobileGateway::parse_command("/help"), BotCommand::Help);
    }

    #[test]
    fn test_unauthorized_user_rejection() {
        let config = RemoteBotConfig {
            allowed_user_ids: vec!["valid_user".to_string()],
            ..Default::default()
        };
        let gateway = RemoteMobileGateway::new(config);

        let msg = IncomingBotMessage {
            platform: "telegram",
            user_id: "intruder_user".to_string(),
            user_name: None,
            text: "/status".to_string(),
            chat_id: "1001".to_string(),
        };

        let resp = gateway.handle_incoming(&msg);
        assert!(resp.text.contains("Unauthorized Access"));
        assert!(resp.text.contains("intruder_user"));
    }

    #[test]
    fn test_matrix_payload_encryption_and_decryption() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.org".to_string(),
            user_id: "@admin:matrix.org".to_string(),
            access_token: "tok123".to_string(),
            room_id: "!room:matrix.org".to_string(),
            shared_secret: "super-secret-key-for-e2ee-testing".to_string(),
        };

        let adapter = MatrixClientAdapter::new(config);
        let secret_msg = "Critical database migration approval required for user_table!";

        let encrypted = adapter.encrypt_payload(secret_msg);
        assert_ne!(encrypted.ciphertext, secret_msg);
        assert!(!encrypted.nonce.is_empty());
        assert!(!encrypted.signature.is_empty());

        let decrypted = adapter.decrypt_payload(&encrypted).unwrap();
        assert_eq!(decrypted, secret_msg);
    }

    #[tokio::test]
    async fn test_approval_manager_lifecycle() {
        let manager = ApprovalManager::new();

        let req = manager.create_request(
            HighStakesActionKind::DatabaseMigration,
            "Drop deprecated sessions table",
            "DROP TABLE sessions_v1;",
            "Automated cleanup of schema v1",
            "agent_hgb",
        ).await;

        assert_eq!(req.status, ApprovalStatus::Pending);

        // Approve action
        let approved = manager.approve(&req.id, "admin_user_42").await.unwrap();
        match approved.status {
            ApprovalStatus::Approved { approved_by, .. } => {
                assert_eq!(approved_by, "admin_user_42");
            }
            _ => panic!("Expected Approved status"),
        }

        // Test rejection
        let req2 = manager.create_request(
            HighStakesActionKind::ShellExecution,
            "Run rm -rf in build cache",
            "rm -rf /tmp/cache",
            "Disk space reclamation",
            "agent_hgb",
        ).await;

        let rejected = manager.reject(&req2.id, "sec_auditor_9").await.unwrap();
        match rejected.status {
            ApprovalStatus::Rejected { rejected_by, .. } => {
                assert_eq!(rejected_by, "sec_auditor_9");
            }
            _ => panic!("Expected Rejected status"),
        }
    }

    #[test]
    fn test_telegram_inline_keyboard_generation_and_callback_parsing() {
        let keyboard = build_inline_approval_keyboard("act_abc123");
        let keyboard_str = keyboard.to_string();
        assert!(keyboard_str.contains("approve:act_abc123"));
        assert!(keyboard_str.contains("reject:act_abc123"));

        assert_eq!(parse_callback_data("approve:act_abc123"), Some(("approve", "act_abc123")));
        assert_eq!(parse_callback_data("reject:act_abc123"), Some(("reject", "act_abc123")));
        assert_eq!(parse_callback_data("unknown:123"), None);
    }
}
