use crate::database::Database;
use crate::media_utils;
use crate::security::{self, RuntimeState};
use crate::telegram::{TelegramService, UploadError};
use log::{error, info, warn};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

/// Artificial delay between successful uploads to avoid rate limiting (seconds)
const UPLOAD_COOLDOWN_SECS: u64 = 2;

/// Event payload for upload status changes
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadEvent {
    pub id: i64,
    pub file_path: String,
    pub status: String,
    pub error: Option<String>,
}

/// Event payload for upload progress updates
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgressEvent {
    pub id: i64,
    pub file_path: String,
    pub bytes_uploaded: u64,
    pub total_bytes: u64,
    pub speed_bps: f64,
    pub eta_seconds: u64,
    pub percent: f64,
}

/// Event payload for rate limiting
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitEvent {
    pub id: i64,
    pub file_path: String,
    pub wait_seconds: u64,
}

pub async fn run_upload_worker(
    db: Arc<Database>,
    telegram: Arc<TelegramService>,
    security_runtime: Arc<Mutex<RuntimeState>>,
    app_handle: AppHandle,
    cancel: CancellationToken,
) {
    info!("Starting upload worker...");

    loop {
        // Check for cancellation
        if cancel.is_cancelled() {
            info!("Upload worker received shutdown signal");
            break;
        }

        // 1. Fetch next pending item
        match db.get_next_pending_item() {
            Ok(Some(item)) => {
                info!(
                    "Processing pending upload: {} (ID: {})",
                    item.file_path, item.id
                );

                // 2. Mark as uploading
                if let Err(e) = db.update_queue_status(item.id, "uploading", None) {
                    error!("Failed to update status to uploading: {}", e);
                }

                // Defensive dedupe at worker time: if current bytes already match an uploaded
                // media hash, skip re-upload. This protects against transient watcher races.
                if let Ok(hash) = media_utils::hash_file_streaming(std::path::Path::new(&item.file_path)) {
                    if let Ok(true) = db.is_media_uploaded(&hash) {
                        info!(
                            "Skipping upload for {} (hash already uploaded)",
                            item.file_path
                        );
                        let _ = db.update_queue_status(item.id, "completed", None);
                        let _ = app_handle.emit(
                            "upload-completed",
                            UploadEvent {
                                id: item.id,
                                file_path: item.file_path.clone(),
                                status: "completed".to_string(),
                                error: None,
                            },
                        );
                        continue;
                    }
                }

                // Emit upload-started event
                let _ = app_handle.emit(
                    "upload-started",
                    UploadEvent {
                        id: item.id,
                        file_path: item.file_path.clone(),
                        status: "uploading".to_string(),
                        error: None,
                    },
                );

                // 3. Attempt upload with progress
                let progress_handle = app_handle.clone();
                let progress_id = item.id;
                let progress_path = item.file_path.clone();
                // Fail closed: if the security bundle cannot be read we do not
                // know whether this file must be encrypted, and uploading the
                // plaintext original to Telegram is not an acceptable default.
                let should_encrypt = match crate::encryption_required(&db) {
                    Ok(v) => v,
                    Err(e) => {
                        error!(
                            "Cannot determine encryption state, deferring upload of {}: {}",
                            item.file_path, e
                        );
                        let _ = db.update_queue_status(item.id, "pending", None);
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
                let mut upload_path = item.file_path.clone();
                let mut encrypted_temp: Option<PathBuf> = None;

                if should_encrypt {
                    let maybe_key = security_runtime.lock().await.master_key;
                    let key = match maybe_key {
                        Some(k) => k,
                        None => {
                            warn!(
                                "Skipping upload {} because encryption vault is locked",
                                item.file_path
                            );
                            let _ = db.update_queue_status(item.id, "pending", None);
                            sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                    };

                    let temp_dir = std::env::temp_dir().join("wanderer-encrypted-uploads");
                    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
                        let err_msg = format!("Failed to create temp encrypted upload dir: {}", e);
                        let _ = db.update_queue_status(item.id, "failed", Some(&err_msg));
                        continue;
                    }

                    let temp_path = temp_dir.join(format!("upload_{}_enc.wbenc", item.id));
                    match security::encrypt_file(
                        std::path::Path::new(&item.file_path),
                        &temp_path,
                        &key,
                    ) {
                        Ok(_) => {
                            upload_path = temp_path.to_string_lossy().to_string();
                            encrypted_temp = Some(temp_path);
                        }
                        Err(e) => {
                            let err_msg = format!("Failed to encrypt file before upload: {}", e);
                            error!("{}", err_msg);
                            let _ = db.update_queue_status(item.id, "failed", Some(&err_msg));
                            continue;
                        }
                    }

                    // Last line of defence before bytes leave the machine: the
                    // artifact must actually be a WBENC1 file. Without this,
                    // any future bug in the branch above sends plaintext and
                    // nothing notices.
                    let is_sealed = security::is_encrypted_file(std::path::Path::new(&upload_path))
                        .unwrap_or(false);
                    if !is_sealed {
                        let err_msg =
                            "Refusing to upload: encrypted artifact failed its magic check"
                                .to_string();
                        error!("{} ({})", err_msg, upload_path);
                        if let Some(tmp) = encrypted_temp.take() {
                            let _ = std::fs::remove_file(tmp);
                        }
                        let _ = db.update_queue_status(item.id, "failed", Some(&err_msg));
                        continue;
                    }
                }

                let upload_result = telegram
                    .upload_file_with_progress(&upload_path, move |bytes, total, speed| {
                        let eta = if speed > 0.0 {
                            ((total - bytes) as f64 / speed) as u64
                        } else {
                            0
                        };
                        let percent = if total > 0 {
                            (bytes as f64 / total as f64) * 100.0
                        } else {
                            0.0
                        };

                        let _ = progress_handle.emit(
                            "upload-progress",
                            UploadProgressEvent {
                                id: progress_id,
                                file_path: progress_path.clone(),
                                bytes_uploaded: bytes,
                                total_bytes: total,
                                speed_bps: speed,
                                eta_seconds: eta,
                                percent,
                            },
                        );
                    })
                    .await;

                if let Some(temp) = encrypted_temp {
                    let _ = std::fs::remove_file(temp);
                }

                match upload_result {
                    Ok(telegram_msg_id) => {
                        info!(
                            "Successfully uploaded: {} (Telegram ID: {})",
                            item.file_path, telegram_msg_id
                        );

                        // Store the Telegram message ID for later deletion
                        if let Err(e) = db.update_telegram_id_by_path(
                            &item.file_path,
                            &telegram_msg_id.to_string(),
                        ) {
                            error!("Failed to store Telegram message ID: {}", e);
                        }

                        // 4. Success: Update queue and media
                        if let Err(e) = db.update_queue_status(item.id, "completed", None) {
                            error!("Failed to mark queue item completed: {}", e);
                        }

                        if let Err(e) = db.mark_media_uploaded_by_path(&item.file_path) {
                            error!("Failed to mark media uploaded: {}", e);
                        }
                        if should_encrypt {
                            if let Err(e) = db.mark_media_encrypted_by_path(&item.file_path) {
                                error!("Failed to mark media encrypted: {}", e);
                            }
                        }

                        // Emit upload-completed event
                        let _ = app_handle.emit(
                            "upload-completed",
                            UploadEvent {
                                id: item.id,
                                file_path: item.file_path.clone(),
                                status: "completed".to_string(),
                                error: None,
                            },
                        );

                        // Artificial cooldown to avoid rate limiting
                        info!(
                            "Cooldown: waiting {}s before next upload",
                            UPLOAD_COOLDOWN_SECS
                        );
                        sleep(Duration::from_secs(UPLOAD_COOLDOWN_SECS)).await;
                    }
                    Err(UploadError::RateLimit(wait_secs)) => {
                        warn!("Rate limited by Telegram! Waiting {} seconds...", wait_secs);

                        // Emit rate-limit event for UI
                        let _ = app_handle.emit(
                            "upload-rate-limited",
                            RateLimitEvent {
                                id: item.id,
                                file_path: item.file_path.clone(),
                                wait_seconds: wait_secs,
                            },
                        );

                        // Update status to rate_limited
                        let _ = db.update_queue_status(item.id, "rate_limited", None);

                        // Wait for the required duration
                        sleep(Duration::from_secs(wait_secs)).await;

                        // Reset status back to pending for retry
                        let _ = db.update_queue_status(item.id, "pending", None);
                        continue;
                    }
                    Err(UploadError::Other(e)) => {
                        // Check for connection error
                        if e.contains("Client not connected") {
                            error!("Worker waiting for Telegram connection...");
                            sleep(Duration::from_secs(5)).await;
                            // Reset status back to pending for retry
                            let _ = db.update_queue_status(item.id, "pending", None);
                            continue;
                        }

                        error!("Upload failed for {}: {}", item.file_path, e);

                        // 5. Failure: Update queue with error
                        if let Err(db_err) = db.update_queue_status(item.id, "failed", Some(&e)) {
                            error!("Failed to log upload error to db: {}", db_err);
                        }

                        // Emit upload-failed event
                        let _ = app_handle.emit(
                            "upload-failed",
                            UploadEvent {
                                id: item.id,
                                file_path: item.file_path.clone(),
                                status: "failed".to_string(),
                                error: Some(e),
                            },
                        );
                    }
                }
            }
            Ok(None) => {
                // Queue empty
                sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!("Database error fetching queue: {}", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
