//! Commands over the on-device AI results: faces, people, tags and the
//! CLIP index.

use crate::*;

#[tauri::command]
pub async fn detect_faces(
    state: State<'_, AppState>,
    path: String,
) -> Result<Vec<ai::Face>, AppError> {
    let detector = match &state.face_detector {
        Some(d) => d.clone(),
        None => return Err(AppError::unavailable("AI face detection is not available")),
    };
    let path_buf = std::path::PathBuf::from(path);

    // Offload CPU-intensive task to a blocking thread
    let join_handle = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ai::Face>> {
        let detector = detector.blocking_lock();
        detector.detect(&path_buf)
    });

    match join_handle.await {
        Ok(detection_res) => detection_res.map_err(AppError::from),
        Err(e) => Err(AppError::from(e)),
    }
}

#[tauri::command]
pub async fn get_faces(
    state: State<'_, AppState>,
    media_id: i64,
) -> Result<Vec<ai::Face>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_faces(media_id).map_err(AppError::from)
}

#[tauri::command]
pub async fn get_tags_for_media(
    media_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<String>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_tags_for_media(media_id).map_err(AppError::from)
}

#[tauri::command]

pub async fn get_persons(state: State<'_, AppState>) -> Result<Vec<database::Person>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_people().map_err(AppError::from)
}

#[tauri::command]
pub async fn update_person_name(
    person_id: i64,
    name: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.update_person_name(person_id, &name)
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn get_media_by_person(
    person_id: i64,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db
        .get_media_by_person(person_id, limit, offset)
        .map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn merge_persons(
    target_id: i64,
    source_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.merge_persons(target_id, &source_ids)
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn get_all_tags(state: State<'_, AppState>) -> Result<Vec<database::Tag>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_all_tags().map_err(AppError::from)
}

#[tauri::command]
pub async fn get_media_by_tag(
    tag: String,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let items = db
        .get_media_by_tag(&tag, limit, offset)
        .map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

/// Check if CLIP models are available for semantic search
#[tauri::command]
pub async fn check_clip_models(app: tauri::AppHandle) -> Result<bool, AppError> {
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");
    if !clip::models_available(&models_dir) {
        return Ok(false);
    }

    // Loading ONNX sessions reads hundreds of megabytes and builds the graph, which
    // is several seconds of pure blocking work.
    let loaded = tokio::task::spawn_blocking(move || clip::ensure_models_loaded(&models_dir))
        .await
        .map_err(|e| format!("CLIP model load task failed: {}", e))?;

    match loaded {
        Ok(_) => Ok(true),
        Err(e) => {
            log::warn!("CLIP models found but failed to initialize: {}", e);
            Ok(false)
        }
    }
}

/// Download CLIP models
#[tauri::command]
pub async fn download_clip_models(app: tauri::AppHandle) -> Result<(), AppError> {
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
    .map_err(AppError::from)
}

#[tauri::command]
pub async fn index_pending_clip(
    limit: i32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<usize, AppError> {
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");

    // Check availability only, to avoid blocking if not ready
    if !clip::models_available(&models_dir) {
        return Err(AppError::unavailable("CLIP models not available"));
    }

    let db = {
        let db_guard = state.db.lock().await;
        db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?
            .clone()
    };

    let pending = db.get_pending_clip_items(limit).map_err(AppError::from)?;
    if pending.is_empty() {
        return Ok(0);
    }

    // Model load plus one inference per image, on a blocking thread and with no lock
    // held. This used to run on the runtime while holding the database mutex, so
    // indexing a batch froze the interface and every other command with it.
    let encoded = tokio::task::spawn_blocking(move || {
        clip::ensure_models_loaded(&models_dir)?;

        let results: Vec<(i64, String, Option<Vec<f32>>)> = pending
            .into_iter()
            .map(|(id, path_str)| {
                let path = std::path::Path::new(&path_str);
                if !path.exists() {
                    return (id, path_str, None);
                }
                match clip::encode_image(path) {
                    Ok(embedding) => (id, path_str, Some(embedding)),
                    Err(e) => {
                        log::error!("Failed to encode media {}: {}", id, e);
                        (id, path_str, None)
                    }
                }
            })
            .collect();
        Ok::<_, String>(results)
    })
    .await
    .map_err(|e| format!("CLIP indexing task failed: {}", e))?
    .map_err(AppError::from)?;

    let mut count = 0;
    for (id, _path, embedding) in encoded {
        match embedding {
            Some(embedding) => {
                if let Err(e) = db.store_clip_embedding(id, &embedding) {
                    log::error!("Failed to store the embedding for media {}: {}", id, e);
                } else {
                    count += 1;
                }
            }
            None => {
                let _ = db.mark_clip_failed(id);
            }
        }
    }

    Ok(count)
}
