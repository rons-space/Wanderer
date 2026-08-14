//! Commands over the Telegram upload queue.

use crate::*;

#[tauri::command]
pub async fn get_queue_status(
    state: State<'_, AppState>,
) -> Result<Vec<database::QueueItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_queue_status().map_err(AppError::from)
}

#[tauri::command]
pub async fn get_upload_queue(
    state: State<'_, AppState>,
) -> Result<Vec<database::QueueItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_queue_status().map_err(AppError::from)
}

#[tauri::command]
pub async fn get_queue_counts(
    state: State<'_, AppState>,
) -> Result<database::QueueCounts, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_queue_counts().map_err(AppError::from)
}

#[tauri::command]
pub async fn retry_upload(id: i64, state: State<'_, AppState>) -> Result<(), AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.retry_failed_item(id).map_err(AppError::from)
}
