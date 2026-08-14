mod ai;
mod backup;
mod clip;
mod database;
mod errors;
mod logging;
mod media_utils;
mod metadata;
mod model_integrity;
mod paths;
mod progress_stream;
mod raw_support;
mod secret_store;
mod security;
mod session_store;
mod sync_manifest;
mod sync_worker;
mod telegram;
mod upload_worker;
mod view_cache;
mod watcher;

mod commands;
// The handler list below names the commands unqualified, as it did when they
// all lived in this file.
use commands::ai::*;
use commands::albums::*;
use commands::backup::*;
use commands::config::*;
use commands::diagnostics::*;
use commands::media::*;
use commands::search::*;
use commands::security::*;
use commands::telegram::*;
use commands::uploads::*;

use database::Database;
use errors::AppError;
use security::{
    EncryptionMode, MigrationStatus, RuntimeState, SecurityBundle, TelegramApiCredentials,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use telegram::TelegramService;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Shared state behind every command.
///
/// Two rules keep the mutexes here from deadlocking or serialising the application:
///
/// 1. `db` is a slot, not a lock over database work. `Database` is `Sync` and does its
///    own locking, so the guard exists only long enough to clone the `Arc` out of it.
///    Holding it across an `.await` blocks all 74 commands behind whatever that await
///    is waiting for, which in the worst case was a Telegram transfer.
/// 2. Where two of these are needed at once, take `db` first and `security_runtime`
///    second. Both orders were in use, which is a deadlock waiting for the two paths
///    to run concurrently.
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

fn fallback_app_data_dir() -> Result<std::path::PathBuf, AppError> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| AppError::internal("Could not find local data directory"))?;
    Ok(base.join(APP_DATA_FALLBACK_DIR_NAME))
}

