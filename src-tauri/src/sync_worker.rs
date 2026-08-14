use crate::database::Database;
use crate::media_utils;
use crate::security::{self, RuntimeState};
use crate::telegram::TelegramService;
use log::{debug, error, info, warn};
use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct SyncWorker {
    db: Arc<Database>,
    telegram: Arc<TelegramService>,
    backup_path: String,
    app_handle: AppHandle,
    security_runtime: Arc<Mutex<RuntimeState>>,
}

impl SyncWorker {
    pub fn new(
        db: Arc<Database>,
        telegram: Arc<TelegramService>,
        backup_path: String,
        app_handle: AppHandle,
        security_runtime: Arc<Mutex<RuntimeState>>,
    ) -> Self {
        Self {
            db,
            telegram,
            backup_path,
            app_handle,
            security_runtime,
        }
    }

    pub async fn run(&self, cancel: CancellationToken) {
        info!("SyncWorker: Started.");
        loop {
            // Check for cancellation
            if cancel.is_cancelled() {
                info!("SyncWorker received shutdown signal");
                break;
            }

            if let Err(e) = self.sync_once().await {
                error!("SyncWorker: Error in sync loop: {}", e);
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    }

    async fn sync_once(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Fail closed: an unreadable bundle must not be read as "plaintext is
        // fine", so the cycle is skipped and retried rather than downgraded.
        let encrypted_mode = match crate::encryption_required(&self.db) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "SyncWorker: cannot determine encryption state, skipping cycle: {}",
                    e
                );
                return Ok(());
            }
        };
        let master_key = if encrypted_mode {
            let maybe = self.security_runtime.lock().await.master_key.clone();
            if maybe.is_none() {
                debug!("SyncWorker: encryption enabled but vault locked; waiting for unlock.");
                return Ok(());
            }
            maybe
        } else {
            None
        };

        if !self.telegram.has_credentials().await {
            return Ok(());
        }

        if !self.telegram.is_authorized().await {
            debug!("SyncWorker: Telegram not authorized yet; skipping sync cycle.");
            return Ok(());
        }

        debug!("SyncWorker: Checking for new messages...");
        let messages = self.telegram.get_history(0, 20).await?;

