//! Commands for albums and their membership.

use crate::*;

#[tauri::command]
pub async fn create_album(name: String, state: State<'_, AppState>) -> Result<i64, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.create_album(&name).map_err(AppError::from)
}

#[tauri::command]
pub async fn get_albums(state: State<'_, AppState>) -> Result<Vec<database::Album>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.get_albums().map_err(AppError::from)
}

#[tauri::command]
pub async fn add_media_to_album(
    album_id: i64,
    media_id: i64,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    log::debug!(
        "add_media_to_album: album_id={}, media_id={}",
        album_id,
        media_id
    );
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.add_media_to_album(album_id, media_id)
        .map_err(AppError::from)
}

#[tauri::command]
pub async fn get_album_media(
    album_id: i64,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    log::debug!(
        "get_album_media: album_id={}, limit={}, offset={}",
        album_id,
        limit,
        offset
    );
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let result = db
        .get_album_media(album_id, limit, offset)
        .map_err(AppError::from);
    match &result {
        Ok(items) => log::debug!("get_album_media: returning {} items", items.len()),
        Err(e) => log::warn!("get_album_media failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}