fn resolve_app_data_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, AppError> {
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

    std::fs::create_dir_all(&app_data_dir).map_err(AppError::from)?;
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
pub(crate) fn encryption_required(db: &Database) -> Result<bool, AppError> {
    Ok(load_security_bundle(db)?
        .map(|b| b.mode == EncryptionMode::Encrypted)
        .unwrap_or(false))
}

pub(crate) fn load_security_bundle(db: &Database) -> Result<Option<SecurityBundle>, AppError> {
    let raw = db.get_config(SECURITY_BUNDLE_KEY).map_err(AppError::from)?;
    match raw {
        Some(json) => serde_json::from_str::<SecurityBundle>(&json)
            .map(Some)
            .map_err(AppError::from),
        None => Ok(None),
    }
}

fn save_security_bundle(db: &Database, bundle: &SecurityBundle) -> Result<(), AppError> {
    let json = serde_json::to_string(bundle).map_err(AppError::from)?;
    db.set_config(SECURITY_BUNDLE_KEY, &json)
        .map_err(AppError::from)?;
    let mode = match bundle.mode {
        EncryptionMode::Encrypted => "encrypted",
        EncryptionMode::Unencrypted => "unencrypted",
    };
    // Kept only so an older build installed over this one still finds the row it
    // expects. Nothing in this codebase reads it as a decision input any more:
    // use `encryption_required`, which reads the bundle above.
    db.set_config(SECURITY_MODE_KEY, mode)
        .map_err(AppError::from)?;
    Ok(())
}

fn load_migration_status(db: &Database) -> MigrationStatus {
    db.get_config(SECURITY_MIGRATION_STATUS_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<MigrationStatus>(&json).ok())
        .unwrap_or_default()
}

fn save_migration_status(db: &Database, status: &MigrationStatus) -> Result<(), AppError> {
    let json = serde_json::to_string(status).map_err(AppError::from)?;
    db.set_config(SECURITY_MIGRATION_STATUS_KEY, &json)
        .map_err(AppError::from)
}

fn ensure_thumbnail_encrypted(
    thumb_path: &str,
    key: &security::MasterKey,
    roots: &[std::path::PathBuf],
) -> Result<Option<std::path::PathBuf>, AppError> {
    let path = std::path::Path::new(thumb_path);

    // `thumbnail_path` is a text column, and this function deletes the file it
    // reads, so a poisoned row would mean encrypting and then unlinking an
    // arbitrary file.
    if !paths::is_within_any(roots, path) {
        return Err(AppError::invalid_input(format!(
            "Refusing to encrypt a thumbnail outside the managed library: {}",
            thumb_path
        )));
    }

    if !path.exists() {
        return Ok(None);
    }

    if security::is_encrypted_file(path).map_err(AppError::from)? {
        return Ok(Some(path.to_path_buf()));
    }

    let encrypted_path = path.with_extension("wbenc");
    security::encrypt_file(path, &encrypted_path, key).map_err(AppError::from)?;
    let _ = std::fs::remove_file(path);
    Ok(Some(encrypted_path))
}

/// Decrypt one thumbnail into the cache, returning the path the UI should load.
///
/// Synchronous and free of any lock: every caller runs it inside `spawn_blocking`,
/// because decrypting a thumbnail reads and writes a file and is repeated for every
/// item in a response.
fn materialize_thumbnail_path(
    thumbnail_path: Option<String>,
    key: Option<&security::MasterKey>,
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

    let key = key?;
    let cache_dir = paths::thumb_cache_dir();
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

    if needs_refresh && security::decrypt_file(&src, &output, key).is_err() {
        return None;
    }

    Some(output.to_string_lossy().to_string())
}

/// Prepare a response's thumbnails, off the async runtime.
///
/// Every media-returning command ends here, and for an encrypted library this decrypts
/// one file per item. Doing that inline stalled the runtime for the length of a whole
/// page of thumbnails, which is why a scroll could freeze unrelated commands. The page
/// is handed to a single blocking task rather than one task per item, since the work is
/// short and the spawns would cost more than the decryptions.
async fn materialize_media_items_for_response(
    mut items: Vec<database::MediaItem>,
    state: &State<'_, AppState>,
) -> Vec<database::MediaItem> {
    let key = get_active_master_key(state).await;
    let thumbnails: Vec<Option<String>> = items
        .iter()
        .map(|item| item.thumbnail_path.clone())
        .collect();

    let materialized = tokio::task::spawn_blocking(move || {
        thumbnails
            .into_iter()
            .map(|path| materialize_thumbnail_path(path, key.as_ref()))
            .collect::<Vec<_>>()
    })
    .await;

    match materialized {
        Ok(paths) => {
            for (item, path) in items.iter_mut().zip(paths) {
                item.thumbnail_path = path;
            }
        }
        // A panic in the blocking task must not take the response with it: the items
        // are still correct, they just point at paths the UI may not be able to read.
        Err(e) => log::error!("Thumbnail materialization task failed: {}", e),
    }
    items
}

/// A clone rather than a borrow, because the guard cannot be held across the awaits
/// that follow. The copy is a `MasterKey`, so it zeroizes when the caller drops it.
async fn get_active_master_key(state: &State<'_, AppState>) -> Option<security::MasterKey> {
    state.security_runtime.lock().await.master_key.clone()
}

async fn download_and_materialize_media(
    state: &State<'_, AppState>,
    msg_id: i32,
    final_path: &std::path::Path,
) -> Result<(), AppError> {
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).map_err(AppError::from)?;
    }

    let temp_dir = paths::download_staging_dir();
    std::fs::create_dir_all(&temp_dir).map_err(AppError::from)?;
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
    // `Unknown`: this downloads by message id, with no media row in hand, and an
    // encrypted library can still contain pre-migration plaintext blobs. Decrypting a
    // media file is whole-file crypto, so it runs on a blocking thread.
    let decrypt_src = temp_path.clone();
    let decrypt_dst = final_path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        security::decrypt_file_if_needed(
            &decrypt_src,
            &decrypt_dst,
            maybe_key.as_ref(),
            security::Expect::Unknown,
        )
    })
    .await
    .map_err(|e| format!("Decrypt task failed: {}", e))?
    .map_err(AppError::from);

    let _ = tokio::fs::remove_file(&temp_path).await;
    result.map(|_| ())
}

