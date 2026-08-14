//! Commands for the Telegram account: credentials, login and session.

use crate::*;

#[tauri::command]
pub async fn set_telegram_api_credentials(
    api_id: i32,
    api_hash: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    if api_id <= 0 {
        return Err(AppError::invalid_input("API ID must be a positive integer"));
    }
    if api_hash.trim().len() < 8 {
        return Err(AppError::invalid_input("API hash is invalid"));
    }

    let creds = TelegramApiCredentials {
        api_id,
        api_hash: api_hash.trim().to_string(),
    };

    let protected_blob = security::serialize_and_protect(&creds, "wanderer-telegram-credentials")
        .map_err(AppError::from)?;

    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    db.set_config(TELEGRAM_CREDS_KEY, &protected_blob)
        .map_err(AppError::from)?;
    drop(db_guard);

    state
        .telegram
        .set_credentials(creds.api_id, creds.api_hash.clone())
        .await;
    Ok(())
}

#[tauri::command]
pub async fn clear_telegram_api_credentials(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    {
        let db_guard = state.db.lock().await;
        let db = db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?;
        db.remove_config(TELEGRAM_CREDS_KEY)
            .map_err(AppError::from)?;
    }
    let app_dir = resolve_app_data_dir(&app)?;
    let _ = state.telegram.logout(app_dir).await;
    state.telegram.clear_credentials().await;
    Ok(())
}

#[tauri::command]
pub async fn login_request_code(
    phone: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    if !state.telegram.has_credentials().await {
        return Err(AppError::unavailable(
            "Telegram API credentials are not configured. Complete onboarding first.",
        ));
    }
    let app_dir = resolve_app_data_dir(&app)?;
    let master_key = state.security_runtime.lock().await.master_key.clone();

    match state
        .telegram
        .request_code(&phone, app_dir, master_key)
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => Err(AppError::telegram(e.to_string())),
    }
}

#[tauri::command]
pub async fn login_sign_in(code: String, state: State<'_, AppState>) -> Result<String, AppError> {
    state
        .telegram
        .sign_in(&code)
        .await
        .map_err(AppError::telegram)
}

#[tauri::command]
pub async fn get_me(state: State<'_, AppState>) -> Result<String, AppError> {
    if !state.telegram.has_credentials().await {
        return Err(AppError::unavailable(
            "Telegram API credentials are not configured",
        ));
    }
    state.telegram.get_me().await.map_err(AppError::telegram)
}

#[tauri::command]
pub async fn logout(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), AppError> {
    let app_dir = resolve_app_data_dir(&app)?;

    state
        .telegram
        .logout(app_dir)
        .await
        .map_err(AppError::telegram)
}
