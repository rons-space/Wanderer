use grammers_client::client::{LoginToken, UpdatesConfiguration};
use grammers_client::message::InputMessage;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool};
use log::info;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::secret_store::SecretStore;
use crate::security::MasterKey;
use crate::session_store::EncryptedSession;

/// Error type for upload operations supporting rate limit detection
#[derive(Debug)]
pub enum UploadError {
    /// Telegram rate limit - wait for specified seconds
    RateLimit(u64),
    /// Generic error
    Other(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::RateLimit(secs) => write!(f, "RATE_LIMIT:{}", secs),
            UploadError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

/// Parse FLOOD_WAIT from error string and extract seconds
fn parse_flood_wait(err: &str) -> Option<u64> {
    // Grammers error format: "rpc error: FLOOD_WAIT (X)" or "FLOOD_WAIT_X"
    if err.contains("FLOOD_WAIT") {
        // Try to extract the number
        // Pattern 1: "FLOOD_WAIT (42)"
        if let Some(start) = err.find("FLOOD_WAIT (") {
            let rest = &err[start + 12..];
            if let Some(end) = rest.find(')') {
                if let Ok(secs) = rest[..end].trim().parse::<u64>() {
                    return Some(secs);
                }
            }
        }
        // Pattern 2: "FLOOD_WAIT_42"
        if let Some(idx) = err.find("FLOOD_WAIT_") {
            let rest = &err[idx + 11..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(secs) = num_str.parse::<u64>() {
                return Some(secs);
            }
        }
        // Pattern 3: Just "A]wait of X seconds"
        if let Some(idx) = err.find("wait of ") {
            let rest = &err[idx + 8..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(secs) = num_str.parse::<u64>() {
                return Some(secs);
            }
        }
        // Default: wait 60 seconds if we detect FLOOD but can't parse
        Some(60)
    } else {
        None
    }
}

pub struct TelegramService {
    client: Mutex<Option<Client>>,
    pending_token: Mutex<Option<LoginToken>>, // Store token between request_code and sign_in
    backend_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    update_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    credentials: Mutex<Option<(i32, String)>>,
}

impl TelegramService {
    pub fn new() -> Self {
        Self {
            client: Mutex::new(None),
            pending_token: Mutex::new(None),
            backend_handle: Mutex::new(None),
            update_handle: Mutex::new(None),
            credentials: Mutex::new(None),
        }
    }

    pub async fn set_credentials(&self, api_id: i32, api_hash: String) {
        *self.credentials.lock().await = Some((api_id, api_hash));
    }

    pub async fn clear_credentials(&self) {
        *self.credentials.lock().await = None;
    }

    pub async fn has_credentials(&self) -> bool {
        self.credentials.lock().await.is_some()
    }

    pub async fn is_connected(&self) -> bool {
        self.client.lock().await.is_some()
    }

    /// Connect, unsealing the stored session first.
    ///
    /// `master_key` is what the vault currently holds. On Windows it is unused, since
    /// DPAPI needs nothing unlocked; elsewhere it is the only thing that can open the
    /// session, so a locked or unencrypted library fails here with an explanation
    /// rather than falling back to storing the account key in the clear.
    pub async fn connect(
        &self,
        app_data_dir: PathBuf,
        master_key: Option<MasterKey>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (api_id, _api_hash) = self
            .credentials
            .lock()
            .await
            .clone()
            .ok_or("Telegram API credentials not configured")?;

        // 1. Initialize Session
        let store = SecretStore::for_session(master_key).map_err(|e| format!("{:#}", e))?;
        info!(
            "Connecting to Telegram with a session sealed by {}",
            store.describe()
        );
        let session =
            EncryptedSession::open(&app_data_dir, store).map_err(|e| format!("{:#}", e))?;

        // 2. Initialize SenderPool
        let session_handle = Arc::new(session);
        let sender_pool = SenderPool::new(session_handle, api_id);

        // 3. Initialize Client
        let client = Client::new(&sender_pool);

        // 4. Destructure pool to get runner and updates
        let SenderPool {
            runner,
            updates,
            handle: _handle, // Keep handle if we want to disconnect explicitly later
        } = sender_pool;

        // 5. Spawn runner (essential for network loop)
        let runner_handle = tokio::spawn(async move {
            let _ = runner.run().await;
        });

        // 6. Spawn update listener
        let mut update_stream = client.stream_updates(updates, UpdatesConfiguration::default());

        let updates_handle = tokio::spawn(async move {
            while let Ok(update) = update_stream.next().await {
                if let Update::NewMessage(message) = update {
                    info!("New message: {:?}", message.text());
                }
            }
        });

        *self.backend_handle.lock().await = Some(runner_handle);
        *self.update_handle.lock().await = Some(updates_handle);

        *self.client.lock().await = Some(client);
        info!("Connected to Telegram");
        Ok(())
    }

    pub async fn request_code(
        &self,
        phone: &str,
        app_data_dir: PathBuf,
        master_key: Option<MasterKey>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Reconnect if client is missing (e.g. after logout)
        let needs_connect = { self.client.lock().await.is_none() };

        if needs_connect {
            info!("Client not connected, re-initializing...");
            self.connect(app_data_dir, master_key).await?;
        }

        let client_guard = self.client.lock().await;
        let client = client_guard.as_ref().ok_or("Client not connected")?;
        let api_hash = self
            .credentials
            .lock()
            .await
            .as_ref()
            .map(|(_, hash)| hash.clone())
            .ok_or("Telegram API credentials not configured")?;

        // pass api_hash now required
        let token = client.request_login_code(phone, &api_hash).await?;
        *self.pending_token.lock().await = Some(token);
        Ok(())
    }

    pub async fn sign_in(&self, code: &str) -> Result<String, String> {
        let client_guard = self.client.lock().await;
        let client = client_guard
            .as_ref()
            .ok_or("Client not connected".to_string())?;

        let mut token_guard = self.pending_token.lock().await;
        let token = token_guard
            .take()
            .ok_or("No pending login request".to_string())?;

        match client.sign_in(&token, code).await {
            Ok(user) => {
                info!("Signed in as: {}", user.full_name());
                Ok(user.full_name())
            }
            // Error handling remains similar
            Err(e) => {
                // If failed, we might need to put the token back if it's retryable?
                // Grammers consumes token on use usually.
                // For now, assume we need to request code again on failure or handle specific errors.
                // Assuming token is consumed.
                Err(e.to_string())
            }
        }
    }

    pub async fn get_me(&self) -> Result<String, String> {
        let client_guard = self.client.lock().await;
        let client = client_guard
            .as_ref()
            .ok_or("Client not connected".to_string())?;

        match client.get_me().await {
            Ok(me) => Ok(me.full_name()),
            Err(e) => Err(e.to_string()),
        }
    }

    #[allow(dead_code)]
    pub async fn is_authorized(&self) -> bool {
        let client_guard = self.client.lock().await;
        if let Some(client) = client_guard.as_ref() {
            client.is_authorized().await.unwrap_or(false)
        } else {
            false
        }
    }
    pub async fn upload_file(&self, path: &str) -> Result<(), String> {
        let client_guard = self.client.lock().await;
        // Check connection
        let client = client_guard.as_ref().ok_or("Client not connected")?;

        // Upload logic
        // We reuse the client instance
        let uploaded_file = client.upload_file(path).await.map_err(|e| e.to_string())?;

        // Send to self (Saved Messages)
        let me = client.get_me().await.map_err(|e| e.to_string())?;

        let message = InputMessage::new()
            .text("Uploaded via Wander(er)")
            .file(uploaded_file);

        // Ensure we convert me to a PeerRef
        let peer = me.to_ref().ok_or("Could not get peer reference")?;

        client
            .send_message(peer, message)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Upload a file with progress callback
    /// Returns the Telegram message ID on success, or UploadError::RateLimit if rate limited
    pub async fn upload_file_with_progress<F>(
        &self,
        path: &str,
        on_progress: F,
    ) -> Result<i32, UploadError>
    where
        F: Fn(u64, u64, f64) + Send + Sync + 'static,
    {
        use crate::progress_stream::ProgressStream;
        use std::sync::Arc;
        use tokio::fs::File;
        use tokio::io::BufReader;

        let client_guard = self.client.lock().await;
        let client = client_guard
            .as_ref()
            .ok_or_else(|| UploadError::Other("Client not connected".to_string()))?;

        // Get file metadata
        let file = File::open(path)
            .await
            .map_err(|e| UploadError::Other(e.to_string()))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|e| UploadError::Other(e.to_string()))?;
        let total_bytes = metadata.len();
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        // Create progress-wrapped stream
        let callback = Arc::new(on_progress);
        let reader = BufReader::new(file);
        let mut progress_stream = ProgressStream::new(reader, total_bytes, callback);

        // Upload using stream - check for rate limit errors
        let uploaded_file = match client
            .upload_stream(&mut progress_stream, total_bytes as usize, file_name)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                let err_str = e.to_string();
                if let Some(secs) = parse_flood_wait(&err_str) {
                    return Err(UploadError::RateLimit(secs));
                }
                return Err(UploadError::Other(err_str));
            }
        };

        // Send to self (Saved Messages)
        let me = client
            .get_me()
            .await
            .map_err(|e| UploadError::Other(e.to_string()))?;
        let message = grammers_client::message::InputMessage::new()
            .text("Uploaded via Wander(er)")
            .file(uploaded_file);
        let peer = me
            .to_ref()
            .ok_or_else(|| UploadError::Other("Could not get peer reference".to_string()))?;

        // send_message can also rate limit
        match client.send_message(peer, message).await {
            Ok(sent_msg) => Ok(sent_msg.id()),
            Err(e) => {
                let err_str = e.to_string();
                if let Some(secs) = parse_flood_wait(&err_str) {
                    return Err(UploadError::RateLimit(secs));
                }
                Err(UploadError::Other(err_str))
            }
        }
    }

    pub async fn get_history(
        &self,
        _offset_id: i32,
        limit: usize,
    ) -> Result<Vec<grammers_client::message::Message>, String> {
        let client_guard = self.client.lock().await;
        let client = client_guard.as_ref().ok_or("Client not connected")?;

        let me = client.get_me().await.map_err(|e| e.to_string())?;
        let peer = me.to_ref().ok_or("Could not get peer error")?;

        // Grammers `iter_messages` returns an async iterator
        let mut messages = Vec::new();
        let mut row_iter = client.iter_messages(peer).limit(limit);

        while let Some(msg) = row_iter.next().await.map_err(|e| e.to_string())? {
            messages.push(msg);
        }

        Ok(messages)
    }

    pub async fn download_file(
        &self,
        message: &grammers_client::message::Message,
        path: &str,
    ) -> Result<(), String> {
        let client_guard = self.client.lock().await;
        let client = client_guard.as_ref().ok_or("Client not connected")?;

        // Check if message has media
        if let Some(media) = message.media() {
            client
                .download_media(&media, path)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        } else {
            Err("Message has no media".to_string())
        }
    }

    /// Delete messages from Saved Messages by message IDs
    /// Note: telegram_media_id is stored as String but Telegram uses i32 message IDs
    pub async fn delete_messages(&self, message_ids: &[i32]) -> Result<usize, String> {
        use grammers_client::tl;

        if message_ids.is_empty() {
            return Ok(0);
        }

        log::info!(
            "Telegram: Attempting to delete {} messages with IDs: {:?}",
            message_ids.len(),
            message_ids
        );

        let client_guard = self.client.lock().await;
        let client = client_guard.as_ref().ok_or("Client not connected")?;

        // For Saved Messages (self-chat), we use messages::DeleteMessages with revoke=true
        // This works for private chats including Saved Messages
        let request = tl::functions::messages::DeleteMessages {
            revoke: true,
            id: message_ids.to_vec(),
        };

        let result = client.invoke(&request).await.map_err(|e| {
            log::error!("Telegram delete_messages failed: {}", e);
            format!("Failed to delete messages: {}", e)
        })?;

        // AffectedMessages contains the pts_count indicating how many were deleted
        match result {
            tl::enums::messages::AffectedMessages::Messages(affected) => {
                let deleted_count = affected.pts_count as usize;
                log::info!(
                    "Telegram: Deleted {} messages (requested: {}, pts_count: {})",
                    deleted_count,
                    message_ids.len(),
                    affected.pts_count
                );
                Ok(deleted_count)
            }
        }
    }

    /// Download a file by message ID
    /// Fetches the message from saved messages and downloads its media to the specified path
    pub async fn download_by_message_id(&self, message_id: i32, path: &str) -> Result<(), String> {
        let client_guard = self.client.lock().await;
        let client = client_guard.as_ref().ok_or("Client not connected")?;

        // Get the "me" user for Saved Messages
        let me = client.get_me().await.map_err(|e| e.to_string())?;
        let peer = me.to_ref().ok_or("Could not get peer")?;

        // Iterate through messages to find the one with matching ID
        // We start from message_id + 1 and limit to 10 to find the message efficiently
        let mut iter = client
            .iter_messages(peer)
            .offset_id(message_id + 1)
            .limit(10);

        while let Some(msg) = iter.next().await.map_err(|e| e.to_string())? {
            if msg.id() == message_id {
                if let Some(media) = msg.media() {
                    client
                        .download_media(&media, path)
                        .await
                        .map_err(|e| e.to_string())?;
                    return Ok(());
                } else {
                    return Err("Message has no media".to_string());
                }
            }
        }

        Err(format!("Message with ID {} not found", message_id))
    }

    pub async fn logout(&self, app_data_dir: PathBuf) -> Result<(), String> {
        // 1. Graceful Sign Out
        {
            let mut client_guard = self.client.lock().await;
            if let Some(client) = client_guard.as_ref() {
                info!("Attempting graceful sign out...");
                match client.sign_out().await {
                    Ok(_) => info!("Signed out from Telegram successfully"),
                    Err(e) => log::error!("Failed to sign out gracefully: {}", e),
                }
            }
            // 2. Disconnect (Drop Client) - inside the same lock or re-acquire?
            // Better to keep it locked or just set to None immediately.
            *client_guard = None;
        }
        info!("Client disconnected");

        // 3. Abort background tasks (Wait until after sign out so network is available)
        if let Some(handle) = self.backend_handle.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = self.update_handle.lock().await.take() {
            handle.abort();
        }

        // 4. Delete the session, sealed or legacy plaintext.
        //
        // Retried, because the runner task above is aborted rather than joined and
        // Windows refuses to unlink a file another handle still holds. Logout that
        // leaves the account key on disk is the one outcome worth retrying for.
        for attempt in 0..5 {
            match EncryptedSession::remove(&app_data_dir) {
                Ok(()) => {
                    info!("Deleted the stored Telegram session");
                    break;
                }
                Err(e) if attempt == 4 => {
                    return Err(format!(
                        "Failed to delete the Telegram session after retries: {:#}",
                        e
                    ));
                }
                Err(e) => {
                    log::warn!(
                        "Failed to delete the Telegram session (attempt {}): {:#}. Retrying...",
                        attempt + 1,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
        }

        Ok(())
    }
}
