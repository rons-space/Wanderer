//! Commands for text, filtered and semantic search.

use crate::*;

#[tauri::command]
pub async fn search_media(
    query: String,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let filters = database::SearchFilters::default();
    let items = db
        .search_fts(&query, &filters, limit, offset)
        .map_err(AppError::from)?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

#[tauri::command]
pub async fn search_fts(
    query: String,
    filters: database::SearchFilters,
    limit: i32,
    offset: i32,
    state: State<'_, AppState>,
) -> Result<Vec<database::MediaItem>, AppError> {
    // The query text is the user's own words about their own library, so only its
    // shape is logged. It is the one command argument that is content rather than an
    // identifier.
    log::debug!(
        "search_fts: {}-character query, has_location={:?}",
        query.chars().count(),
        filters.has_location
    );
    let db_guard = state.db.lock().await;
    let db = db_guard
        .as_ref()
        .ok_or_else(AppError::database_not_initialized)?;
    let result = db
        .search_fts(&query, &filters, limit, offset)
        .map_err(AppError::from);

    match &result {
        Ok(items) => log::debug!("search_fts: returning {} items", items.len()),
        Err(e) => log::warn!("search_fts failed: {}", e),
    }
    let items = result?;
    drop(db_guard);
    Ok(materialize_media_items_for_response(items, &state).await)
}

/// Semantic search using CLIP embeddings
/// Returns media IDs sorted by similarity to the query
#[tauri::command]
pub async fn semantic_search(
    query: String,
    limit: i32,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Vec<database::MediaItem>, AppError> {
    // Changed to return objects for UI convenience
    let app_dir = resolve_app_data_dir(&app)?;
    let models_dir = app_dir.join("models");

    // Model load and text encoding are both ONNX work.
    let query_embedding = tokio::task::spawn_blocking(move || {
        clip::ensure_models_loaded(&models_dir)?;
        clip::encode_text(&query)
    })
    .await
    .map_err(|e| format!("CLIP query task failed: {}", e))?
    .map_err(AppError::from)?;

    // Get all embeddings from DB
    // NOTE: For large datasets, this should be optimized or moved to an indexing structure (FAISS/Granne)
    let db = {
        let db_guard = state.db.lock().await;
        db_guard
            .as_ref()
            .ok_or_else(AppError::database_not_initialized)?
            .clone()
    };

    let all_embeddings = db.get_all_clip_embeddings().map_err(AppError::from)?;

    // Scoring is a dot product against every embedding in the library, so it grows
    // with the library and belongs off the runtime along with the sort.
    let scores: Vec<(i64, f32)> = tokio::task::spawn_blocking(move || {
        let mut scores: Vec<(i64, f32)> = all_embeddings
            .iter()
            .map(|(id, emb)| (*id, clip::cosine_similarity(&query_embedding, emb)))
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    })
    .await
    .map_err(|e| format!("CLIP scoring task failed: {}", e))?;

    // Get Top-K IDs. `limit` arrives from the frontend as a signed integer, and
    // `-1i32 as usize` is 18446744073709551615, so a negative limit asked for every
    // media item in the library rather than none.
    let top_k = limit.clamp(0, 1000) as usize;
    let top_ids: Vec<i64> = scores.iter().take(top_k).map(|(id, _)| *id).collect();

    // Fetch Media Items
    if top_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Preserve order? get_media_by_ids usually doesn't preserve order.
    // We should re-sort items by the order of top_ids.
    let mut items = db.get_media_by_ids(&top_ids).map_err(AppError::from)?;

    // Sort items to match top_ids order
    items.sort_by_key(|item| {
        top_ids
            .iter()
            .position(|&id| id == item.id)
            .unwrap_or(usize::MAX)
    });

    Ok(materialize_media_items_for_response(items, &state).await)
}
