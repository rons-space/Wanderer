//! Commands for the passphrase lifecycle, the vault lock, and the
//! plaintext-to-encrypted migration.

use crate::*;

#[tauri::command]
pub async fn get_security_status(
    state: State<'_, AppState>,
) -> Result<SecurityStatusResponse, AppError> {
    get_security_status_inner(&state).await
}

#[tauri::command]
pub async fn initialize_unencrypted_mode(state: State<'_, AppState>) -> Result<(), AppError> {
    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        if let Some(bundle) = load_security_bundle(db)? {
            if bundle.mode == EncryptionMode::Encrypted {
                return Err(AppError::invalid_input(
                    "Encryption is already enabled and cannot be downgraded in-place",
                ));
            }
        }
        let bundle = SecurityBundle::unencrypted();
        save_security_bundle(db, &bundle)?;
    }
    state.security_runtime.lock().await.master_key = None;
    Ok(())
}

#[tauri::command]
pub async fn initialize_encryption(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<InitializeEncryptionResponse, AppError> {
    // Rebound rather than taken as `Zeroizing<String>` directly: a Tauri command
    // parameter has to deserialize, and moving the `String` in here keeps the same
    // heap buffer, so the secret is wiped when this returns. Whatever copies serde
    // made while parsing the IPC payload are outside our reach either way.
    let passphrase = Zeroizing::new(passphrase);
    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        if let Some(bundle) = load_security_bundle(db)? {
            if bundle.mode == EncryptionMode::Encrypted {
                return Err(AppError::invalid_input("Encryption is already enabled"));
            }
        }
    }

    let (bundle, recovery_key, master_key) =
        SecurityBundle::new_encrypted(&passphrase).map_err(AppError::from)?;

    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        save_security_bundle(db, &bundle)?;
    }

    state.security_runtime.lock().await.master_key = Some(master_key);

    // The recovery key has to reach the user, who is expected to write it down, so
    // this is the one copy that cannot be zeroized: past the serializer it belongs to
    // the frontend.
    Ok(InitializeEncryptionResponse {
        recovery_key: recovery_key.to_string(),
    })
}

#[tauri::command]
pub async fn unlock_encryption(
    passphrase: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    let passphrase = Zeroizing::new(passphrase);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| AppError::invalid_input("Encryption is not initialized for this library"))?;
    if bundle.mode != EncryptionMode::Encrypted {
        return Err(AppError::invalid_input("Encryption mode is not enabled"));
    }
    let key = bundle
        .unlock_with_passphrase(&passphrase)
        .map_err(AppError::from)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    connect_telegram_after_unlock(&state, &app).await;
    Ok(())
}

#[tauri::command]
pub async fn lock_encryption(state: State<'_, AppState>) -> Result<(), AppError> {
    state.security_runtime.lock().await.master_key = None;
    // Clearing the key in memory was the whole of "lock" before this, which left every
    // thumbnail and every media file the user had viewed sitting decrypted in %TEMP%.
    // Locking has to mean the plaintext is gone, not just the key.
    let purged = paths::purge_scratch_dirs();
    log::info!(
        "Locked encryption and purged {} scratch directories",
        purged
    );
    Ok(())
}

#[tauri::command]
pub async fn recover_encryption(
    recovery_key: String,
    new_passphrase: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<RegenerateRecoveryResponse, AppError> {
    let recovery_key = Zeroizing::new(recovery_key);
    let new_passphrase = Zeroizing::new(new_passphrase);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| AppError::invalid_input("Encryption is not initialized for this library"))?;
    // The recovery key that was just used is retired, so a fresh one comes back
    // for the caller to show once. Nothing else can produce it later.
    let (next_bundle, new_recovery_key, key) = bundle
        .recover_and_rewrap(&recovery_key, &new_passphrase)
        .map_err(AppError::from)?;
    save_security_bundle(db, &next_bundle)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    connect_telegram_after_unlock(&state, &app).await;
    Ok(RegenerateRecoveryResponse {
        recovery_key: new_recovery_key.to_string(),
    })
}

/// Change the passphrase for someone who still knows the current one.
///
/// Separate from `recover_encryption`, which is the path for someone who does
/// not: this one requires the old passphrase and leaves the recovery key alone.
#[tauri::command]
pub async fn change_passphrase(
    current_passphrase: String,
    new_passphrase: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let current_passphrase = Zeroizing::new(current_passphrase);
    let new_passphrase = Zeroizing::new(new_passphrase);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| AppError::invalid_input("Encryption is not initialized for this library"))?;
    let (next_bundle, key) = bundle
        .change_passphrase(&current_passphrase, &new_passphrase)
        .map_err(AppError::from)?;
    save_security_bundle(db, &next_bundle)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    Ok(())
}

