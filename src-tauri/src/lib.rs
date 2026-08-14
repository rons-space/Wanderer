mod ai;
mod backup;
mod clip;
mod database;
mod errors;
mod media_utils;
mod metadata;
mod paths;
mod progress_stream;
mod raw_support;
mod security;
mod sync_manifest;
mod sync_worker;
mod telegram;
mod upload_worker;
mod view_cache;
mod watcher;

use database::Database;
use security::{
    EncryptionMode, MigrationStatus, RuntimeState, SecurityBundle, TelegramApiCredentials,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use telegram::TelegramService;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct AppState {
    telegram: Arc<TelegramService>,
    db: Mutex<Option<Arc<Database>>>,
    watcher: Mutex<Option<watcher::FileWatcher>>,
    security_runtime: Arc<Mutex<RuntimeState>>,
    /// Face detector is optional - AI features gracefully degrade if model fails to load
    face_detector: Option<Arc<Mutex<ai::FaceDetector>>>,
}

const APP_DATA_FALLBACK_DIR_NAME: &str = "com.wanderer.desktop";
const SECURITY_BUNDLE_KEY: &str = "security_bundle_v1";
const SECURITY_MODE_KEY: &str = "security_mode";
const SECURITY_ONBOARDING_COMPLETE_KEY: &str = "security_onboarding_complete";
const TELEGRAM_CREDS_KEY: &str = "security_telegram_credentials";
const SECURITY_MIGRATION_STATUS_KEY: &str = "security_migration_status";
const SECURITY_MIGRATION_PENDING_PREFIX: &str = "security_migration_pending_new_msg_";

/// Prefix for every `config` row that holds security state.
///
/// `security_bundle_v1` carries the Argon2id salts and both wrapped copies of
/// the master key, and `security_telegram_credentials` carries the DPAPI blob.
/// These rows are written only by the dedicated security commands and must
/// never be readable from the webview, which is one `execute_js` or one
/// compromised dependency away from being attacker-controlled.
const SECURITY_KEY_PREFIX: &str = "security_";

/// True for config keys the webview may neither read nor write.
fn is_security_key(key: &str) -> bool {
    key.starts_with(SECURITY_KEY_PREFIX)
}

fn fallback_app_data_dir() -> Result<std::path::PathBuf, String> {
    let base =
        dirs::data_local_dir().ok_or_else(|| "Could not find local data directory".to_string())?;
    Ok(base.join(APP_DATA_FALLBACK_DIR_NAME))
}

fn resolve_app_data_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let app_data_dir = match app.path().app_local_data_dir() {
        Ok(path) => path,
        Err(e) => {
            let fallback = fallback_app_data_dir()?;
            log::warn!(
                "Failed to resolve Tauri app_local_data_dir ({}), falling back to {:?}",
                e,
                fallback
            );
            fallback
        }
    };

    std::fs::create_dir_all(&app_data_dir).map_err(|e| e.to_string())?;
    log::debug!("Using app data directory at {:?}", app_data_dir);
    Ok(app_data_dir)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecurityStatusResponse {
    onboarding_complete: bool,
    security_mode: String,
    encryption_configured: bool,
    encryption_locked: bool,
    telegram_credentials_configured: bool,
    migration: MigrationStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeEncryptionResponse {
    recovery_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegenerateRecoveryResponse {
    recovery_key: String,
}

/// The authoritative answer to "must this data be encrypted?".
///
/// Always derived from the `SecurityBundle`, never from the duplicated
/// `security_mode` row: the two are written by separate, non-transactional
/// statements, so a crash or a transient error between them leaves the bundle
/// saying `encrypted` while the mode row is absent. Reading the mode row with
/// `.ok().flatten().unwrap_or("unset")` then answered "not encrypted" and the
/// caller uploaded the plaintext original.
///
/// Returns `Err` when the bundle cannot be read or parsed. Callers must fail
/// closed on that: defer the work, never fall back to sending plaintext.
pub(crate) fn encryption_required(db: &Database) -> Result<bool, String> {
    Ok(load_security_bundle(db)?
        .map(|b| b.mode == EncryptionMode::Encrypted)
        .unwrap_or(false))
}

pub(crate) fn load_security_bundle(db: &Database) -> Result<Option<SecurityBundle>, String> {
    let raw = db
        .get_config(SECURITY_BUNDLE_KEY)
        .map_err(|e| e.to_string())?;
    match raw {
        Some(json) => serde_json::from_str::<SecurityBundle>(&json)
            .map(Some)
            .map_err(|e| format!("Invalid security bundle: {}", e)),
        None => Ok(None),
    }
}

fn save_security_bundle(db: &Database, bundle: &SecurityBundle) -> Result<(), String> {
    let json = serde_json::to_string(bundle).map_err(|e| e.to_string())?;
    db.set_config(SECURITY_BUNDLE_KEY, &json)
        .map_err(|e| e.to_string())?;
    let mode = match bundle.mode {
        EncryptionMode::Encrypted => "encrypted",
        EncryptionMode::Unencrypted => "unencrypted",
    };
    // Kept only so an older build installed over this one still finds the row it
    // expects. Nothing in this codebase reads it as a decision input any more:
    // use `encryption_required`, which reads the bundle above.
    db.set_config(SECURITY_MODE_KEY, mode)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_migration_status(db: &Database) -> MigrationStatus {
    db.get_config(SECURITY_MIGRATION_STATUS_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<MigrationStatus>(&json).ok())
        .unwrap_or_default()
}

fn save_migration_status(db: &Database, status: &MigrationStatus) -> Result<(), String> {
    let json = serde_json::to_string(status).map_err(|e| e.to_string())?;
    db.set_config(SECURITY_MIGRATION_STATUS_KEY, &json)
        .map_err(|e| e.to_string())
}

fn ensure_thumbnail_encrypted(
    thumb_path: &str,
    key: &[u8; 32],
    roots: &[std::path::PathBuf],
) -> Result<Option<std::path::PathBuf>, String> {
    let path = std::path::Path::new(thumb_path);

    // `thumbnail_path` is a text column, and this function deletes the file it
    // reads, so a poisoned row would mean encrypting and then unlinking an
    // arbitrary file.
    if !paths::is_within_any(roots, path) {
        return Err(format!(
            "Refusing to encrypt a thumbnail outside the managed library: {}",
            thumb_path
        ));
    }

    if !path.exists() {
        return Ok(None);
    }

    if security::is_encrypted_file(path).map_err(|e| e.to_string())? {
        return Ok(Some(path.to_path_buf()));
    }

    let encrypted_path = path.with_extension("wbenc");
    security::encrypt_file(path, &encrypted_path, key).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(path);
    Ok(Some(encrypted_path))
}

async fn materialize_thumbnail_path_for_response(
    thumbnail_path: Option<String>,
    state: &State<'_, AppState>,
) -> Option<String> {
    let thumbnail_path = thumbnail_path?;
    let src = std::path::PathBuf::from(&thumbnail_path);
    if !src.exists() {
        return None;
    }

    let is_encrypted = security::is_encrypted_file(&src).ok().unwrap_or(false);
    if !is_encrypted {
        return Some(thumbnail_path);
    }

    let key = state.security_runtime.lock().await.master_key?;
    let cache_dir = std::env::temp_dir().join("wanderer-thumb-cache");
    if std::fs::create_dir_all(&cache_dir).is_err() {
        return None;
    }

    let cache_key = blake3::hash(src.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    let output = cache_dir.join(format!("{}.jpg", cache_key));

    let needs_refresh = if output.exists() {
        let src_m = std::fs::metadata(&src).and_then(|m| m.modified());
        let out_m = std::fs::metadata(&output).and_then(|m| m.modified());
        match (src_m, out_m) {
            (Ok(s), Ok(o)) => s > o,
            _ => true,
        }
    } else {
        true
    };

    if needs_refresh && security::decrypt_file(&src, &output, &key).is_err() {
        return None;
    }

    Some(output.to_string_lossy().to_string())
}

async fn materialize_media_items_for_response(
    mut items: Vec<database::MediaItem>,
    state: &State<'_, AppState>,
) -> Vec<database::MediaItem> {
    for item in &mut items {
        item.thumbnail_path =
            materialize_thumbnail_path_for_response(item.thumbnail_path.clone(), state).await;
    }
    items
}

async fn get_active_master_key(state: &State<'_, AppState>) -> Option<[u8; 32]> {
    state.security_runtime.lock().await.master_key
}

async fn download_and_materialize_media(
    state: &State<'_, AppState>,
    msg_id: i32,
    final_path: &std::path::Path,
) -> Result<(), String> {
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let temp_dir = std::env::temp_dir().join("wanderer-download-staging");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let temp_path = temp_dir.join(format!(
        "msg_{}_{}.bin",
        msg_id,
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let temp_path_str = temp_path.to_string_lossy().to_string();

    state
        .telegram
        .download_by_message_id(msg_id, &temp_path_str)
        .await
        .map_err(|e| format!("Failed to download from Telegram: {}", e))?;

    let maybe_key = get_active_master_key(state).await;
    let result = security::decrypt_file_if_needed(&temp_path, final_path, maybe_key.as_ref())
        .map_err(|e| e.to_string());

    let _ = std::fs::remove_file(&temp_path);
    result.map(|_| ())
}

async fn get_security_status_inner(
    state: &State<'_, AppState>,
) -> Result<SecurityStatusResponse, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let onboarding_complete = db
        .get_config(SECURITY_ONBOARDING_COMPLETE_KEY)
        .map_err(|e| e.to_string())?
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let bundle = load_security_bundle(db)?;
    let encryption_configured = bundle
        .as_ref()
        .map(|b| b.mode == EncryptionMode::Encrypted)
        .unwrap_or(false);

    // Derived from the bundle, like everything else now. Reporting this from the
    // `security_mode` row was how the UI could say "encrypted" while the workers
    // read the same row and disagreed.
    let mode = match bundle.as_ref().map(|b| &b.mode) {
        Some(EncryptionMode::Encrypted) => "encrypted".to_string(),
        Some(EncryptionMode::Unencrypted) => "unencrypted".to_string(),
        None => "unset".to_string(),
    };
    let encryption_locked = if encryption_configured {
        state.security_runtime.lock().await.master_key.is_none()
    } else {
        false
    };

    let telegram_credentials_configured = db
        .get_config(TELEGRAM_CREDS_KEY)
        .map_err(|e| e.to_string())?
        .is_some();

    let runtime_migration = state.security_runtime.lock().await.migration.clone();
    let migration = if runtime_migration.total == 0
        && runtime_migration.processed == 0
        && runtime_migration.succeeded == 0
        && runtime_migration.failed == 0
    {
        load_migration_status(db)
    } else {
        runtime_migration
    };

    Ok(SecurityStatusResponse {
        onboarding_complete,
        security_mode: mode,
        encryption_configured,
        encryption_locked,
        telegram_credentials_configured,
        migration,
    })
}

#[tauri::command]
async fn get_security_status(state: State<'_, AppState>) -> Result<SecurityStatusResponse, String> {
    get_security_status_inner(&state).await
}

#[tauri::command]
async fn initialize_unencrypted_mode(state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    if let Some(bundle) = load_security_bundle(db)? {
        if bundle.mode == EncryptionMode::Encrypted {
            return Err(
                "Encryption is already enabled and cannot be downgraded in-place".to_string(),
            );
        }
    }
    let bundle = SecurityBundle::unencrypted();
    save_security_bundle(db, &bundle)?;
    state.security_runtime.lock().await.master_key = None;
    Ok(())
}

#[tauri::command]
async fn initialize_encryption(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<InitializeEncryptionResponse, String> {
    {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        if let Some(bundle) = load_security_bundle(db)? {
            if bundle.mode == EncryptionMode::Encrypted {
                return Err("Encryption is already enabled".to_string());
            }
        }
    }

    let (bundle, recovery_key, master_key) =
        SecurityBundle::new_encrypted(&passphrase).map_err(|e| e.to_string())?;

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    save_security_bundle(db, &bundle)?;

    state.security_runtime.lock().await.master_key = Some(master_key);

    Ok(InitializeEncryptionResponse { recovery_key })
}

#[tauri::command]
async fn unlock_encryption(passphrase: String, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| "Encryption is not initialized for this library".to_string())?;
    if bundle.mode != EncryptionMode::Encrypted {
        return Err("Encryption mode is not enabled".to_string());
    }
    let key = bundle
        .unlock_with_passphrase(&passphrase)
        .map_err(|e| e.to_string())?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    Ok(())
}

#[tauri::command]
async fn lock_encryption(state: State<'_, AppState>) -> Result<(), String> {
    state.security_runtime.lock().await.master_key = None;
    Ok(())
}

#[tauri::command]
async fn recover_encryption(
    recovery_key: String,
    new_passphrase: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| "Encryption is not initialized for this library".to_string())?;
    let (next_bundle, key) = bundle
        .recover_and_rewrap(&recovery_key, &new_passphrase)
        .map_err(|e| e.to_string())?;
    save_security_bundle(db, &next_bundle)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    Ok(())
}

#[tauri::command]
async fn regenerate_recovery_key(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<RegenerateRecoveryResponse, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let bundle = load_security_bundle(db)?
        .ok_or_else(|| "Encryption is not initialized for this library".to_string())?;
    let (next_bundle, recovery_key, key) = bundle
        .regenerate_recovery_key(&passphrase)
        .map_err(|e| e.to_string())?;
    save_security_bundle(db, &next_bundle)?;
    drop(db_guard);
    state.security_runtime.lock().await.master_key = Some(key);
    Ok(RegenerateRecoveryResponse { recovery_key })
}

#[tauri::command]
async fn complete_onboarding(state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.set_config(SECURITY_ONBOARDING_COMPLETE_KEY, "true")
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_telegram_api_credentials(
    api_id: i32,
    api_hash: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if api_id <= 0 {
        return Err("API ID must be a positive integer".to_string());
    }
    if api_hash.trim().len() < 8 {
        return Err("API hash is invalid".to_string());
    }

    let creds = TelegramApiCredentials {
        api_id,
        api_hash: api_hash.trim().to_string(),
    };

    let protected_blob = security::serialize_and_protect(&creds, "wanderer-telegram-credentials")
        .map_err(|e| e.to_string())?;

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.set_config(TELEGRAM_CREDS_KEY, &protected_blob)
        .map_err(|e| e.to_string())?;
    drop(db_guard);

    state
        .telegram
        .set_credentials(creds.api_id, creds.api_hash.clone())
        .await;
    Ok(())
}

#[tauri::command]
async fn clear_telegram_api_credentials(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        db.remove_config(TELEGRAM_CREDS_KEY)
            .map_err(|e| e.to_string())?;
    }
    let app_dir = resolve_app_data_dir(&app)?;
    let _ = state.telegram.logout(app_dir).await;
    state.telegram.clear_credentials().await;
    Ok(())
}

#[tauri::command]
async fn get_encryption_migration_status(
    state: State<'_, AppState>,
) -> Result<MigrationStatus, String> {
    let runtime_status = state.security_runtime.lock().await.migration.clone();
    if runtime_status.total == 0
        && runtime_status.processed == 0
        && runtime_status.succeeded == 0
        && runtime_status.failed == 0
    {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        Ok(load_migration_status(db))
    } else {
        Ok(runtime_status)
    }
}

#[tauri::command]
async fn start_encryption_migration(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let db = {
        let db_guard = state.db.lock().await;
        db_guard.as_ref().ok_or("Database not initialized")?.clone()
    };

    let bundle = load_security_bundle(&db)?
        .ok_or_else(|| "Encryption is not initialized for this library".to_string())?;
    if bundle.mode != EncryptionMode::Encrypted {
        return Err("Encryption mode is not enabled".to_string());
    }

    let key = state
        .security_runtime
        .lock()
        .await
        .master_key
        .ok_or_else(|| "Unlock encryption before starting migration".to_string())?;

    // Resolved once here and moved into the worker below, so every unlink the
    // migration performs is confined to the managed library.
    let managed_roots = paths::managed_roots(&resolve_app_data_dir(&app)?);

    let cloud_items = db
        .get_uploaded_unencrypted_media(1_000_000)
        .map_err(|e| e.to_string())?;
    let thumb_items = db
        .get_unencrypted_thumbnail_paths(1_000_000)
        .map_err(|e| e.to_string())?;

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
                            .map_err(|e| e.to_string())
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
                    state_guard.migration.last_error = Some(err);
                }
            }
            let _ = save_migration_status(&db, &state_guard.migration);
        }

        for (media_id, file_path, previous_tg_id, thumbnail_path) in cloud_items {
            let pending_key = format!("{}{}", pending_prefix, media_id);

            let result: Result<(), String> = async {
                if let Some(thumb_path) = thumbnail_path.as_deref() {
                    if let Some(new_thumb) =
                        ensure_thumbnail_encrypted(thumb_path, &key, &managed_roots)?
                    {
                        let new_thumb_str = new_thumb.to_string_lossy().to_string();
                        if new_thumb_str != thumb_path {
                            db.update_thumbnail_path(media_id, &new_thumb_str)
                                .map_err(|e| e.to_string())?;
                        }
                    }
                }

                let maybe_pending = db
                    .get_config(&pending_key)
                    .map_err(|e| e.to_string())?
                    .and_then(|v| v.parse::<i32>().ok());

                let new_msg_id = if let Some(id) = maybe_pending {
                    id
                } else {
                    let source = std::path::Path::new(&file_path);
                    if !source.exists() {
                        return Err("Local file is missing; cannot migrate cloud blob".to_string());
                    }

                    let temp_dir = std::env::temp_dir().join("wanderer-migration");
                    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
                    let temp_path = temp_dir.join(format!("media_{}_enc.wbenc", media_id));
                    security::encrypt_file(source, &temp_path, &key).map_err(|e| e.to_string())?;

                    let temp_path_str = temp_path.to_string_lossy().to_string();
                    let upload_res = telegram
                        .upload_file_with_progress(&temp_path_str, |_bytes, _total, _speed| {})
                        .await;
                    let _ = std::fs::remove_file(&temp_path);

                    let uploaded_id = upload_res.map_err(|e| e.to_string())?;
                    db.set_config(&pending_key, &uploaded_id.to_string())
                        .map_err(|e| e.to_string())?;
                    uploaded_id
                };

                db.update_telegram_id_by_path(&file_path, &new_msg_id.to_string())
                    .map_err(|e| e.to_string())?;
                db.mark_media_encrypted_by_id(media_id)
                    .map_err(|e| e.to_string())?;

                if let Ok(old_id) = previous_tg_id.parse::<i32>() {
                    if old_id != new_msg_id {
                        let _ = telegram.delete_messages(&[old_id]).await;
                    }
                }

                let _ = db.remove_config(&pending_key);
                Ok(())
            }
            .await;

            let mut state_guard = runtime.lock().await;
            state_guard.migration.processed += 1;
            match result {
                Ok(_) => state_guard.migration.succeeded += 1,
                Err(err) => {
                    state_guard.migration.failed += 1;
                    state_guard.migration.last_error = Some(err);
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

#[tauri::command]
async fn login_request_code(
    phone: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if !state.telegram.has_credentials().await {
        return Err(
            "Telegram API credentials are not configured. Complete onboarding first.".to_string(),
        );
    }
    let app_dir = resolve_app_data_dir(&app)?;

    match state.telegram.request_code(&phone, app_dir).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn login_sign_in(code: String, state: State<'_, AppState>) -> Result<String, String> {
    state.telegram.sign_in(&code).await
}

#[tauri::command]
async fn get_me(state: State<'_, AppState>) -> Result<String, String> {
    if !state.telegram.has_credentials().await {
        return Err("Telegram API credentials are not configured".to_string());
    }
    state.telegram.get_me().await
}

#[tauri::command]
async fn logout(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let app_dir = resolve_app_data_dir(&app)?;

    state.telegram.logout(app_dir).await
}

#[tauri::command]
async fn get_media(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let _db = db_guard.as_ref().ok_or("Database not initialized")?;

    println!(
        "Command: get_media called with limit={}, offset={}",
        limit, offset
    );
    let result = _db.get_media(limit, offset).map_err(|e| e.to_string());
    match &result {
        Ok(items) => println!("Command: get_media returning {} items", items.len()),
        Err(e) => println!("Command: get_media failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn search_media(
    query: String,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let filters = database::SearchFilters::default();
    let items = db
        .search_fts(&query, &filters, limit, offset)
        .map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn search_fts(
    query: String,
    filters: database::SearchFilters,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    println!(
        "Command: search_fts called with query='{}', has_location={:?}",
        query, filters.has_location
    );
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let result = db
        .search_fts(&query, &filters, limit, offset)
        .map_err(|e| e.to_string());

    match &result {
        Ok(items) => println!("Command: search_fts returning {} items", items.len()),
        Err(e) => println!("Command: search_fts failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn create_album(name: String, state: State<'_, AppState>) -> Result<i64, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.create_album(&name).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_albums(state: State<'_, AppState>) -> Result<Vec<database::Album>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_albums().map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_media_to_album(
    album_id: i64,
    media_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    println!(
        "Command: add_media_to_album called with album_id={}, media_id={}",
        album_id, media_id
    );
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.add_media_to_album(album_id, media_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_album_media(
    album_id: i64,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    println!(
        "Command: get_album_media called with album_id={}, limit={}, offset={}",
        album_id, limit, offset
    );
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let result = db
        .get_album_media(album_id, limit, offset)
        .map_err(|e| e.to_string());
    match &result {
        Ok(items) => println!("Command: get_album_media returning {} items", items.len()),
        Err(e) => println!("Command: get_album_media failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn detect_faces(state: State<'_, AppState>, path: String) -> Result<Vec<ai::Face>, String> {
    let detector = match &state.face_detector {
        Some(d) => d.clone(),
        None => return Err("AI face detection is not available".to_string()),
    };
    let path_buf = std::path::PathBuf::from(path);

    // Offload CPU-intensive task to a blocking thread
    let join_handle = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ai::Face>> {
        let detector = detector.blocking_lock();
        detector.detect(&path_buf)
    });

    match join_handle.await {
        Ok(detection_res) => detection_res.map_err(|e: anyhow::Error| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn get_faces(state: State<'_, AppState>, media_id: i64) -> Result<Vec<ai::Face>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_faces(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn debug_reset_faces(state: State<'_, AppState>) -> Result<usize, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.reset_all_scans().map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // TODO: Load from config/env
    // Load .env file if it exists
    dotenvy::dotenv().ok();
    let telegram_service = Arc::new(TelegramService::new());
    let security_runtime = Arc::new(Mutex::new(RuntimeState::default()));

    // Initialize AI Face Detector - gracefully degrade if unavailable
    let face_detector: Option<Arc<Mutex<ai::FaceDetector>>> = match ai::FaceDetector::new() {
        Ok(fd) => {
            log::info!("Face detection initialized successfully");
            Some(Arc::new(Mutex::new(fd)))
        }
        Err(e) => {
            log::warn!("Face detection unavailable: {}. AI features disabled.", e);
            None
        }
    };

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init());

    // The MCP bridge is a development tool and must never reach a distributed
    // build. Its WebSocket server has no authentication and exposes `execute_js`,
    // so anyone who can reach the port can run arbitrary script in the webview and
    // from there call every command registered below, including
    // `unlock_encryption`, `get_all_config` and `permanent_delete_media`.
    //
    // Two gates, because either one alone is easy to defeat by accident: the
    // dependency is an opt-in feature (see `Cargo.toml`), and registration is
    // additionally restricted to debug builds. `localhost_only()` replaces the
    // plugin's `0.0.0.0` default so that even in development it is not reachable
    // from the local network.
    #[cfg(all(debug_assertions, feature = "mcp-bridge"))]
    let builder = builder.plugin(tauri_plugin_mcp_bridge::init_with_config(
        tauri_plugin_mcp_bridge::Config::localhost_only(),
    ));

    builder
        .manage(AppState {
            telegram: telegram_service,
            db: Mutex::new(None),
            watcher: Mutex::new(None),
            security_runtime,
            face_detector: face_detector,
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();

            tauri::async_runtime::spawn(async move {
                // Initialize State with cache
                // Note: We need to manage how state is initialized if we changed the struct
                // actually `manage` call is usually done before setup or passed via .manage()
                // But here we are accessing state that was managed?
                // Wait, tauri::Builder::default().manage(AppState { ... }) is missing in the visible code!
                // Ah, the view showed `.setup`.
                // I need to see where `AppState` is created.
                // Usually it's `.manage(AppState::default())` or similar.

                // Let's look at the full file content of lib.rs again to check `manage`.
                // If I can't find it, I will assume I need to add it or modify it.
                // Assuming `manage` is called with initial state.

                // Wait, the previous view of lib.rs showed:
                // let state: tauri::State<AppState> = app_handle.state();
                // This means state is ALREADY managed.
                // I need to find where `.manage` is called to update the INITIALIZATION.

                // If I change AppState struct, I MUST update the `.manage(...)` call.
                // Let's find it.

                // I will do a `view_file` regarding this in next step if I can't find it.
                // But let's look at lines 100-150 relative to previous view.

                let state: tauri::State<AppState> = app_handle.state();

                let app_dir = match resolve_app_data_dir(&app_handle) {
                    Ok(dir) => dir,
                    Err(e) => {
                        eprintln!("Failed to resolve app data directory: {}", e);
                        return;
                    }
                };

                let db_path = app_dir.join("library.db");

                // Initialize Database
                let db_arc = match Database::new(&db_path) {
                    Ok(db) => {
                        let arc = Arc::new(db);
                        *state.db.lock().await = Some(arc.clone());
                        println!("Database initialized at {:?}", db_path);
                        Some(arc)
                    }
                    Err(e) => {
                        eprintln!("Failed to initialize database: {}", e);
                        None
                    }
                };

                if let Some(db) = db_arc {
                    // Load persisted security mode/bundle.
                    match load_security_bundle(&db) {
                        Ok(Some(bundle)) if bundle.mode == EncryptionMode::Encrypted => {
                            state.security_runtime.lock().await.master_key = None;
                            log::info!("Encryption enabled for this library (vault locked)");
                        }
                        Ok(Some(_)) | Ok(None) => {
                            state.security_runtime.lock().await.master_key = None;
                        }
                        Err(e) => {
                            log::warn!("Failed to load security bundle: {}", e);
                        }
                    }
                    state.security_runtime.lock().await.migration = load_migration_status(&db);

                    // Load BYOK Telegram API credentials from DPAPI-protected config.
                    match db.get_config(TELEGRAM_CREDS_KEY) {
                        Ok(Some(blob)) => {
                            match security::unprotect_and_deserialize::<TelegramApiCredentials>(
                                &blob,
                            ) {
                                Ok(creds) => {
                                    state
                                        .telegram
                                        .set_credentials(creds.api_id, creds.api_hash)
                                        .await;
                                    log::info!(
                                        "Loaded Telegram API credentials from secure storage"
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "Failed to decode stored Telegram credentials: {}",
                                        e
                                    );
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::warn!("Failed to read Telegram credentials from config: {}", e);
                        }
                    }

                    match db.reconcile_cloud_only_flags() {
                        Ok(updated) if updated > 0 => {
                            log::info!(
                                "Startup reconciliation marked {} item(s) as cloud-only",
                                updated
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::warn!("Failed to reconcile cloud-only flags: {}", e);
                        }
                    }

                    // Start Watcher
                    let watch_path = app_dir.join("backup");
                    let cache_dir = app_dir.join("cache");
                    std::fs::create_dir_all(&watch_path).ok();
                    std::fs::create_dir_all(&cache_dir).ok();

                    match watcher::FileWatcher::new(
                        watch_path.clone(),
                        cache_dir,
                        db.clone(),
                        app_handle.clone(),
                        state.security_runtime.clone(),
                    ) {
                        Ok(w) => {
                            *state.watcher.lock().await = Some(w);
                            println!("File Watcher started at {:?}", watch_path);
                        }
                        Err(e) => eprintln!("Failed to start watcher: {}", e),
                    }

                    // Start AI Worker
                    let models_dir = app_dir.join("models");
                    let ai_worker = ai::worker::AiWorker::new(
                        db.clone(),
                        state.face_detector.clone(),
                        models_dir,
                    );

                    let worker_cancel = tokio_util::sync::CancellationToken::new();
                    let worker_cancel_clone = worker_cancel.clone();
                    tokio::spawn(async move {
                        ai_worker.run(worker_cancel_clone).await;
                    });
                    println!("AI Worker spawned");

                    // Create cancellation token for graceful shutdown
                    let cancel_token = CancellationToken::new();

                    // Start Upload Worker
                    let telegram_for_worker = state.telegram.clone();
                    let db_for_worker = db.clone();
                    let app_handle_for_worker = app_handle.clone();
                    let security_for_worker = state.security_runtime.clone();
                    let cancel_for_upload = cancel_token.clone();
                    tauri::async_runtime::spawn(async move {
                        upload_worker::run_upload_worker(
                            db_for_worker,
                            telegram_for_worker,
                            security_for_worker,
                            app_handle_for_worker,
                            cancel_for_upload,
                        )
                        .await;
                    });

                    // Start Sync Worker
                    let sync_worker = sync_worker::SyncWorker::new(
                        db.clone(),
                        state.telegram.clone(),
                        app_dir.join("backup").to_string_lossy().to_string(),
                        app_handle.clone(),
                        state.security_runtime.clone(),
                    );
                    let sync_worker = Arc::new(sync_worker);
                    let cancel_for_sync = cancel_token.clone();
                    tauri::async_runtime::spawn(async move {
                        sync_worker.run(cancel_for_sync).await;
                    });

                    // Start View Cache Cleanup Task
                    let db_for_cleanup = db.clone();
                    let app_handle_for_cleanup = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        // Wait a bit for startup to finish
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                        // Read config
                        let max_size_mb = db_for_cleanup
                            .get_config("view_cache_max_size_mb")
                            .unwrap_or(None)
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(500);

                        let retention_hours = db_for_cleanup
                            .get_config("view_cache_retention_hours")
                            .unwrap_or(None)
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(24);

                        let max_size_bytes = max_size_mb * 1024 * 1024;
                        let retention_secs = retention_hours * 3600;

                        let app_dir = resolve_app_data_dir(&app_handle_for_cleanup)
                            .unwrap_or_else(|_| std::path::PathBuf::from("."));
                        let cache_dir = app_dir.join("view_cache");

                        log::info!(
                            "Starting View Cache Cleanup. Max Size: {} MB, Retention: {} hours",
                            max_size_mb,
                            retention_hours
                        );

                        if let Err(e) =
                            view_cache::cleanup_cache(&cache_dir, max_size_bytes, retention_secs)
                        {
                            log::error!("Failed to cleanup view cache: {}", e);
                        }
                    });
                }

                // Connect Telegram only when BYOK credentials are configured.
                if state.telegram.has_credentials().await {
                    if let Err(e) = state.telegram.connect(app_dir.clone()).await {
                        eprintln!("Failed to connect to Telegram: {}", e);
                    }
                } else {
                    log::info!("Telegram API credentials not configured yet; skipping connect");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_security_status,
            initialize_unencrypted_mode,
            initialize_encryption,
            unlock_encryption,
            lock_encryption,
            recover_encryption,
            regenerate_recovery_key,
            complete_onboarding,
            set_telegram_api_credentials,
            clear_telegram_api_credentials,
            get_encryption_migration_status,
            start_encryption_migration,
            login_request_code,
            login_sign_in,
            get_me,
            logout,
            get_media,
            search_media,
            search_fts,
            create_album,
            get_albums,
            add_media_to_album,
            get_album_media,
            import_files,
            get_queue_status,
            detect_faces,
            get_faces,
            debug_reset_faces,
            // Phase 2: Favorites & Ratings
            toggle_favorite,
            set_rating,
            get_favorites,
            // Phase 2: Trash
            soft_delete_media,
            restore_from_trash,
            get_trash,
            // Phase 3: Upload Queue
            get_upload_queue,
            get_queue_counts,
            retry_upload,
            // Phase 5: Bulk Operations
            bulk_set_favorite,
            bulk_delete,
            bulk_add_to_album,
            // Phase 6: Export & Advanced Features
            export_media,
            // Phase 7: Duplicate Detection & People
            find_duplicates,
            scan_duplicates,
            get_persons,
            update_person_name,
            get_media_by_person,
            merge_persons,
            // Phase 7: Tags / Object Detection
            get_all_tags,
            get_media_by_tag,
            get_tags_for_media,
            // Config / Settings
            get_all_config,
            set_config,
            // Smart Albums
            get_smart_album_counts,
            get_videos,
            get_recent,
            get_top_rated,
            // Archive
            archive_media,
            unarchive_media,
            get_archived_media,
            // Permanent Delete
            permanent_delete_media,
            empty_trash,
            // Backup
            get_backup_path,
            backup_database,
            inspect_backup_archive,
            restore_backup_archive,
            // Cloud-Only Mode
            remove_local_copy,
            download_local_copy,
            download_for_view,
            // Share
            generate_share_link,
            // Sync
            export_sync_manifest,
            import_sync_manifest,
            get_device_id,
            // CLIP Semantic Search
            check_clip_models,
            download_clip_models,
            semantic_search,
            index_pending_clip,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
async fn get_backup_path(app: tauri::AppHandle) -> Result<String, String> {
    let app_data = resolve_app_data_dir(&app)?;
    let backup_dir = app_data.join("backup");
    std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
    Ok(backup_dir.to_string_lossy().to_string())
}

#[tauri::command]
async fn get_queue_status(state: State<'_, AppState>) -> Result<Vec<database::QueueItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_queue_status().map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_files(files: Vec<String>, app: tauri::AppHandle) -> Result<usize, String> {
    // Resolve backup directory in app data path
    let app_dir = resolve_app_data_dir(&app)?;

    let backup_dir = app_dir.join("backup");

    // Ensure it exists (should be created by setup, but safety check)
    std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;

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

            // Copy the file
            if let Err(e) = std::fs::copy(&path, &dest_path) {
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

// --- Phase 2: Favorites & Ratings Commands ---

#[tauri::command]
async fn toggle_favorite(media_id: i64, state: State<'_, AppState>) -> Result<bool, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.toggle_favorite(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_rating(media_id: i64, rating: i32, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.set_rating(media_id, rating).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_favorites(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_favorites(limit, offset).map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

// --- Phase 2: Trash Commands ---

#[tauri::command]
async fn soft_delete_media(media_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    println!(">>> soft_delete_media CALLED for id={}", media_id);
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.soft_delete(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn restore_from_trash(media_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.restore_from_trash(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_trash(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    println!(
        ">>> get_trash CALLED with limit={}, offset={}",
        limit, offset
    );
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_trash(limit, offset).map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

// --- Phase 3: Upload Queue Commands ---

#[tauri::command]
async fn get_upload_queue(state: State<'_, AppState>) -> Result<Vec<database::QueueItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_queue_status().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_queue_counts(state: State<'_, AppState>) -> Result<database::QueueCounts, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_queue_counts().map_err(|e| e.to_string())
}

#[tauri::command]
async fn retry_upload(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.retry_failed_item(id).map_err(|e| e.to_string())
}

// --- Phase 5: Bulk Operations Commands ---

#[tauri::command]
async fn bulk_set_favorite(
    media_ids: Vec<i64>,
    is_favorite: bool,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.bulk_set_favorite(&media_ids, is_favorite)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn bulk_delete(media_ids: Vec<i64>, state: State<'_, AppState>) -> Result<usize, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.bulk_soft_delete(&media_ids).map_err(|e| e.to_string())
}

#[tauri::command]
async fn bulk_add_to_album(
    album_id: i64,
    media_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.bulk_add_to_album(album_id, &media_ids)
        .map_err(|e| e.to_string())
}

// --- Phase 6: Export & Advanced Features ---

#[tauri::command]
async fn export_media(
    media_ids: Vec<i64>,
    destination: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    use std::path::Path;
    use time::OffsetDateTime;

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_media_by_ids(&media_ids).map_err(|e| e.to_string())?;
    drop(db_guard);

    let dest_path = paths::normalize_lexically(Path::new(&destination));
    if !dest_path.is_absolute() {
        return Err("Export destination must be an absolute path".to_string());
    }
    if !dest_path.exists() {
        std::fs::create_dir_all(&dest_path).map_err(|e| e.to_string())?;
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
            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        }

        let file_name = source_hint.file_name().ok_or("Invalid file name")?;
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
            std::fs::copy(source, &final_dest).map_err(|e| e.to_string())?;
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

// --- Phase 7: Duplicate Detection ---

#[tauri::command]
async fn find_duplicates(
    state: State<'_, AppState>,
) -> Result<Vec<Vec<database::MediaItem>>, String> {
    // Opportunistically fill missing pHashes so Refresh can recover even if
    // Scan Library was run before watcher ingestion completed.
    let items_to_scan = {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        db.get_media_without_phash().map_err(|e| e.to_string())?
    };

    if !items_to_scan.is_empty() {
        for (media_id, file_path) in items_to_scan {
            let path = std::path::Path::new(&file_path);
            if let Some(phash) = media_utils::generate_phash(path) {
                let db_guard = state.db.lock().await;
                if let Some(db) = db_guard.as_ref() {
                    let _ = db.update_phash(media_id, &phash);
                }
            }
        }
    }

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let groups = db.find_duplicates().map_err(|e| e.to_string())?;
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
async fn scan_duplicates(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    use tauri::Emitter;

    // Prefer missing hashes first. If none are missing, run a full image rescan.
    // This recovers from stale/invalid historical phash values and keeps
    // "Scan Library" behavior deterministic for QA workflows.
    let items_to_scan = {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        let missing = db.get_media_without_phash().map_err(|e| e.to_string())?;
        if missing.is_empty() {
            db.get_all_media_for_phash_scan()
                .map_err(|e| e.to_string())?
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

    for (idx, (media_id, file_path)) in items_to_scan.into_iter().enumerate() {
        let path = std::path::Path::new(&file_path);

        // Compute phash
        if let Some(phash) = media_utils::generate_phash(path) {
            // Update database
            let db_guard = state.db.lock().await;
            if let Some(db) = db_guard.as_ref() {
                if db.update_phash(media_id, &phash).is_ok() {
                    success_count += 1;
                }
            }
        }

        // Emit progress every 5 items or on last item
        if (idx + 1) % 5 == 0 || idx + 1 == total {
            let _ = app.emit("scan-duplicates-progress", (idx + 1, total));
        }
    }

    log::info!("Scan complete: {} of {} items hashed", success_count, total);
    let _ = app.emit("scan-duplicates-finished", success_count);

    Ok(success_count)
}

// --- Object Detection / Tags Commands ---

#[tauri::command]
async fn get_tags_for_media(
    media_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_tags_for_media(media_id).map_err(|e| e.to_string())
}

#[tauri::command]

async fn get_persons(state: State<'_, AppState>) -> Result<Vec<database::Person>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_people().map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_person_name(
    person_id: i64,
    name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.update_person_name(person_id, &name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_media_by_person(
    person_id: i64,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db
        .get_media_by_person(person_id, limit, offset)
        .map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn merge_persons(
    target_id: i64,
    source_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.merge_persons(target_id, &source_ids)
        .map_err(|e| e.to_string())
}

// --- Phase 7: Tags / Object Detection ---

#[tauri::command]
async fn get_all_tags(state: State<'_, AppState>) -> Result<Vec<database::Tag>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_all_tags().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_media_by_tag(
    tag: String,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db
        .get_media_by_tag(&tag, limit, offset)
        .map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

// --- Config / Settings ---

// --- Duplicate Detection ---

#[tauri::command]
async fn get_all_config(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let mut config = db.get_all_config().map_err(|e| e.to_string())?;
    // Mirrors the guard in `set_config`. This command returned the entire config
    // table, so every `MediaGrid` mount handed the wrapped master key and the
    // DPAPI credential blob to JavaScript purely to read `timeline_grouping`.
    config.retain(|key, _| !is_security_key(key));
    Ok(config)
}

#[tauri::command]
async fn set_config(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    if is_security_key(&key) {
        return Err("Security settings are managed by dedicated security commands".to_string());
    }
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.set_config(&key, &value).map_err(|e| e.to_string())
}

// --- Smart Albums Commands ---

#[tauri::command]
async fn get_smart_album_counts(
    state: State<'_, AppState>,
) -> Result<database::SmartAlbumCounts, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_smart_album_counts().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_videos(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_videos(limit, offset).map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn get_recent(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_recent(limit, offset).map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn get_top_rated(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db.get_top_rated(limit, offset).map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn archive_media(media_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.archive_media(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn unarchive_media(media_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.unarchive_media(media_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_archived_media(
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let items = db
        .get_archived_media(limit, offset)
        .map_err(|e| e.to_string())?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn permanent_delete_media(
    media_id: i64,
    delete_from_telegram: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    // Delete from local + DB, get telegram_media_id
    let telegram_media_id = db.permanent_delete(media_id).map_err(|e| e.to_string())?;

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
async fn empty_trash(
    delete_from_telegram: bool,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    println!(
        ">>> empty_trash CALLED! delete_from_telegram={}",
        delete_from_telegram
    );

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    // Delete all trashed items from local + DB
    let (deleted_count, telegram_ids) = db.empty_trash().map_err(|e| e.to_string())?;

    println!(
        "empty_trash: Deleted {} items locally. Telegram IDs to delete: {:?}, delete_from_telegram={}",
        deleted_count,
        telegram_ids,
        delete_from_telegram
    );

    // Optionally delete from Telegram
    if delete_from_telegram && !telegram_ids.is_empty() {
        drop(db_guard); // Release DB lock before async operation

        let msg_ids: Vec<i32> = telegram_ids
            .iter()
            .filter_map(|id| {
                let parsed = id.parse::<i32>().ok();
                if parsed.is_none() {
                    println!("empty_trash: Failed to parse telegram_id '{}' as i32", id);
                }
                parsed
            })
            .collect();

        println!(
            "empty_trash: Parsed {} message IDs for Telegram deletion: {:?}",
            msg_ids.len(),
            msg_ids
        );

        if !msg_ids.is_empty() {
            match state.telegram.delete_messages(&msg_ids).await {
                Ok(deleted) => {
                    println!(
                        "empty_trash: Successfully deleted {} messages from Telegram",
                        deleted
                    );
                }
                Err(e) => {
                    println!("empty_trash: Failed to delete from Telegram: {}", e);
                }
            }
        }
    }

    Ok(deleted_count)
}

#[tauri::command]
async fn backup_database(
    destination: Option<String>,
    upload_to_telegram: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    use std::path::Path;

    // Get the database path
    let app_data = resolve_app_data_dir(&app)?;
    let db_path = app_data.join("library.db");

    if !db_path.exists() {
        return Err("Database file not found".to_string());
    }

    // Determine backup destination. A caller-supplied destination must be an
    // existing directory the user already has: this command creates a file with
    // a name it chooses itself, and has no business creating directory trees at
    // arbitrary locations on the disk.
    let backup_path = if let Some(dest) = destination {
        let dest_path = paths::normalize_lexically(Path::new(&dest));
        if !dest_path.is_dir() {
            return Err(format!(
                "Backup destination is not an existing directory: {}",
                dest_path.display()
            ));
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
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // Copy the database file
    std::fs::copy(&db_path, &backup_path).map_err(|e| e.to_string())?;

    let mut final_backup_path = backup_path.clone();

    // The authoritative encryption state is the bundle, not the `security_mode`
    // row, and a read failure must not silently downgrade the backup to
    // plaintext, so this fails closed rather than defaulting to "unset".
    let bundle = {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
        load_security_bundle(db)?
    };
    let encrypted_mode = bundle
        .as_ref()
        .map(|b| b.mode == EncryptionMode::Encrypted)
        .unwrap_or(false);

    if encrypted_mode {
        let bundle = bundle.ok_or("Security bundle missing")?;
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
        .map_err(|e| e.to_string())?;
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

/// Metadata about a backup archive, readable without any secret.
#[derive(serde::Serialize)]
struct BackupArchiveInfo {
    format_version: u8,
    created_at: i64,
    app_version: String,
    source_file: String,
    encrypted: bool,
    has_passphrase_wrap: bool,
    has_recovery_wrap: bool,
}

#[tauri::command]
async fn inspect_backup_archive(archive_path: String) -> Result<BackupArchiveInfo, String> {
    let path = std::path::PathBuf::from(&archive_path);
    let header = backup::read_header(&path).map_err(|e| e.to_string())?;
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
async fn restore_backup_archive(
    archive_path: String,
    passphrase: Option<String>,
    recovery_key: Option<String>,
) -> Result<String, String> {
    let archive = std::path::PathBuf::from(&archive_path);
    let passphrase = passphrase.filter(|p| !p.is_empty());
    let recovery_key = recovery_key.filter(|k| !k.is_empty());
    if passphrase.is_none() && recovery_key.is_none() {
        return Err("A passphrase or a recovery key is required".to_string());
    }

    let out_path = archive.with_extension("restored.db");
    // Argon2id at 64 MiB plus a full-file decrypt is far too much work for the
    // async runtime; every other heavy path in this file that gets this right
    // uses spawn_blocking too.
    tauri::async_runtime::spawn_blocking(move || {
        let secret = match (passphrase.as_deref(), recovery_key.as_deref()) {
            (Some(p), _) => backup::BackupSecret::Passphrase(p),
            (None, Some(k)) => backup::BackupSecret::RecoveryKey(k),
            (None, None) => return Err("A passphrase or a recovery key is required".to_string()),
        };
        backup::restore_encrypted_backup(&archive, &out_path, secret)
            .map(|_| out_path.to_string_lossy().to_string())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn remove_local_copy(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Get the media item to find the file path
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Media not found".to_string())?;

    // Check if it has a telegram_media_id (required for cloud-only mode)
    if media.telegram_media_id.is_none() {
        return Err("Cannot remove local copy: media not uploaded to Telegram yet".to_string());
    }

    // Check if already cloud-only
    if media.is_cloud_only {
        return Err("Media is already cloud-only".to_string());
    }

    // Delete the local file (but keep the thumbnail). `file_path` is a plain
    // text column, so it is confined to the managed library before any unlink.
    let app_data = resolve_app_data_dir(&app)?;
    let roots = paths::managed_roots(&app_data);
    let file_path = std::path::Path::new(&media.file_path);
    if !paths::is_within_any(&roots, file_path) {
        return Err(format!(
            "Refusing to delete a file outside the managed library: {}",
            media.file_path
        ));
    }
    if file_path.exists() {
        std::fs::remove_file(file_path).map_err(|e| format!("Failed to delete file: {}", e))?;
    }

    // Mark as cloud-only in database
    db.set_cloud_only(media_id, true)
        .map_err(|e| e.to_string())?;

    log::info!("Removed local copy for media {}, now cloud-only", media_id);
    Ok(())
}

#[tauri::command]
async fn download_local_copy(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // Get the media item
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Media not found".to_string())?;

    // Check if it's cloud-only
    if !media.is_cloud_only {
        return Err("Media already has local copy".to_string());
    }

    // Get the telegram_media_id
    let telegram_id = media
        .telegram_media_id
        .clone()
        .ok_or_else(|| "No Telegram ID found".to_string())?;

    // Parse the telegram_media_id to get the message ID
    let msg_id: i32 = telegram_id
        .parse()
        .map_err(|_| "Invalid Telegram message ID".to_string())?;

    // Drop db guard before async operation
    drop(db_guard);

    // Get the backup directory
    let app_data = resolve_app_data_dir(&app)?;
    let backup_dir = app_data.join("backup");
    std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;

    // Determine filename from original path
    let filename = std::path::Path::new(&media.file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let download_path = backup_dir.join(filename);
    let download_path_str = download_path.to_string_lossy().to_string();

    // Download to a temp file first to avoid watcher hashing/upload-queue races
    // while the file is still being written.
    let restore_staging_dir = std::env::temp_dir().join("wanderer-local-restore-staging");
    std::fs::create_dir_all(&restore_staging_dir).map_err(|e| e.to_string())?;
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
    match std::fs::rename(&staged_path, &download_path) {
        Ok(_) => {}
        Err(_) => {
            std::fs::copy(&staged_path, &download_path).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&staged_path);
        }
    }

    // Re-acquire db lock to update
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    // Mark as not cloud-only
    db.set_cloud_only(media_id, false)
        .map_err(|e| e.to_string())?;

    log::info!(
        "Downloaded local copy for media {} to {}",
        media_id,
        download_path_str
    );
    Ok(download_path_str)
}

#[tauri::command]
async fn download_for_view(
    media_id: i64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // Get the media item
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Media not found".to_string())?;

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
        .ok_or_else(|| "No Telegram ID found".to_string())?;

    // Parse the telegram_media_id to get the message ID
    let msg_id: i32 = telegram_id
        .parse()
        .map_err(|_| "Invalid Telegram message ID".to_string())?;

    // Drop db guard
    drop(db_guard);

    // Get the view_cache directory
    let app_data = resolve_app_data_dir(&app)?;
    let cache_dir = app_data.join("view_cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
    let encrypted_mode = {
        let db_guard = state.db.lock().await;
        let db = db_guard.as_ref().ok_or("Database not initialized")?;
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
            .ok_or_else(|| "Encryption vault is locked. Unlock to view cloud media.".to_string())?;

        // In encrypted mode, keep cache encrypted-at-rest and materialize plaintext
        // only in temp for active viewing.
        let cache_blob_path = cache_dir.join(format!("{}_{}.wbenc", media_id, filename));

        if !cache_blob_path.exists() {
            let staging_dir = std::env::temp_dir().join("wanderer-view-cache-staging");
            std::fs::create_dir_all(&staging_dir).map_err(|e| e.to_string())?;
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
                security::is_encrypted_file(&raw_download_path).map_err(|e| e.to_string())?;

            let write_result = if downloaded_is_encrypted {
                match std::fs::rename(&raw_download_path, &cache_blob_path) {
                    Ok(_) => Ok(()),
                    Err(_) => {
                        std::fs::copy(&raw_download_path, &cache_blob_path)
                            .map_err(|e| e.to_string())?;
                        let _ = std::fs::remove_file(&raw_download_path);
                        Ok(())
                    }
                }
            } else {
                security::encrypt_file(&raw_download_path, &cache_blob_path, &key)
                    .map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(&raw_download_path);
                Ok(())
            };

            if let Err(e) = write_result {
                let _ = std::fs::remove_file(&raw_download_path);
                return Err(e);
            }
        }

        let _ = filetime::set_file_mtime(&cache_blob_path, filetime::FileTime::now());

        let materialized_dir = std::env::temp_dir().join("wanderer-view-cache-materialized");
        std::fs::create_dir_all(&materialized_dir).map_err(|e| e.to_string())?;
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
            security::decrypt_file_if_needed(&cache_blob_path, &materialized_path, Some(&key))
                .map_err(|e| e.to_string())?;
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
async fn generate_share_link(media_id: i64, state: State<'_, AppState>) -> Result<String, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let media = db
        .get_media_by_id(media_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Media not found".to_string())?;

    // Check if uploaded to Telegram
    let telegram_id = media
        .telegram_media_id
        .ok_or("Media not uploaded to Telegram yet")?;

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

/// Export the current database state to a sync manifest JSON file
/// Returns the path to the generated manifest file
#[tauri::command]
async fn export_sync_manifest(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    // Get or create device ID
    let device_id = db
        .get_config("device_id")
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| {
            let id = sync_manifest::generate_device_id();
            let _ = db.set_config("device_id", &id);
            id
        });

    // Create manifest from current database state
    let mut manifest = sync_manifest::SyncManifest::new(device_id);

    // Export all media metadata
    let all_media = db.get_all_media_for_sync().map_err(|e| e.to_string())?;
    for item in all_media {
        if let Some(hash) = &item.file_hash {
            // Get albums for this item
            let albums = db
                .get_albums_for_media(item.id)
                .map_err(|e| e.to_string())?
                .iter()
                .map(|a| a.name.clone())
                .collect();

            manifest.update_media(hash, item.is_favorite, item.rating, albums);
        }
    }

    // Export all albums
    let all_albums = db.get_albums().map_err(|e| e.to_string())?;
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
async fn import_sync_manifest(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

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
                .map_err(|e| e.to_string())?
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
    for (_, album_meta) in &remote_manifest.albums {
        if db
            .get_album_by_name(&album_meta.name)
            .map_err(|e| e.to_string())?
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
async fn get_device_id(state: State<'_, AppState>) -> Result<String, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let device_id = db
        .get_config("device_id")
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| {
            let id = sync_manifest::generate_device_id();
            let _ = db.set_config("device_id", &id);
            id
        });

    Ok(device_id)
}

/// Check if CLIP models are available for semantic search
#[tauri::command]
async fn check_clip_models(app: tauri::AppHandle) -> Result<bool, String> {
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");
    if !clip::models_available(&models_dir) {
        return Ok(false);
    }

    match clip::ensure_models_loaded(&models_dir) {
        Ok(_) => Ok(true),
        Err(e) => {
            log::warn!("CLIP models found but failed to initialize: {}", e);
            Ok(false)
        }
    }
}

/// Download CLIP models
#[tauri::command]
async fn download_clip_models(app: tauri::AppHandle) -> Result<(), String> {
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");

    let app_handle = app.clone();
    clip::download_models(&models_dir, move |model, current, total| {
        let _ = app_handle.emit(
            "model_download_progress",
            serde_json::json!({
                "model": model,
                "current": current,
                "total": total
            }),
        );
    })
    .await
}

/// Semantic search using CLIP embeddings
/// Returns media IDs sorted by similarity to the query
#[tauri::command]
async fn semantic_search(
    query: String,
    limit: i32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Vec<database::MediaItem>, String> {
    // Changed to return objects for UI convenience
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");

    // Ensure models loaded
    clip::ensure_models_loaded(&models_dir).map_err(|e| e.to_string())?;

    // Encode Query
    let query_embedding = clip::encode_text(&query).map_err(|e| e.to_string())?;

    // Get all embeddings from DB
    // NOTE: For large datasets, this should be optimized or moved to an indexing structure (FAISS/Granne)
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let all_embeddings = db.get_all_clip_embeddings().map_err(|e| e.to_string())?;

    // Compute Similarities
    let mut scores: Vec<(i64, f32)> = all_embeddings
        .iter()
        .map(|(id, emb)| (*id, clip::cosine_similarity(&query_embedding, emb)))
        .collect();

    // Sort by score (descending)
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Get Top-K IDs
    let top_ids: Vec<i64> = scores
        .iter()
        .take(limit as usize)
        .map(|(id, _)| *id)
        .collect();

    // Fetch Media Items
    if top_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Preserve order? get_media_by_ids usually doesn't preserve order.
    // We should re-sort items by the order of top_ids.
    let mut items = db.get_media_by_ids(&top_ids).map_err(|e| e.to_string())?;

    // Sort items to match top_ids order
    items.sort_by_key(|item| {
        top_ids
            .iter()
            .position(|&id| id == item.id)
            .unwrap_or(usize::MAX)
    });

    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
async fn index_pending_clip(
    limit: i32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<usize, String> {
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");

    // Check availability only, to avoid blocking if not ready
    if !clip::models_available(&models_dir) {
        return Err("CLIP models not available".to_string());
    }

    // Ensure loaded
    clip::ensure_models_loaded(&models_dir).map_err(|e| e.to_string())?;

    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;

    let pending = db
        .get_pending_clip_items(limit)
        .map_err(|e| e.to_string())?;
    let mut count = 0;

    for (id, path_str) in pending {
        let path = std::path::Path::new(&path_str);
        if !path.exists() {
            let _ = db.mark_clip_failed(id);
            continue;
        }

        // Encode
        match clip::encode_image(path) {
            Ok(embedding) => {
                if let Err(e) = db.store_clip_embedding(id, &embedding) {
                    log::error!("Failed to store embedding for {}: {}", path_str, e);
                } else {
                    count += 1;
                }
            }
            Err(e) => {
                log::error!("Failed to encode image {}: {}", path_str, e);
                let _ = db.mark_clip_failed(id);
            }
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_keys_are_recognised() {
        // Every key the security commands write must be caught by the prefix,
        // on both the read and the write path.
        for key in [
            SECURITY_BUNDLE_KEY,
            SECURITY_MODE_KEY,
            SECURITY_ONBOARDING_COMPLETE_KEY,
            TELEGRAM_CREDS_KEY,
            SECURITY_MIGRATION_STATUS_KEY,
        ] {
            assert!(
                is_security_key(key),
                "{key} must be treated as security state"
            );
        }
        assert!(is_security_key(&format!(
            "{SECURITY_MIGRATION_PENDING_PREFIX}42"
        )));
    }

    #[test]
    fn ordinary_config_keys_are_not_filtered() {
        // The six keys the frontend actually reads, plus the one the backend
        // writes for itself, must survive the filter.
        for key in [
            "cache_size_mb",
            "view_cache_max_size_mb",
            "view_cache_retention_hours",
            "ai_face_enabled",
            "ai_tags_enabled",
            "timeline_grouping",
            "device_id",
        ] {
            assert!(!is_security_key(key), "{key} must remain readable");
        }
    }
}
