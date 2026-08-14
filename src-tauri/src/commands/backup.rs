//! Commands for the encrypted backup archive and the sync manifest.

use crate::*;

#[tauri::command]
pub async fn get_backup_path(app: tauri::AppHandle) -> Result<String, AppError> {
    let app_data = resolve_app_data_dir(&app)?;
    let backup_dir = app_data.join("backup");
    std::fs::create_dir_all(&backup_dir).map_err(AppError::from)?;
    Ok(backup_dir.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn backup_database(
    destination: Option<String>,
    upload_to_telegram: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, AppError> {
    use std::path::Path;

    // Get the database path
    let app_data = resolve_app_data_dir(&app)?;
    let db_path = app_data.join("library.db");

    if !db_path.exists() {
        return Err(AppError::not_found("Database file not found"));
    }

    // Determine backup destination. A caller-supplied destination must be an
    // existing directory the user already has: this command creates a file with
    // a name it chooses itself, and has no business creating directory trees at
    // arbitrary locations on the disk.
    let backup_path = if let Some(dest) = destination {
        let dest_path = paths::normalize_lexically(Path::new(&dest));
        if !dest_path.is_dir() {
            return Err(AppError::invalid_input(format!(
                "Backup destination is not an existing directory: {}",
                dest_path.display()
            )));
        }
        let filename = format!(
            "library_backup_{}.db",
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        dest_path.join(filename)
    } else {
        // Default to app data dir
        let filename = format!(
            "library_backup_{}.db",
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        app_data.join("backups").join(filename)
    };

    // Create backup directory if needed
    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent).map_err(AppError::from)?;
    }

    // Snapshot through SQLite rather than copying the file. See `Database::backup_to`:
    // with WAL enabled, a raw copy of library.db can omit committed data that is still
    // sitting in the write-ahead log.
    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        db.backup_to(&backup_path)
            .map_err(|e| format!("Failed to write the database snapshot: {}", e))?;
    }

    let mut final_backup_path = backup_path.clone();

    // The authoritative encryption state is the bundle, not the `security_mode`
    // row, and a read failure must not silently downgrade the backup to
    // plaintext, so this fails closed rather than defaulting to "unset".
    let bundle = {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        load_security_bundle(db)?
    };
    let encrypted_mode = bundle
        .as_ref()
        .map(|b| b.mode == EncryptionMode::Encrypted)
        .unwrap_or(false);

    if encrypted_mode {
        let bundle = bundle.ok_or_else(|| AppError::not_found("Security bundle missing"))?;
        let key = get_active_master_key(&state).await.ok_or_else(|| {
            "Encryption vault is locked. Unlock to create encrypted backup.".to_string()
        })?;
        // The archive carries the wrapped master key in its plaintext header, so
        // the passphrase and the recovery key can open it on a machine that has
        // lost `library.db`. See `backup.rs`.
        let archive_path = backup_path.with_extension("wbak");
        backup::write_encrypted_backup(
            &backup_path,
            &archive_path,
            &bundle,
            &key,
            app.package_info().version.to_string().as_str(),
        )
        .map_err(AppError::from)?;
        let _ = std::fs::remove_file(&backup_path);
        final_backup_path = archive_path;
    }

    let backup_path_str = final_backup_path.to_string_lossy().to_string();

    // Optionally upload to Telegram
    if upload_to_telegram {
        match state.telegram.upload_file(&backup_path_str).await {
            Ok(_) => {
                log::info!("Database backup uploaded to Telegram");
            }
            Err(e) => {
                log::warn!("Failed to upload backup to Telegram: {}", e);
                // Don't fail the whole operation
            }
        }
    }

    Ok(backup_path_str)
}

#[tauri::command]
pub async fn inspect_backup_archive(archive_path: String) -> Result<BackupArchiveInfo, AppError> {
    let path = std::path::PathBuf::from(&archive_path);
    let header = backup::read_header(&path).map_err(AppError::from)?;
    Ok(BackupArchiveInfo {
        format_version: header.format_version,
        created_at: header.created_at,
        app_version: header.app_version,
        source_file: header.source_file,
        encrypted: header.bundle.mode == EncryptionMode::Encrypted,
        has_passphrase_wrap: header.bundle.passphrase_wrap.is_some(),
        has_recovery_wrap: header.bundle.recovery.is_some(),
    })
}

/// Decrypt a backup archive to a plaintext `library.db` next to it.
///
/// This deliberately does not overwrite the live database: the app holds it
/// open, and a disaster restore is done with the app closed. It returns the path
/// of the restored file for the caller to put in place.
#[tauri::command]
pub async fn restore_backup_archive(
    archive_path: String,
    passphrase: Option<String>,
    recovery_key: Option<String>,
) -> Result<String, AppError> {
    let archive = std::path::PathBuf::from(&archive_path);
    let passphrase = passphrase.filter(|p| !p.is_empty()).map(Zeroizing::new);
    let recovery_key = recovery_key.filter(|k| !k.is_empty()).map(Zeroizing::new);
    if passphrase.is_none() && recovery_key.is_none() {
        return Err(AppError::invalid_input(
            "A passphrase or a recovery key is required",
        ));
    }

    let out_path = archive.with_extension("restored.db");
    // Argon2id at 64 MiB plus a full-file decrypt is far too much work for the
    // async runtime; every other heavy path in this file that gets this right
    // uses spawn_blocking too.
    tauri::async_runtime::spawn_blocking(move || {
        let secret = match (passphrase.as_deref(), recovery_key.as_deref()) {
            (Some(p), _) => backup::BackupSecret::Passphrase(p.as_str()),
            (None, Some(k)) => backup::BackupSecret::RecoveryKey(k.as_str()),
            (None, None) => {
                return Err(AppError::invalid_input(
                    "A passphrase or a recovery key is required",
                ))
            }
        };
        backup::restore_encrypted_backup(&archive, &out_path, secret)
            .map(|_| out_path.to_string_lossy().to_string())
            .map_err(AppError::from)
    })
    .await
    .map_err(AppError::from)?
}

/// Export the current database state to a sync manifest JSON file
/// Returns the path to the generated manifest file
#[tauri::command]
pub async fn export_sync_manifest(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    // Get or create device ID
    let device_id = db
        .get_config("device_id")
        .map_err(AppError::from)?
        .unwrap_or_else(|| {
            let id = sync_manifest::generate_device_id();
            let _ = db.set_config("device_id", &id);
            id
        });

    // Create manifest from current database state
    let mut manifest = sync_manifest::SyncManifest::new(device_id);

    // Export all media metadata. Album membership is read once for the whole
    // library rather than per photo: the per-photo call ran two correlated cover
    // subqueries over every album, for a cover path the manifest does not use.
    let all_media = db.get_all_media_for_sync().map_err(AppError::from)?;
    let albums_by_media = db.album_names_by_media().map_err(AppError::from)?;
    for item in all_media {
        if let Some(hash) = &item.file_hash {
            let albums = albums_by_media.get(&item.id).cloned().unwrap_or_default();
            manifest.update_media(hash, item.is_favorite, item.rating, albums);
        }
    }

    // Export all albums
    let all_albums = db.get_albums().map_err(AppError::from)?;
    for album in all_albums {
        let normalized = album.name.to_lowercase().replace(' ', "_");
        manifest.add_album(&normalized, &album.name);
    }

    // Save to temp file
    let app_dir = resolve_app_data_dir(&app)?;
    let manifest_path = app_dir.join(sync_manifest::MANIFEST_FILENAME);

    manifest.to_file(&manifest_path)?;

    log::info!("Exported sync manifest to {:?}", manifest_path);
    Ok(manifest_path.to_string_lossy().to_string())
}

/// Import and merge a sync manifest from a file path
/// Updates local database with merged values using LWW
#[tauri::command]
pub async fn import_sync_manifest(
    path: String,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    // Load the remote manifest
    let remote_manifest = sync_manifest::SyncManifest::from_file(std::path::Path::new(&path))?;

    let mut updated_count = 0;

    // Apply merged media metadata to database
    for (hash, meta) in &remote_manifest.media {
        // Find media by hash
        if let Ok(Some(media)) = db.get_media_by_hash(hash) {
            // Get current last_modified from local
            let local_modified = db
                .get_config(&format!("media_modified_{}", media.id))
                .map_err(AppError::from)?
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

            // LWW: only update if remote is newer
            if meta.last_modified > local_modified {
                // Update favorite
                if meta.is_favorite != media.is_favorite {
                    let _ = db.set_favorite(media.id, meta.is_favorite);
                }
                // Update rating
                if meta.rating != media.rating {
                    let _ = db.set_rating(media.id, meta.rating);
                }
                // Store new last_modified
                let _ = db.set_config(&format!("media_modified_{}", media.id), &meta.last_modified);
                updated_count += 1;
            }
        }
    }

    // Create any new albums from the manifest
    for album_meta in remote_manifest.albums.values() {
        if db
            .get_album_by_name(&album_meta.name)
            .map_err(AppError::from)?
            .is_none()
        {
            let _ = db.create_album(&album_meta.name);
        }
    }

    log::info!("Imported sync manifest: {} items updated", updated_count);
    Ok(format!("Synced {} items from manifest", updated_count))
}

/// Get the unique device ID for this installation
#[tauri::command]
pub async fn get_device_id(state: State<'_, AppState>) -> Result<String, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;

    let device_id = db
        .get_config("device_id")
        .map_err(AppError::from)?
        .unwrap_or_else(|| {
            let id = sync_manifest::generate_device_id();
            let _ = db.set_config("device_id", &id);
            id
        });

    Ok(device_id)
}