#[tauri::command]
pub async fn regenerate_recovery_key(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<RegenerateRecoveryResponse, AppError> {
    let passphrase = Zeroizing::new(passphrase);
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| AppError::invalid_input("Encryption is not initialized for this library"))?;
    let (next_bundle, recovery_key, key) = bundle
        .regenerate_recovery_key(&passphrase)
        .map_err(AppError::from)?;
    save_security_bundle(db, &next_bundle)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    Ok(RegenerateRecoveryResponse {
        recovery_key: recovery_key.to_string(),
    })
}

#[tauri::command]
pub async fn complete_onboarding(state: State<'_, AppState>) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.set_config(SECURITY_ONBOARDING_COMPLETE_KEY, "true")
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn get_encryption_migration_status(
    state: State<'_, AppState>,
) -> Result<MigrationStatus, AppError> {
    let runtime_status = state.security_runtime.lock().await.migration.clone();
    if runtime_status.total == 0
        && runtime_status.processed == 0
        && runtime_status.succeeded == 0
        && runtime_status.failed == 0
    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        Ok(load_migration_status(db))
    } else {
        Ok(runtime_status)
    }
}

/// Retry deleting the plaintext copies the migration could not confirm gone.
///
/// Returns how many are confirmed deleted by this attempt. The backlog is the only
/// record that unencrypted media is still in the cloud, so it is rewritten from what
/// survived rather than simply cleared.
#[tauri::command]
pub async fn retry_plaintext_purge(state: State<'_, AppState>) -> Result<usize, AppError> {
    let pending = state
        .security_runtime
        .lock()
        .await
        .migration
        .unpurged_plaintext
        .clone();
    if pending.is_empty() {
        return Ok(0);
    }
    if !state.telegram.is_connected().await {
        return Err(AppError::unavailable(
            "Connect to Telegram before retrying the purge",
        ));
    }

    let surviving = state.telegram.purge_messages(&pending).await?;
    let purged = pending.len() - surviving.len();

    let status = {
        let mut guard = state.security_runtime.lock().await;
        // Ids that were not part of this attempt are left alone: a migration running
        // alongside this may have added one since the list was copied.
        guard
            .migration
            .unpurged_plaintext
            .retain(|id| surviving.contains(id) || !pending.contains(id));
        guard.migration.clone()
    };

    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    save_migration_status(db, &status)?;

    log::info!("Retried the plaintext purge: {} confirmed deleted", purged);
    Ok(purged)
}

