//! Commands over the settings table.

use crate::*;

#[tauri::command]
pub async fn get_all_config(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let mut config = db.get_all_config().map_err(AppError::from)?;
    // Mirrors the guard in `set_config`. This command returned the entire config
    // table, so every `MediaGrid` mount handed the wrapped master key and the
    // DPAPI credential blob to JavaScript purely to read `timeline_grouping`.
    config.retain(|key, _| !is_security_key(key));
    Ok(config)
}

#[tauri::command]
pub async fn set_config(
    key: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let value = validate_config_write(&key, &value)?;
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.set_config(&key, &value).map_err(AppError::from)
}