async fn get_security_status_inner(
    state: &State<'_, AppState>,
) -> Result<SecurityStatusResponse, AppError> {
    // Cloned out of the slot: this function locks `security_runtime` twice below, and
    // holding the database guard across those awaits is the ordering hazard described
    // on `AppState`.
    let db = {
        let db_guard = state.db.lock().await;
        db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?
            .clone()
    };
    let db = db.as_ref();

    let onboarding_complete = db
        .get_config(SECURITY_ONBOARDING_COMPLETE_KEY)
        .map_err(AppError::from)?
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
        .map_err(AppError::from)?
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

/// Connect Telegram now that the vault holds a master key.
///
/// Outside Windows the session file is sealed with that key, so startup cannot open it
/// while the library is locked and unlocking is the first moment a connection becomes
/// possible. Best effort by design: the user asked to unlock, not to go online, and a
/// network or session failure must not turn a successful unlock into an error.
async fn connect_telegram_after_unlock(state: &State<'_, AppState>, app: &tauri::AppHandle) {
    if !state.telegram.has_credentials().await || state.telegram.is_connected().await {
        return;
    }
    let app_dir = match resolve_app_data_dir(app) {
        Ok(dir) => dir,
        Err(e) => {
            log::warn!("Cannot connect to Telegram after unlock: {}", e);
            return;
        }
    };
    let master_key = state.security_runtime.lock().await.master_key.clone();
    if let Err(e) = state.telegram.connect(app_dir, master_key).await {
        log::warn!("Failed to connect to Telegram after unlock: {}", e);
    }
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
            face_detector,
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // Before the first log line that matters: everything up to here went to
            // stderr, which a packaged build does not have. `resolve_app_data_dir`
            // logs on failure, so it is called for its path and not for its warning.
            if let Ok(app_dir) = resolve_app_data_dir(&app_handle) {
                logging::init(&app_dir);
            }

            // Before anything else, and before the vault can be unlocked again: a crash
            // or a kill leaves decrypted scratch behind that no lock handler ever ran
            // for, and startup is the only moment nothing is using it.
            let purged = paths::purge_scratch_dirs();
            if purged > 0 {
                log::info!(
                    "Purged {} scratch directories left by a previous run",
                    purged
                );
            }

            tauri::async_runtime::spawn(async move {
                let state: tauri::State<AppState> = app_handle.state();

                let app_dir = match resolve_app_data_dir(&app_handle) {
                    Ok(dir) => dir,
                    Err(e) => {
                        log::error!("Failed to resolve app data directory: {}", e);
                        return;
                    }
                };

                let db_path = app_dir.join("library.db");

                // Initialize Database
                let db_arc = match Database::new(&db_path) {
                    Ok(db) => {
                        let arc = Arc::new(db);
                        *state.db.lock().await = Some(arc.clone());
                        log::info!("Database initialized");
                        log::debug!("Database path: {:?}", db_path);
                        Some(arc)
                    }
                    Err(e) => {
                        log::error!("Failed to initialize database: {}", e);
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
                            log::info!("File watcher started");
                            log::debug!("Watching {:?}", watch_path);
                        }
                        Err(e) => log::error!("Failed to start watcher: {}", e),
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
                    log::info!("AI worker spawned");

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
                    // Windows ignores this: DPAPI opens the session with nothing
                    // unlocked. Elsewhere an encrypted library that has not been
                    // unlocked yet cannot unseal the session, so this fails and
                    // `unlock_encryption` connects instead once the key exists.
                    let master_key = state.security_runtime.lock().await.master_key.clone();
                    if let Err(e) = state.telegram.connect(app_dir.clone(), master_key).await {
                        log::warn!("Not connecting to Telegram yet: {}", e);
                    }
                } else {
                    log::info!("Telegram API credentials not configured yet; skipping connect");
                }
            });
            Ok(())
        })
        // The last chance to clean up while the process is still alive. `Destroyed`
        // rather than `CloseRequested`, because the latter can be cancelled and the
        // viewer may still hold the very files being deleted.
        .on_window_event(|_window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                let purged = paths::purge_scratch_dirs();
                log::info!("Window closed; purged {} scratch directories", purged);
            }
        })
        .invoke_handler(tauri::generate_handler![
            report_frontend_error,
            get_log_path,
            get_security_status,
            initialize_unencrypted_mode,
            initialize_encryption,
            unlock_encryption,
            lock_encryption,
            recover_encryption,
            regenerate_recovery_key,
            change_passphrase,
            complete_onboarding,
            set_telegram_api_credentials,
            clear_telegram_api_credentials,
            get_encryption_migration_status,
            retry_plaintext_purge,
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

// --- Phase 2: Favorites & Ratings Commands ---

// --- Phase 2: Trash Commands ---

// --- Phase 3: Upload Queue Commands ---

// --- Phase 5: Bulk Operations Commands ---

// --- Phase 6: Export & Advanced Features ---

// --- Phase 7: Duplicate Detection ---

/// Photos hashed per blocking batch during a library scan.
///
/// Large enough that the transaction and thread hop are amortised, small enough
/// that the progress bar still moves and a cancelled window does not lose much.
const PHASH_SCAN_CHUNK: usize = 32;

/// Compute perceptual hashes for a batch of files, dropping the ones that fail.
///
/// Runs on a blocking thread: each hash decodes and resizes an image, which is
/// exactly the work that must not happen on a runtime worker.
fn hash_batch(items: &[(i64, String)]) -> Vec<(i64, String)> {
    items
        .iter()
        .filter_map(|(media_id, file_path)| {
            let path = std::path::Path::new(file_path);
            media_utils::generate_phash(path).map(|phash| (*media_id, phash))
        })
        .collect()
}

// --- Object Detection / Tags Commands ---

// --- Phase 7: Tags / Object Detection ---

// --- Config / Settings ---

// --- Duplicate Detection ---

/// What a settings value is allowed to be.
enum ConfigDomain {
    /// `true` or `false`, in any case, stored lowercased.
    Bool,
    /// One of a fixed set of strings.
    Choice(&'static [&'static str]),
    /// A whole number in an inclusive range.
    Integer { min: i64, max: i64 },
}

/// Every config key the frontend may write, with the domain of its value.
///
/// This replaces a `security_` denylist, which stopped script in the webview from
/// overwriting the key material but left every other row writable: the AI opt-ins that
/// decide whether the user's photos are analysed, the cache sizes that decide how much
/// plaintext is kept on disk, and any key at all, including ones no code reads, since
/// an unknown key was simply inserted. Anything the backend writes for itself
/// (`device_id`, the per-media sync markers, the security rows) goes through
/// `Database::set_config` directly and is not reachable from here.
const WRITABLE_CONFIG: &[(&str, ConfigDomain)] = &[
    (
        "cache_size_mb",
        ConfigDomain::Integer {
            min: 100,
            max: 1_000_000,
        },
    ),
    (
        "view_cache_max_size_mb",
        ConfigDomain::Integer {
            min: 100,
            max: 1_000_000,
        },
    ),
    (
        "view_cache_retention_hours",
        ConfigDomain::Integer { min: 1, max: 8760 },
    ),
    ("ai_face_enabled", ConfigDomain::Bool),
    ("ai_tags_enabled", ConfigDomain::Bool),
    (
        "timeline_grouping",
        ConfigDomain::Choice(&["day", "month", "year"]),
    ),
];

/// Check a settings write and return the value to store.
///
/// Returns the normalized form rather than the input, so `"TRUE"` and `"true"` cannot
/// both end up in the table and disagree with the `eq_ignore_ascii_case` readers.
fn validate_config_write(key: &str, value: &str) -> Result<String, AppError> {
    let Some((_, domain)) = WRITABLE_CONFIG.iter().find(|(name, _)| *name == key) else {
        return Err(AppError::invalid_input(format!(
            "'{}' is not a writable setting",
            key
        )));
    };

    match domain {
        ConfigDomain::Bool => match value.to_ascii_lowercase().as_str() {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            _ => Err(AppError::invalid_input(format!(
                "'{}' must be true or false",
                key
            ))),
        },
        ConfigDomain::Choice(allowed) => {
            let lowered = value.to_ascii_lowercase();
            if allowed.contains(&lowered.as_str()) {
                Ok(lowered)
            } else {
                Err(AppError::invalid_input(format!(
                    "'{}' must be one of {}",
                    key,
                    allowed.join(", ")
                )))
            }
        }
        ConfigDomain::Integer { min, max } => {
            let parsed: i64 = value.trim().parse().map_err(|_| {
                AppError::invalid_input(format!("'{}' must be a whole number", key))
            })?;
            if parsed < *min || parsed > *max {
                return Err(AppError::invalid_input(format!(
                    "'{}' must be between {} and {}",
                    key, min, max
                )));
            }
            Ok(parsed.to_string())
        }
    }
}

// --- Smart Albums Commands ---

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

    #[test]
    fn the_settings_the_ui_writes_are_all_writable() {
        // Everything Settings.tsx can save has to pass, or the allowlist has quietly
        // broken a control the user can see.
        for (key, value) in [
            ("cache_size_mb", "5000"),
            ("view_cache_max_size_mb", "2000"),
            ("view_cache_retention_hours", "24"),
            ("ai_face_enabled", "false"),
            ("ai_tags_enabled", "true"),
            ("timeline_grouping", "month"),
        ] {
            assert_eq!(validate_config_write(key, value).unwrap(), value);
        }
    }

    #[test]
    fn unknown_and_security_keys_are_refused() {
        // The denylist this replaces let both of these through.
        assert!(validate_config_write("something_invented", "1").is_err());
        assert!(validate_config_write(SECURITY_BUNDLE_KEY, "{}").is_err());
        assert!(validate_config_write("device_id", "attacker-chosen").is_err());
    }

    #[test]
    fn values_outside_their_domain_are_refused() {
        assert!(validate_config_write("ai_face_enabled", "yes").is_err());
        assert!(validate_config_write("timeline_grouping", "century").is_err());
        assert!(validate_config_write("cache_size_mb", "-1").is_err());
        assert!(validate_config_write("cache_size_mb", "999999999").is_err());
        assert!(validate_config_write("cache_size_mb", "5000; DROP TABLE media").is_err());
        assert!(validate_config_write("view_cache_retention_hours", "0").is_err());
    }

    /// Readers compare with `eq_ignore_ascii_case`, so a stored `"TRUE"` would work by
    /// luck. Normalizing keeps one spelling in the table.
    #[test]
    fn accepted_values_are_normalized() {
        assert_eq!(
            validate_config_write("ai_tags_enabled", "TRUE").unwrap(),
            "true"
        );
        assert_eq!(
            validate_config_write("timeline_grouping", "Year").unwrap(),
            "year"
        );
        assert_eq!(
            validate_config_write("cache_size_mb", " 5000 ").unwrap(),
            "5000"
        );
    }

    /// A rejected settings write is the user's mistake, not an internal failure, and
    /// the frontend decides how to present it from the code.
    #[test]
    fn a_rejected_setting_is_invalid_input() {
        let err = validate_config_write("timeline_grouping", "century").unwrap_err();
        assert_eq!(err.code(), errors::ErrorCode::InvalidInput);
        assert!(err.message().contains("timeline_grouping"));
    }
}