#[tauri::command]
pub async fn start_encryption_migration(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    let db = {
        let db_guard = state.db.lock().await;
        db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?
            .clone()
    };

    let bundle = load_security_bundle(&db)?
        .ok_or_else(|| AppError::invalid_input("Encryption is not initialized for this library"))?;
    if bundle.mode != EncryptionMode::Encrypted {
        return Err(AppError::invalid_input("Encryption mode is not enabled"));
    }

    let key = state
        .security_runtime
        .lock()
        .await
        .master_key
        .clone()
        .ok_or_else(|| AppError::vault_locked("Unlock encryption before starting migration"))?;

    // Resolved once here and moved into the worker below, so every unlink the
    // migration performs is confined to the managed library.
    let managed_roots = paths::managed_roots(&resolve_app_data_dir(&app)?);

    let cloud_items = db
        .get_uploaded_unencrypted_media(1_000_000)
        .map_err(AppError::from)?;
    let thumb_items = db
        .get_unencrypted_thumbnail_paths(1_000_000)
        .map_err(AppError::from)?;

    {
        let mut runtime = state.security_runtime.lock().await;
        if runtime.migration_worker_active {
            return Ok(());
        }
        runtime.migration_worker_active = true;
        runtime.migration = MigrationStatus {
            running: true,
            total: (cloud_items.len() + thumb_items.len()) as i64,
            processed: 0,
            succeeded: 0,
            failed: 0,
            last_error: None,
            // Carried across runs: an unpurged plaintext copy is not something a
            // fresh migration pass fixes, so starting over must not forget it.
            unpurged_plaintext: runtime.migration.unpurged_plaintext.clone(),
        };
        let _ = save_migration_status(&db, &runtime.migration);
    }

    let runtime = state.security_runtime.clone();
    let telegram = state.telegram.clone();
    let pending_prefix = SECURITY_MIGRATION_PENDING_PREFIX.to_string();

    tokio::spawn(async move {
        for (media_id, thumb_path) in thumb_items {
            let result = match ensure_thumbnail_encrypted(&thumb_path, &key, &managed_roots) {
                Ok(Some(new_path)) => {
                    let new_path_str = new_path.to_string_lossy().to_string();
                    if new_path_str != thumb_path {
                        db.update_thumbnail_path(media_id, &new_path_str)
                            .map_err(AppError::from)
                            .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            };

            let mut state_guard = runtime.lock().await;
            state_guard.migration.processed += 1;
            match result {
                Ok(_) => state_guard.migration.succeeded += 1,
                Err(err) => {
                    state_guard.migration.failed += 1;
                    // The status is persisted and shown, so it keeps the message the
                    // user can act on rather than the error object.
                    state_guard.migration.last_error = Some(err.to_string());
                }
            }
            let _ = save_migration_status(&db, &state_guard.migration);
        }

        for (media_id, file_path, previous_tg_id, thumbnail_path) in cloud_items {
            let pending_key = format!("{}{}", pending_prefix, media_id);

            // `Ok(Some(id))` means the media itself migrated but the old plaintext
            // message is still in Telegram.
            let result: Result<Option<i32>, AppError> = async {
                if let Some(thumb_path) = thumbnail_path.as_deref() {
                    if let Some(new_thumb) =
                        ensure_thumbnail_encrypted(thumb_path, &key, &managed_roots)?
                    {
                        let new_thumb_str = new_thumb.to_string_lossy().to_string();
                        if new_thumb_str != thumb_path {
                            db.update_thumbnail_path(media_id, &new_thumb_str)
                                .map_err(AppError::from)?;
                        }
                    }
                }

                let maybe_pending = db
                    .get_config(&pending_key)
                    .map_err(AppError::from)?
                    .and_then(|v| v.parse::<i32>().ok());

                let new_msg_id = if let Some(id) = maybe_pending {
                    id
                } else {
                    let source = std::path::Path::new(&file_path);
                    if !source.exists() {
                        return Err(AppError::not_found(
                            "Local file is missing; cannot migrate cloud blob",
                        ));
                    }

                    let temp_dir = paths::migration_staging_dir();
                    std::fs::create_dir_all(&temp_dir).map_err(AppError::from)?;
                    let temp_path = temp_dir.join(format!("media_{}_enc.wbenc", media_id));
                    // Encrypting the original is whole-file crypto on a media file, and
                    // this worker shares the runtime with every command the user is
                    // still issuing while the migration runs.
                    let encrypt_src = source.to_path_buf();
                    let encrypt_dst = temp_path.clone();
                    let encrypt_key = key.clone();
                    tokio::task::spawn_blocking(move || {
                        security::encrypt_file(&encrypt_src, &encrypt_dst, &encrypt_key)
                    })
                    .await
                    .map_err(|e| format!("Encrypt task failed: {}", e))?
                    .map_err(AppError::from)?;

                    let temp_path_str = temp_path.to_string_lossy().to_string();
                    let upload_res = telegram
                        .upload_file_with_progress(&temp_path_str, |_bytes, _total, _speed| {})
                        .await;
                    let _ = std::fs::remove_file(&temp_path);

                    let uploaded_id = upload_res.map_err(|e| AppError::telegram(e.to_string()))?;
                    db.set_config(&pending_key, &uploaded_id.to_string())
                        .map_err(AppError::from)?;
                    uploaded_id
                };

                db.update_telegram_id_by_path(&file_path, &new_msg_id.to_string())
                    .map_err(AppError::from)?;
                db.mark_media_encrypted_by_id(media_id)
                    .map_err(AppError::from)?;

                // The old message holds the unencrypted original. Deleting it is the
                // point of the migration, so its result is checked rather than
                // discarded, and an id that survives is handed back to be recorded.
                let mut unpurged = None;
                if let Ok(old_id) = previous_tg_id.parse::<i32>() {
                    if old_id != new_msg_id {
                        match telegram.purge_messages(&[old_id]).await {
                            Ok(surviving) if surviving.is_empty() => {}
                            Ok(_) => {
                                log::error!(
                                    "Plaintext copy of media {} is still in Telegram",
                                    media_id
                                );
                                unpurged = Some(old_id);
                            }
                            Err(e) => {
                                log::error!(
                                    "Could not confirm the plaintext purge for media {}: {}",
                                    media_id,
                                    e
                                );
                                unpurged = Some(old_id);
                            }
                        }
                    }
                }

                let _ = db.remove_config(&pending_key);
                Ok(unpurged)
            }
            .await;

            let mut state_guard = runtime.lock().await;
            state_guard.migration.processed += 1;
            match result {
                Ok(unpurged) => {
                    state_guard.migration.succeeded += 1;
                    if let Some(id) = unpurged {
                        if !state_guard.migration.unpurged_plaintext.contains(&id) {
                            state_guard.migration.unpurged_plaintext.push(id);
                        }
                    }
                }
                Err(err) => {
                    state_guard.migration.failed += 1;
                    // The status is persisted and shown, so it keeps the message the
                    // user can act on rather than the error object.
                    state_guard.migration.last_error = Some(err.to_string());
                }
            }
            let _ = save_migration_status(&db, &state_guard.migration);
        }

        let mut state_guard = runtime.lock().await;
        state_guard.migration.running = false;
        state_guard.migration_worker_active = false;
        let _ = save_migration_status(&db, &state_guard.migration);
    });

    Ok(())
}