        for msg in messages {
            if let Some(_media) = msg.media() {
                let msg_id = msg.id();
                // Naive extension guess, ideally usage mime from media
                let mime_type = match &_media {
                    grammers_client::media::Media::Photo(_) => "image/jpeg",
                    grammers_client::media::Media::Document(doc) => {
                        doc.mime_type().unwrap_or("application/octet-stream")
                    }
                    _ => "application/octet-stream",
                };

                // Force jpg for photos to avoid .jfif issues and ensure Watcher/AI support
                let extension = if mime_type == "image/jpeg" {
                    "jpg"
                } else {
                    mime_guess::get_mime_extensions_str(mime_type)
                        .and_then(|exts| exts.first())
                        .unwrap_or(&"bin")
                };

                let filename = format!("tg_{}.{}", msg_id, extension);
                let final_path_buf = std::path::Path::new(&self.backup_path).join(&filename);

                // Check if this file is marked as cloud-only in the database
                // If so, we should NOT download it again (user explicitly removed local copy)
                let tg_id_str = msg_id.to_string();
                match self.db.is_cloud_only_by_telegram_id(&tg_id_str) {
                    Ok(true) => {
                        debug!(
                            "SyncWorker: Skipping re-download of cloud-only media: {}",
                            filename
                        );
                        continue;
                    }
                    Err(e) => {
                        error!(
                            "SyncWorker: Failed to check cloud-only status for {}: {}",
                            filename, e
                        );
                        // Continue anyway to be safe? Or skip?
                        // Start conservatively: continue with download attempts if DB check fails might be safer than missing data,
                        // but if DB is broken, maybe we shouldn't spam.
                        // Let's log error and proceed to normal existence check.
                    }
                    Ok(false) => {}
                }

                if !final_path_buf.exists() {
                    info!("SyncWorker: Downloading new file {:?}", filename);

                    let temp_filename = format!("tg_{}.{}.tmp", msg_id, extension);
                    let temp_path_buf =
                        std::path::Path::new(&self.backup_path).join(&temp_filename);

                    // Download to temp
                    if let Err(e) = self
                        .telegram
                        .download_file(&msg, temp_path_buf.to_str().unwrap())
                        .await
                    {
                        error!("SyncWorker: Failed to download: {}", e);
                        // Clean up temp if exists
                        let _ = fs::remove_file(&temp_path_buf);
                        continue;
                    }

                    info!(
                        "SyncWorker: Downloaded to temp {:?}. Processing...",
                        temp_filename
                    );

                    let processing_path = if encrypted_mode {
                        let decrypt_tmp = std::path::Path::new(&self.backup_path)
                            .join(format!("tg_{}.{}.dec.tmp", msg_id, extension));
                        // `Unknown`, not `Encrypted`: an encrypted library still holds
                        // blobs uploaded before encryption was turned on, until the
                        // migration reaches them, and sync has no per-media row here to
                        // tell the two apart.
                        // Whole-file crypto on a media blob, so off the runtime.
                        let decrypt_src = temp_path_buf.clone();
                        let decrypt_dst = decrypt_tmp.clone();
                        let decrypt_key = master_key.clone();
                        let decrypted = tokio::task::spawn_blocking(move || {
                            security::decrypt_file_if_needed(
                                &decrypt_src,
                                &decrypt_dst,
                                decrypt_key.as_ref(),
                                security::Expect::Unknown,
                            )
                        })
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("Decrypt task failed: {}", e)));

                        match decrypted {
                            Ok(_) => {
                                let _ = fs::remove_file(&temp_path_buf);
                                decrypt_tmp
                            }
                            Err(e) => {
                                error!(
                                    "SyncWorker: Failed to decrypt synced payload {:?}: {}",
                                    temp_filename, e
                                );
                                let _ = fs::remove_file(&temp_path_buf);
                                continue;
                            }
                        }
                    } else {
                        temp_path_buf.clone()
                    };

                    // Process (Hash, Thumb, DB Insert for FINAL path), then Rename
                    if let Err(e) = self
                        .process_and_finalize_download(&processing_path, &final_path_buf, msg_id)
                        .await
                    {
                        error!(
                            "SyncWorker: Failed to process downloaded file {:?}: {}",
                            filename, e
                        );
                        // Cleanup temp on failure
                        let _ = fs::remove_file(&processing_path);
                    }
                } else {
                    // File exists locally. Ensure DB has the Telegram ID.
                    // Blake3 over the whole file, which for a video is gigabytes.
                    let hash_path = final_path_buf.clone();
                    let hashed = tokio::task::spawn_blocking(move || {
                        media_utils::hash_file_streaming(&hash_path)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(std::io::Error::other(format!("Hash task failed: {}", e)))
                    });

                    match hashed {
                        Ok(hash) => {
                            match self.db.media_exists_by_hash(&hash) {
                                Ok(true) => {
                                    // Exists in DB. Update media ID if needed.
                                    let tg_id_str = msg_id.to_string();
                                    if let Err(e) = self.db.update_telegram_id(&hash, &tg_id_str) {
                                        error!(
                                            "SyncWorker: Failed to update telegram ID for {:?}: {}",
                                            filename, e
                                        );
                                    } else {
                                        info!("SyncWorker: Updated existing file DB entry with Telegram ID: {:?}", filename);
                                    }
                                }
                                Ok(false) => {
                                    info!("SyncWorker: Found existing file NOT in DB: {:?}. Importing...", filename);
                                    // Re-import (Generating thumb etc.)
                                    if let Err(e) = self
                                        .process_and_finalize_download(
                                            &final_path_buf,
                                            &final_path_buf, // Same path -> process_and_finalize skips rename
                                            msg_id,
                                        )
                                        .await
                                    {
                                        error!("SyncWorker: Failed to import existing file: {}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("SyncWorker: DB check failed for {:?}: {}", filename, e);
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "SyncWorker: Failed to hash existing file {:?}: {}",
                                filename, e
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn process_and_finalize_download(
        &self,
        temp_path: &std::path::Path,
        final_path: &std::path::Path,
        telegram_msg_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db_clone = self.db.clone();
        let app_handle_clone = self.app_handle.clone();
        let final_path_str = final_path.to_string_lossy().to_string();

        // 1. Hash (the temp file) using streaming hasher, off the runtime.
        let hash = {
            let path = temp_path.to_path_buf();
            tokio::task::spawn_blocking(move || media_utils::hash_file_streaming(&path)).await??
        };

        // 2. Check if hash exists in DB (Dedupe)
        // If it exists, we might still want to keep the file or delete it?
        // If it exists and is uploaded, we probably don't need this download.
        if db_clone.media_exists_by_hash(&hash)? {
            info!("SyncWorker: File hash exists in DB. Deleting temp and skipping.");
            // We need to verify cleanup logic.
            // Here we return Ok, so 'sync_once' thinks success.
            // But we haven't renamed. So temp file is left?
            // Let's delete it here to be safe.
            fs::remove_file(temp_path)?;
            return Ok(());
        }

        // 3. Thumbnails using shared utility
        let cache_dir = std::path::Path::new(&self.backup_path)
            .parent()
            .map(|p| p.join("cache"))
            .unwrap_or_else(|| std::path::PathBuf::from(".").join("cache"));

        let mut thumbnail_path =
            match media_utils::generate_thumbnail(temp_path, &cache_dir, &hash, 300).await {
                Ok(Some(thumb_path)) => Some(thumb_path.to_string_lossy().to_string()),
                Ok(None) => None,
                Err(e) => {
                    warn!("SyncWorker: Thumbnail failed: {}", e);
                    None
                }
            };

        // Fail closed: when the state is unknown, treat the thumbnail as
        // requiring encryption rather than leaving a plaintext copy at rest. The
        // branch below deletes the plaintext thumbnail when it cannot seal it, so
        // the worst case here is a missing thumbnail, which regenerates.
        let encrypted_mode = crate::encryption_required(&db_clone).unwrap_or_else(|e| {
            warn!(
                "SyncWorker: cannot determine encryption state, treating thumbnail as encrypted: {}",
                e
            );
            true
        });
        if encrypted_mode {
            if let Some(thumb_str) = thumbnail_path.clone() {
                let thumb = std::path::PathBuf::from(&thumb_str);
                let maybe_key = self.security_runtime.lock().await.master_key.clone();
                if let Some(key) = maybe_key {
                    let encrypted_thumb = thumb.with_extension("wbenc");
                    let encrypt_src = thumb.clone();
                    let encrypt_dst = encrypted_thumb.clone();
                    let encrypted = tokio::task::spawn_blocking(move || {
                        security::encrypt_file(&encrypt_src, &encrypt_dst, &key)
                    })
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("Encrypt task failed: {}", e)));

                    match encrypted {
                        Ok(_) => {
                            let _ = fs::remove_file(&thumb);
                            thumbnail_path = Some(encrypted_thumb.to_string_lossy().to_string());
                        }
                        Err(e) => {
                            warn!(
                                "SyncWorker: Failed thumbnail encryption for {:?}: {}",
                                thumb, e
                            );
                            let _ = fs::remove_file(&thumb);
                            thumbnail_path = None;
                        }
                    }
                } else {
                    let _ = fs::remove_file(&thumb);
                    thumbnail_path = None;
                }
            }
        }

        // 4. Mime
        let mime_type = mime_guess::from_path(temp_path)
            .first_or_octet_stream()
            .to_string();

        let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let uploaded_at = created_at; // Mark as uploaded now

        // Extract Metadata
        let metadata = if !mime_type.starts_with("video/") {
            Some(crate::metadata::extract_metadata(temp_path))
        } else {
            None
        };

        // 5. DB Insert (Use FINAL path)
        // Store telegram message ID for later deletion
        let tg_id_str = telegram_msg_id.to_string();
        db_clone.add_media_synced(
            &final_path_str,
            &hash,
            thumbnail_path.as_deref(),
            created_at,
            Some(&mime_type),
            uploaded_at,
            Some(&tg_id_str),
            metadata,
        )?;
        if encrypted_mode {
            let _ = db_clone.mark_media_encrypted_by_path(&final_path_str);
        }

        info!("SyncWorker: Registered synced file in DB. Renaming to final.");

        // 6. Finalize (Move generic -> specific)
        if temp_path != final_path {
            fs::rename(temp_path, final_path)?;
        }

        // 7. Emit Event
        let _ = app_handle_clone.emit("media-added", ());

        Ok(())
    }
}
