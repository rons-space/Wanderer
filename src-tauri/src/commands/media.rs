//! Commands over the media library: import, read, rate, trash, archive
//! and the local/cloud copy of a file.

use crate::*;

#[tauri::command]
pub async fn get_media(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let _db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    log::debug!("get_media: limit={}, offset={}", limit, offset);
    let result = _db.get_media(limit, offset).map_err(AppError::from);
    match &result {
        Ok(items) => log::debug!("get_media: returning {} items", items.len()),
        Err(e) => log::warn!("get_media failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn import_files(files: Vec<String>, app: tauri::AppHandle) -> Result<usize, AppError> {
    // Resolve backup directory in app data path
    let app_dir = resolve_app_data_dir(&app)?;

    let backup_dir = app_dir.join("backup");

    // Ensure it exists (should be created by setup, but safety check)
    std::fs::create_dir_all(&backup_dir).map_err(AppError::from)?;

    let mut success_count = 0;

    let managed = paths::managed_roots(&app_dir);
    let mut rejected = 0usize;

    for file_path in files {
        let path = std::path::Path::new(&file_path);

        // Imports are indexed by the watcher and uploaded to Telegram, so an
        // unfiltered import is an exfiltration primitive: a compromised webview
        // could import `id_rsa` and have it sent to the cloud. Only media the
        // app can display is accepted.
        if !path.is_file() || !paths::is_importable(path) {
            rejected += 1;
            log::warn!("Import refused for non-media path: {}", file_path);
            continue;
        }

        // Copying a file that is already in the library onto itself is at best
        // pointless and at worst destructive.
        if paths::is_within_any(&managed, path) {
            rejected += 1;
            log::warn!(
                "Import refused for a path already in the library: {}",
                file_path
            );
            continue;
        }

        if let Some(file_name) = path.file_name() {
            let dest_path = backup_dir.join(file_name);

            // Skip if file already exists (duplicate)
            if dest_path.exists() {
                log::info!("Skipping duplicate file: {:?}", file_name);
                continue;
            }

            // Copy the file. `tokio::fs::copy` runs it on a blocking thread, which
            // matters here: these are media files and the loop can be hundreds long.
            if let Err(e) = tokio::fs::copy(path, &dest_path).await {
                log::error!("Failed to copy file {:?} to {:?}: {}", path, dest_path, e);
            } else {
                success_count += 1;
            }
        }
    }

    if rejected > 0 {
        log::warn!(
            "Import rejected {} path(s) that were not importable media",
            rejected
        );
    }

    Ok(success_count)
}

#[tauri::command]
pub async fn toggle_favorite(media_id: i64, state: State<'_, AppState>) -> Result<bool, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.toggle_favorite(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn set_rating(
    media_id: i64,
    rating: i32,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.set_rating(media_id, rating).map_err(AppError::from)
}

#[tauri::command]
pub async fn get_favorites(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_favorites(limit, offset).map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn soft_delete_media(media_id: i64, state: State<'_, AppState>) -> Result<(), AppError> {
    log::debug!("soft_delete_media: media_id={}", media_id);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.soft_delete(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn restore_from_trash(media_id: i64, state: State<'_, AppState>) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.restore_from_trash(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn get_trash(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    log::debug!("get_trash: limit={}, offset={}", limit, offset);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_trash(limit, offset).map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn bulk_set_favorite(
    media_ids: Vec<i64>,
    is_favorite: bool,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.bulk_set_favorite(&media_ids, is_favorite)
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn bulk_delete(
    media_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.bulk_soft_delete(&media_ids).map_err(AppError::from)
}

#[tauri::command]
pub async fn bulk_add_to_album(
    album_id: i64,
    media_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.bulk_add_to_album(album_id, &media_ids)
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn export_media(
    media_ids: Vec<i64>,
    destination: String,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    use std::path::Path;
    use time::OffsetDateTime;

    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_media_by_ids(&media_ids).map_err(AppError::from)?;
    drop(db_guard);

    let dest_path = paths::normalize_lexically(Path::new(&destination));
    if !dest_path.is_absolute() {
        return Err(AppError::invalid_input(
            "Export destination must be an absolute path",
        ));
    }
    if !dest_path.exists() {
        std::fs::create_dir_all(&dest_path).map_err(AppError::from)?;
    }
    let dest_root = paths::resolve(&dest_path);

    let mut exported = 0;
    for item in &items {
        let source = Path::new(&item.file_path);
        let source_hint = Path::new(&item.file_path);

        // Create Year/Month folder structure.
        //
        // `date_taken` comes from the photo's EXIF, so it is attacker-supplied
        // text being used as two path components. Anything that is not a plain
        // number falls back to today rather than steering the write.
        let now = OffsetDateTime::now_utc();
        let fallback = (now.year().to_string(), format!("{:02}", now.month() as u8));
        let (year, month) = item
            .date_taken
            .as_deref()
            .and_then(|date_taken| {
                // Format: "2026-01-15 12:00:00"
                let mut parts = date_taken.split('-');
                let year = paths::sanitize_date_fragment(parts.next()?, 4)?;
                let month = paths::sanitize_date_fragment(parts.next()?, 2)?;
                Some((year, month))
            })
            .unwrap_or(fallback);

        let folder = dest_path.join(&year).join(&month);
        if !folder.exists() {
            std::fs::create_dir_all(&folder).map_err(AppError::from)?;
        }

        let file_name = source_hint
            .file_name()
            .ok_or_else(|| AppError::invalid_input("Invalid file name"))?;
        let dest_file = folder.join(file_name);

        // Handle duplicate filenames
        let final_dest = if dest_file.exists() {
            let stem = source_hint
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let ext = source_hint
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut counter = 1;
            loop {
                let new_name = if ext.is_empty() {
                    format!("{}_{}", stem, counter)
                } else {
                    format!("{}_{}.{}", stem, counter, ext)
                };
                let candidate = folder.join(&new_name);
                if !candidate.exists() {
                    break candidate;
                }
                counter += 1;
            }
        } else {
            dest_file
        };

        // Belt and braces: whatever the pieces above produced, the write must
        // land under the directory the user chose.
        let final_dest = paths::confine(&dest_root, &final_dest)?;

        if source.exists() {
            tokio::fs::copy(source, &final_dest)
                .await
                .map_err(AppError::from)?;
            exported += 1;
            continue;
        }

        // Cloud-only fallback: pull from Telegram directly to export destination.
        let Some(telegram_id) = &item.telegram_media_id else {
            log::warn!(
                "Export skipped: local file missing and no Telegram ID for media {} ({})",
                item.id,
                item.file_path
            );
            continue;
        };

        let msg_id = match telegram_id.parse::<i32>() {
            Ok(id) => id,
            Err(_) => {
                log::warn!(
                    "Export skipped: invalid telegram_media_id '{}' for media {}",
                    telegram_id,
                    item.id
                );
                continue;
            }
        };

        match download_and_materialize_media(&state, msg_id, &final_dest).await {
            Ok(_) => {
                exported += 1;
            }
            Err(e) => {
                log::warn!(
                    "Export skipped: failed Telegram download for media {} (msg {}): {}",
                    item.id,
                    msg_id,
                    e
                );
            }
        }
    }

    Ok(exported)
}

#[tauri::command]
pub async fn find_duplicates(
    state: State<'_, AppState>,
) -> Result<Vec<Vec<database::MediaItem>>, AppError> {
    // Opportunistically fill missing pHashes so Refresh can recover even if
    // Scan Library was run before watcher ingestion completed.
    let items_to_scan = {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        db.get_media_without_phash().map_err(AppError::from)?
    };

    if !items_to_scan.is_empty() {
        // Hashing decodes and resizes every image, so it belongs on a blocking
        // thread, and the results are written in one transaction rather than
        // reacquiring the database lock per photo.
        let hashes = tauri::async_runtime::spawn_blocking(move || hash_batch(&items_to_scan))
            .await
            .map_err(AppError::from)?;

        if !hashes.is_empty() {
            let db_guard = state.db.lock().await;
            if let Some(db) = db_guard.as_ref() {
                db.update_phashes(&hashes).map_err(AppError::from)?;
            }
        }
    }

    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let groups = db.find_duplicates().map_err(AppError::from)?;
    drop(db_guard);

    let mut out = Vec::with_capacity(groups.len());
    for group in groups {
        out.push(materialize_media_items_for_response(group, &state).await);
    }
    Ok(out)
}

/// Scan media library and compute perceptual hashes for duplicates detection
/// Returns the number of items that were successfully hashed
#[tauri::command]
pub async fn scan_duplicates(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    use tauri::Emitter;

    // Prefer missing hashes first. If none are missing, run a full image rescan.
    // This recovers from stale/invalid historical phash values and keeps
    // "Scan Library" behavior deterministic for QA workflows.
    let items_to_scan = {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        let missing = db.get_media_without_phash().map_err(AppError::from)?;
        if missing.is_empty() {
            db.get_all_media_for_phash_scan().map_err(AppError::from)?
        } else {
            missing
        }
    };

    let total = items_to_scan.len();
    if total == 0 {
        return Ok(0);
    }

    log::info!("Scanning {} items for phash", total);
    let _ = app.emit("scan-duplicates-started", total);

    let mut success_count = 0;
    let mut scanned = 0usize;

    // A chunk at a time: hashing runs off the async runtime, each chunk's results
    // are one transaction instead of one per photo, and progress still moves.
    for chunk in items_to_scan.chunks(PHASH_SCAN_CHUNK) {
        let chunk = chunk.to_vec();
        let chunk_len = chunk.len();

        let hashes = tauri::async_runtime::spawn_blocking(move || hash_batch(&chunk))
            .await
            .map_err(AppError::from)?;

        if !hashes.is_empty() {
            let db_guard = state.db.lock().await;
            if let Some(db) = db_guard.as_ref() {
                success_count += db.update_phashes(&hashes).map_err(AppError::from)?;
            }
        }

        scanned += chunk_len;
        let _ = app.emit("scan-duplicates-progress", (scanned, total));
    }

    log::info!("Scan complete: {} of {} items hashed", success_count, total);
    let _ = app.emit("scan-duplicates-finished", success_count);

    Ok(success_count)
}

#[tauri::command]
pub async fn get_smart_album_counts(
    state: State<'_, AppState>,
) -> Result<database::SmartAlbumCounts, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_smart_album_counts().map_err(AppError::from)
}

#[tauri::command]
pub async fn get_videos(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_videos(limit, offset).map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn get_recent(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_recent(limit, offset).map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn get_top_rated(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db.get_top_rated(limit, offset).map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn archive_media(media_id: i64, state: State<'_, AppState>) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.archive_media(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn unarchive_media(media_id: i64, state: State<'_, AppState>) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.unarchive_media(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn get_archived_media(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db
        .get_archived_media(limit, offset)
        .map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn permanent_delete_media(
    media_id: i64,
    delete_from_telegram: bool,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    // Delete from local + DB, get telegram_media_id
    let telegram_media_id = db.permanent_delete(media_id).map_err(AppError::from)?;

    // Optionally delete from Telegram
    if delete_from_telegram {
        if let Some(tg_id_str) = telegram_media_id {
            if let Ok(tg_id) = tg_id_str.parse::<i32>() {
                drop(db_guard); // Release DB lock before async operation
                let _ = state.telegram.delete_messages(&[tg_id]).await;
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn empty_trash(
    delete_from_telegram: bool,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    log::info!("empty_trash: delete_from_telegram={}", delete_from_telegram);

    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    // Delete all trashed items from local + DB
    let (deleted_count, telegram_ids) = db.empty_trash().map_err(AppError::from)?;

    log::info!(
        "empty_trash: deleted {} items locally, {} with a Telegram copy",
        deleted_count,
        telegram_ids.len()
    );

    // Optionally delete from Telegram
    if delete_from_telegram && !telegram_ids.is_empty() {
        drop(db_guard); // Release DB lock before async operation

        let msg_ids: Vec<i32> = telegram_ids
            .iter()
            .filter_map(|id| {
                let parsed = id.parse::<i32>().ok();
                if parsed.is_none() {
                    log::warn!("empty_trash: unparseable telegram_id, skipping it");
                }
                parsed
            })
            .collect();

        log::debug!(
            "empty_trash: {} message IDs to delete from Telegram",
            msg_ids.len()
        );

        if !msg_ids.is_empty() {
            match state.telegram.delete_messages(&msg_ids).await {
                Ok(deleted) => {
                    log::info!("empty_trash: deleted {} messages from Telegram", deleted);
                }
                Err(e) => {
                    log::warn!("empty_trash: failed to delete from Telegram: {}", e);
                }
            }
        }
    }

    Ok(deleted_count)
}

#[tauri::command]
pub async fn remove_local_copy(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    // Get the media item to find the file path
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("Media not found"))?;

    // Check if it has a telegram_media_id (required for cloud-only mode)
    if media.telegram_media_id.is_none() {
        return Err(AppError::invalid_input(
            "Cannot remove local copy: media not uploaded to Telegram yet",
        ));
    }

    // Check if already cloud-only
    if media.is_cloud_only {
        return Err(AppError::invalid_input("Media is already cloud-only"));
    }

    // Delete the local file (but keep the thumbnail). `file_path` is a plain
    // text column, so it is confined to the managed library before any unlink.
    let app_data = resolve_app_data_dir(&app)?;
    let roots = paths::managed_roots(&app_data);
    let file_path = std::path::Path::new(&media.file_path);
    if !paths::is_within_any(&roots, file_path) {
        return Err(AppError::invalid_input(format!(
            "Refusing to delete a file outside the managed library: {}",
            media.file_path
        )));
    }
    if file_path.exists() {
        std::fs::remove_file(file_path).map_err(|e| format!("Failed to delete file: {}", e))?;
    }

    // Mark as cloud-only in database
    db.set_cloud_only(media_id, true).map_err(AppError::from)?;

    log::info!("Removed local copy for media {}, now cloud-only", media_id);
    Ok(())
}

#[tauri::command]
pub async fn download_local_copy(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, AppError> {
    // Get the media item
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("Media not found"))?;

    // Check if it's cloud-only
    if !media.is_cloud_only {
        return Err(AppError::invalid_input("Media already has local copy"));
    }

    // Get the telegram_media_id
    let telegram_id = media
        .telegram_media_id
        .clone()
        .ok_or_else(|| AppError::not_found("No Telegram ID found"))?;

    // Parse the telegram_media_id to get the message ID
    let msg_id: i32 = telegram_id
        .parse()
        .map_err(|_| "Invalid Telegram message ID".to_string())?;

    // Drop db guard before async operation
    drop(db_guard);

    // Get the backup directory
    let app_data = resolve_app_data_dir(&app)?;
    let backup_dir = app_data.join("backup");
    std::fs::create_dir_all(&backup_dir).map_err(AppError::from)?;

    // Determine filename from original path
    let filename = std::path::Path::new(&media.file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let download_path = backup_dir.join(filename);
    let download_path_str = download_path.to_string_lossy().to_string();

    // Download to a temp file first to avoid watcher hashing/upload-queue races
    // while the file is still being written.
    let restore_staging_dir = paths::local_restore_staging_dir();
    std::fs::create_dir_all(&restore_staging_dir).map_err(AppError::from)?;
    let staged_path = restore_staging_dir.join(format!(
        "restore_{}_{}.tmp",
        media_id,
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));

    // Download from Telegram and decrypt transparently when needed.
    let download_result = download_and_materialize_media(&state, msg_id, &staged_path).await;
    if let Err(e) = download_result {
        let _ = std::fs::remove_file(&staged_path);
        return Err(e);
    }

    if download_path.exists() {
        let _ = std::fs::remove_file(&download_path);
    }
    match tokio::fs::rename(&staged_path, &download_path).await {
        Ok(_) => {}
        // Across filesystems rename fails and the media has to be copied, which for a
        // restored video is gigabytes of work.
        Err(_) => {
            tokio::fs::copy(&staged_path, &download_path)
                .await
                .map_err(AppError::from)?;
            let _ = tokio::fs::remove_file(&staged_path).await;
        }
    }

    // Re-acquire db lock to update
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    // Mark as not cloud-only
    db.set_cloud_only(media_id, false).map_err(AppError::from)?;

    log::info!(
        "Downloaded local copy for media {} to {}",
        media_id,
        download_path_str
    );
    Ok(download_path_str)
}

#[tauri::command]
pub async fn download_for_view(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, AppError> {
    // Get the media item
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("Media not found"))?;

    // Check if it's cloud-only
    if !media.is_cloud_only {
        // If not cloud-only, return existing path if it exists
        // Or if it doesn't exist (deleted manually?), simple return file_path
        // expecting frontend to handle it, OR we could try to download it?
        // For now, if it's not cloud-only, just return current path.
        return Ok(media.file_path);
    }

    // Get the telegram_media_id
    let telegram_id = media
        .telegram_media_id
        .clone()
        .ok_or_else(|| AppError::not_found("No Telegram ID found"))?;

    // Parse the telegram_media_id to get the message ID
    let msg_id: i32 = telegram_id
        .parse()
        .map_err(|_| "Invalid Telegram message ID".to_string())?;

    // Drop db guard
    drop(db_guard);

    // Get the view_cache directory
    let app_data = resolve_app_data_dir(&app)?;
    let cache_dir = app_data.join("view_cache");
    std::fs::create_dir_all(&cache_dir).map_err(AppError::from)?;
    let encrypted_mode = {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        encryption_required(db)?
    };

    // Determine filename
    let filename = std::path::Path::new(&media.file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cache_file");

    if encrypted_mode {
        let key = get_active_master_key(&state)
            .await
            .ok_or_else(|| AppError::vault_locked("Unlock encryption to view cloud media"))?;

        // In encrypted mode, keep cache encrypted-at-rest and materialize plaintext
        // only in temp for active viewing.
        let cache_blob_path = cache_dir.join(format!("{}_{}.wbenc", media_id, filename));

        if !cache_blob_path.exists() {
            let staging_dir = paths::view_cache_staging_dir();
            std::fs::create_dir_all(&staging_dir).map_err(AppError::from)?;
            let raw_download_path = staging_dir.join(format!(
                "view_{}_{}.bin",
                media_id,
                time::OffsetDateTime::now_utc().unix_timestamp_nanos()
            ));
            let raw_download_str = raw_download_path.to_string_lossy().to_string();

            state
                .telegram
                .download_by_message_id(msg_id, &raw_download_str)
                .await
                .map_err(|e| format!("Failed to download from Telegram: {}", e))?;

            let downloaded_is_encrypted =
                security::is_encrypted_file(&raw_download_path).map_err(AppError::from)?;

            let write_result: Result<(), AppError> = if downloaded_is_encrypted {
                match tokio::fs::rename(&raw_download_path, &cache_blob_path).await {
                    Ok(_) => Ok(()),
                    // Rename fails across devices, so the staging directory and the
                    // cache may not share a filesystem. Copying a media file is worth
                    // getting off the runtime.
                    Err(_) => {
                        tokio::fs::copy(&raw_download_path, &cache_blob_path)
                            .await
                            .map_err(AppError::from)?;
                        let _ = tokio::fs::remove_file(&raw_download_path).await;
                        Ok(())
                    }
                }
            } else {
                let encrypt_src = raw_download_path.clone();
                let encrypt_dst = cache_blob_path.clone();
                let encrypt_key = key.clone();
                tokio::task::spawn_blocking(move || {
                    security::encrypt_file(&encrypt_src, &encrypt_dst, &encrypt_key)
                })
                .await
                .map_err(|e| format!("Encrypt task failed: {}", e))?
                .map_err(AppError::from)?;
                let _ = tokio::fs::remove_file(&raw_download_path).await;
                Ok(())
            };

            if let Err(e) = write_result {
                let _ = std::fs::remove_file(&raw_download_path);
                return Err(e);
            }
        }

        let _ = filetime::set_file_mtime(&cache_blob_path, filetime::FileTime::now());

        let materialized_dir = paths::view_cache_materialized_dir();
        std::fs::create_dir_all(&materialized_dir).map_err(AppError::from)?;
        let cache_key = blake3::hash(cache_blob_path.to_string_lossy().as_bytes())
            .to_hex()
            .to_string();
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let materialized_path =
            materialized_dir.join(format!("{}_{}.{}", media_id, cache_key, ext));

        let needs_refresh = if materialized_path.exists() {
            let src_m = std::fs::metadata(&cache_blob_path).and_then(|m| m.modified());
            let out_m = std::fs::metadata(&materialized_path).and_then(|m| m.modified());
            match (src_m, out_m) {
                (Ok(s), Ok(o)) => s > o,
                _ => true,
            }
        } else {
            true
        };

        if needs_refresh {
            // This app wrote `cache_blob_path` itself, in encrypted mode, so a
            // missing header means the cache has been tampered with rather than
            // that the blob is legitimately plaintext.
            let decrypt_src = cache_blob_path.clone();
            let decrypt_dst = materialized_path.clone();
            let decrypt_key = key.clone();
            tokio::task::spawn_blocking(move || {
                security::decrypt_file_if_needed(
                    &decrypt_src,
                    &decrypt_dst,
                    Some(&decrypt_key),
                    security::Expect::Encrypted,
                )
            })
            .await
            .map_err(|e| format!("Decrypt task failed: {}", e))?
            .map_err(AppError::from)?;
        }
        let _ = filetime::set_file_mtime(&materialized_path, filetime::FileTime::now());
        return Ok(materialized_path.to_string_lossy().to_string());
    }

    // Unencrypted mode cache path (plaintext-at-rest).
    let cache_path = cache_dir.join(format!("{}_{}", media_id, filename));
    let cache_path_str = cache_path.to_string_lossy().to_string();
    if cache_path.exists() {
        let _ = filetime::set_file_mtime(&cache_path, filetime::FileTime::now());
        return Ok(cache_path_str);
    }

    download_and_materialize_media(&state, msg_id, &cache_path).await?;

    log::info!(
        "Downloaded view cache for media {} to {}",
        media_id,
        cache_path_str
    );
    Ok(cache_path_str)
}

/// Generate a Telegram share link for a media item
/// Returns a tg:// deep link that opens the message in Telegram
#[tauri::command]
pub async fn generate_share_link(
    media_id: i64,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("Media not found"))?;

    // Check if uploaded to Telegram
    let telegram_id = media
        .telegram_media_id
        .ok_or_else(|| AppError::invalid_input("Media not uploaded to Telegram yet"))?;

    // Parse the telegram_media_id to extract message_id
    // Format is typically "msg_id" as a string
    let msg_id: i32 = telegram_id
        .parse()
        .map_err(|_| "Invalid telegram_media_id format")?;

    // Generate Saved Messages deep link
    // tg://resolve?domain=me works for Saved Messages
    // For direct message link: https://t.me/c/{chat_id}/{msg_id} but Saved Messages is special
    // Using the "me" domain which represents Saved Messages
    let share_link = format!("tg://openmessage?user_id=me&message_id={}", msg_id);

    // Alternative formats that also work:
    // - https://t.me/c/0/{msg_id} (Saved Messages as chat_id 0)
    // - tg://privatepost?channel=0&post={msg_id}

    log::info!(
        "Generated share link for media {}: {}",
        media_id,
        share_link
    );
    Ok(share_link)
}
