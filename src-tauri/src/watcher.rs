use crate::database::Database;
use crate::media_utils;
use crate::security::{self, RuntimeState};
use log::{error, info, warn};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Emitter;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

pub struct FileWatcher {
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
}

impl FileWatcher {
    pub fn new(
        path: PathBuf,
        cache_dir: PathBuf,
        db: Arc<Database>,
        app_handle: tauri::AppHandle,
        security_runtime: Arc<Mutex<RuntimeState>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (tx, mut rx) = mpsc::channel(100);

        let watcher_config = Config::default()
            .with_poll_interval(Duration::from_secs(2))
            .with_compare_contents(true);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            watcher_config,
        )?;

        watcher.watch(&path, RecursiveMode::Recursive)?;

        info!("Watcher started on {:?}", path);

        let app_handle = app_handle.clone(); // Clone for async block
        let path_clone = path.clone();
        let cache_dir_clone = cache_dir.clone();
        let db_clone = db.clone();
        let app_handle_scan = app_handle.clone();
        let runtime_for_scan = security_runtime.clone();
        let runtime_for_event = security_runtime.clone();

        tokio::spawn(async move {
            info!("Starting initial scan of {:?}", path_clone);
            if let Ok(entries) = fs::read_dir(&path_clone) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        info!("Initial scan checking: {:?}", path);
                        if let Err(e) = process_file(
                            &path,
                            &cache_dir_clone,
                            &db_clone,
                            Some(&app_handle_scan),
                            &runtime_for_scan,
                        )
                        .await
                        {
                            error!("Failed to process existing file {:?}: {}", path, e);
                        }
                    }
                }
            } else {
                error!(
                    "Failed to read directory for initial scan: {:?}",
                    path_clone
                );
            }
            info!("Initial scan completed.");

            while let Some(event) = rx.recv().await {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if path.is_file() {
                                if let Err(e) = process_file(
                                    &path,
                                    &cache_dir,
                                    &db,
                                    Some(&app_handle),
                                    &runtime_for_event,
                                )
                                .await
                                {
                                    error!("Failed to process file {:?}: {}", path, e);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        Ok(Self { watcher })
    }
}

async fn process_file(
    path: &Path,
    cache_dir: &Path,
    db: &Arc<Database>,
    app_handle: Option<&tauri::AppHandle>,
    security_runtime: &Arc<Mutex<RuntimeState>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 0. Ignore temp files
    if let Some(ext) = path.extension() {
        let ext_str = ext.to_string_lossy().to_lowercase();
        if ext_str == "tmp" || ext_str == "part" || ext_str == "crdownload" {
            // debug!("Ignoring temporary file: {:?}", path);
            return Ok(());
        }
    }

    // Retry loop for file access (Windows file locking/copying delay)
    let mut retries = 0;
    let max_retries = 5;
    let mut hash = String::new();

    while retries < max_retries {
        match media_utils::hash_file_streaming(path) {
            Ok(h) => {
                hash = h;
                break;
            }
            Err(e) => {
                if retries == max_retries - 1 {
                    error!("Failed to hash file after retries {:?}: {}", path, e);
                    return Err(Box::new(e));
                }
                warn!(
                    "File busy or inaccessible, retrying ({}/{}): {:?}",
                    retries + 1,
                    max_retries,
                    path
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
                retries += 1;
            }
        }
    }

    // 2. Check deduplication
    if db.media_exists_by_hash(&hash)? {
        if !db.is_media_uploaded(&hash)? {
            info!("File exists but NOT uploaded. Re-queueing: {:?}", path);
            let path_str = path.to_string_lossy().to_string();
            // This is safe because database::add_to_queue now handles its own deduplication
            db.add_to_queue(&path_str)?;
        } else {
            info!("Skipping duplicate file (already uploaded): {:?}", path);
        }
        return Ok(());
    }

    info!("New file detected: {:?} (Hash: {})", path, hash);

    let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let path_str = path.to_string_lossy().to_string();

    // 3. Generate Thumbnail using shared utility
    // Wait slightly to ensure file handle is mostly free
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Determine Mime Type early to check if video
    let mime_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let is_video = mime_type.starts_with("video/");

    let mut thumbnail_path = if is_video {
        // Use FFmpeg for video thumbnails
        match media_utils::generate_video_thumbnail(path, cache_dir, &hash, 300).await {
            Ok(Some(thumb_path)) => Some(thumb_path.to_string_lossy().to_string()),
            Ok(None) => None,
            Err(e) => {
                warn!("Video thumbnail generation failed for {:?}: {}", path, e);
                None
            }
        }
    } else {
        // Use image library for image thumbnails
        match media_utils::generate_thumbnail(path, cache_dir, &hash, 300).await {
            Ok(Some(thumb_path)) => Some(thumb_path.to_string_lossy().to_string()),
            Ok(None) => None,
            Err(e) => {
                warn!("Thumbnail generation failed for {:?}: {}", path, e);
                None
            }
        }
    };

    // Encrypt thumbnail at rest when security mode is enabled.
    // Fail closed: if the bundle cannot be read, assume encryption is required.
    // The branch below deletes the plaintext thumbnail when it cannot seal it,
    // so the worst case is a missing thumbnail rather than a plaintext one.
    let thumbnail_needs_encryption = crate::encryption_required(db).unwrap_or_else(|e| {
        warn!(
            "Cannot determine encryption state, treating thumbnail as encrypted: {}",
            e
        );
        true
    });
    if thumbnail_needs_encryption {
        if let Some(thumb_str) = thumbnail_path.clone() {
            let thumb_path = PathBuf::from(&thumb_str);
            let maybe_key = security_runtime.lock().await.master_key;
            if let Some(key) = maybe_key {
                let encrypted_thumb = thumb_path.with_extension("wbenc");
                match security::encrypt_file(&thumb_path, &encrypted_thumb, &key) {
                    Ok(_) => {
                        let _ = fs::remove_file(&thumb_path);
                        thumbnail_path = Some(encrypted_thumb.to_string_lossy().to_string());
                    }
                    Err(e) => {
                        warn!(
                            "Failed to encrypt thumbnail {:?}, dropping thumbnail: {}",
                            thumb_path, e
                        );
                        let _ = fs::remove_file(&thumb_path);
                        thumbnail_path = None;
                    }
                }
            } else {
                // Avoid leaving plaintext thumbnail when vault is locked.
                let _ = fs::remove_file(&thumb_path);
                thumbnail_path = None;
            }
        }
    }

    // 4. Extract Metadata
    let metadata = if !is_video {
        Some(crate::metadata::extract_metadata(path))
    } else {
        None
    };

    // 4.5 Generate Perceptual Hash (for duplicates) unless video
    let phash = if !is_video {
        media_utils::generate_phash(path)
    } else {
        None
    };

    // 5. Add to media table (mime_type already computed above)
    db.add_media(
        &path_str,
        Some(&hash),
        thumbnail_path.as_deref(),
        created_at,
        Some(&mime_type),
        metadata,
        phash.as_deref(),
    )?;

    // 6. Add to upload queue
    db.add_to_queue(&path_str)?;
    info!("Added to upload queue: {:?}", path);

    // 6. Emit event
    if let Some(app_handle) = &app_handle {
        info!("Emitting media-added event");
        let _ = app_handle.emit("media-added", ());
    }

    Ok(())
}
