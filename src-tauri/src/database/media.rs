//! The media table itself: importing, reading, searching, rating,
//! trashing, archiving, and the perceptual-hash duplicate scan.

use super::*;

impl Database {
    // --- Media Operations ---

    // Eight and nine positional arguments mirror the columns being inserted. The fix is
    // a parameter struct, which belongs with the `database.rs` split in T58 (issue #66)
    // rather than in a CI change.
    #[allow(clippy::too_many_arguments)]
    pub fn add_media(
        &self,
        file_path: &str,
        file_hash: Option<&str>,
        thumbnail_path: Option<&str>,
        created_at: i64,
        mime_type: Option<&str>,
        metadata: Option<crate::metadata::Metadata>,
        phash: Option<&str>,
    ) -> Result<i64> {
        let conn = self.get_conn()?;

        let (date_taken, latitude, longitude, camera_make, camera_model) = if let Some(m) = metadata
        {
            (
                m.date_taken,
                m.latitude,
                m.longitude,
                m.camera_make,
                m.camera_model,
            )
        } else {
            (None, None, None, None, None)
        };

        conn.execute(
            "INSERT INTO media (file_path, file_hash, thumbnail_path, created_at, mime_type, date_taken, latitude, longitude, camera_make, camera_model, phash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![file_path, file_hash, thumbnail_path, created_at, mime_type, date_taken, latitude, longitude, camera_make, camera_model, phash],
        )?;
        let media_id = conn.last_insert_rowid();

        // The FTS row is written by the `media_fts_insert` trigger. Doing it here as
        // well would index the same media twice, and was also the reason media added by
        // any other path was never indexed at all.

        Ok(media_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_media_synced(
        &self,
        file_path: &str,
        file_hash: &str,
        thumbnail_path: Option<&str>,
        created_at: i64,
        mime_type: Option<&str>,
        uploaded_at: i64,
        telegram_media_id: Option<&str>,
        metadata: Option<crate::metadata::Metadata>,
    ) -> Result<i64> {
        let conn = self.get_conn()?;

        let (date_taken, latitude, longitude, camera_make, camera_model) = if let Some(m) = metadata
        {
            (
                m.date_taken,
                m.latitude,
                m.longitude,
                m.camera_make,
                m.camera_model,
            )
        } else {
            (None, None, None, None, None)
        };

        conn.execute(
            "INSERT INTO media (file_path, file_hash, thumbnail_path, created_at, mime_type, uploaded_at, telegram_media_id, date_taken, latitude, longitude, camera_make, camera_model) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![file_path, file_hash, thumbnail_path, created_at, mime_type, uploaded_at, telegram_media_id, date_taken, latitude, longitude, camera_make, camera_model],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_telegram_id(&self, file_hash: &str, telegram_id: &str) -> Result<usize> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET telegram_media_id = ?1 WHERE file_hash = ?2",
            (telegram_id, file_hash),
        )
    }

    /// Update Telegram ID by file path (used by UploadWorker after successful upload)
    pub fn update_telegram_id_by_path(&self, file_path: &str, telegram_id: &str) -> Result<usize> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET telegram_media_id = ?1 WHERE file_path = ?2",
            (telegram_id, file_path),
        )
    }

    pub fn mark_media_encrypted_by_path(&self, file_path: &str) -> Result<usize> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_encrypted = 1 WHERE file_path = ?1",
            [file_path],
        )
    }

    pub fn mark_media_encrypted_by_id(&self, media_id: i64) -> Result<usize> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_encrypted = 1 WHERE id = ?1",
            [media_id],
        )
    }

    pub fn get_uploaded_unencrypted_media(&self, limit: i32) -> Result<Vec<UnencryptedUpload>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path, telegram_media_id, thumbnail_path
             FROM media
             WHERE (is_deleted = 0 OR is_deleted IS NULL)
               AND (is_encrypted = 0 OR is_encrypted IS NULL)
               AND telegram_media_id IS NOT NULL
               AND telegram_media_id != ''
             ORDER BY id ASC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map([limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_unencrypted_thumbnail_paths(&self, limit: i32) -> Result<Vec<(i64, String)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, thumbnail_path
             FROM media
             WHERE thumbnail_path IS NOT NULL
               AND thumbnail_path != ''
               AND thumbnail_path NOT LIKE '%.wbenc'
             ORDER BY id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn update_thumbnail_path(&self, media_id: i64, thumbnail_path: &str) -> Result<usize> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET thumbnail_path = ?1 WHERE id = ?2",
            rusqlite::params![thumbnail_path, media_id],
        )
    }

    /// Forget the thumbnails at these paths.
    ///
    /// Called before the files are unlinked, never after: a row pointing at a
    /// file that is gone is a broken thumbnail in the gallery, while a file no
    /// row points at is just wasted space that the next pass will remove.
    pub fn clear_thumbnail_paths(&self, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }

        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let mut cleared = 0;

        for chunk in paths.chunks(MAX_SQL_VARIABLES) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "UPDATE media SET thumbnail_path = NULL WHERE thumbnail_path IN ({placeholders})"
            );
            cleared += tx.execute(&sql, rusqlite::params_from_iter(chunk.iter()))?;
        }

        tx.commit()?;
        Ok(cleared)
    }

    pub fn get_media(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE (is_deleted = 0 OR is_deleted IS NULL) AND (is_archived = 0 OR is_archived IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2")
        )?;

        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Get multiple media items by their IDs for export
    pub fn get_media_by_ids(&self, media_ids: &[i64]) -> Result<Vec<MediaItem>> {
        if media_ids.is_empty() {
            return Ok(Vec::new());
        }
        // One statement per chunk: a selection of every item in a large library would
        // otherwise build a query with more placeholders than SQLite accepts and fail
        // outright, which is a worse answer than doing it in several round trips.
        if media_ids.len() > MAX_SQL_VARIABLES {
            let mut all = Vec::with_capacity(media_ids.len());
            for chunk in media_ids.chunks(MAX_SQL_VARIABLES) {
                all.extend(self.get_media_by_ids(chunk)?);
            }
            return Ok(all);
        }
        let conn = self.get_conn()?;
        let placeholders = media_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media WHERE id IN ({}) AND is_deleted = 0",
            placeholders
        );
        // Not `prepare_cached`: this SQL is built per call, and rusqlite's cache is a
        // small LRU, so variable statements would evict the fixed ones that repeat.
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::ToSql>> = media_ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
            .collect();
        let media_iter = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            Self::map_media_row,
        )?;
        media_iter.collect()
    }

    // --- Smart Albums Methods ---

    /// Get counts for smart albums
    pub fn get_smart_album_counts(&self) -> Result<SmartAlbumCounts> {
        let conn = self.get_conn()?;

        let videos: i32 = conn.query_row(
            "SELECT COUNT(*) FROM media WHERE mime_type LIKE 'video/%' AND (is_deleted = 0 OR is_deleted IS NULL)",
            [],
            |row| row.get(0),
        )?;

        // Recent = last 30 days
        let recent: i32 = conn.query_row(
            "SELECT COUNT(*) FROM media WHERE created_at >= strftime('%s', 'now', '-30 days') AND (is_deleted = 0 OR is_deleted IS NULL)",
            [],
            |row| row.get(0),
        )?;

        // Top rated = 4+ stars
        let top_rated: i32 = conn.query_row(
            "SELECT COUNT(*) FROM media WHERE rating >= 4 AND (is_deleted = 0 OR is_deleted IS NULL)",
            [],
            |row| row.get(0),
        )?;

        Ok(SmartAlbumCounts {
            videos,
            recent,
            top_rated,
        })
    }

    /// Get all videos
    pub fn get_videos(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE mime_type LIKE 'video/%' AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        ))?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Get recent media (last 30 days)
    pub fn get_recent(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE created_at >= strftime('%s', 'now', '-30 days') AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2")
        )?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Get top rated media (4+ stars)
    pub fn get_top_rated(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE rating >= 4 AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY rating DESC, COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        ))?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Helper function to map a row to MediaItem
    pub(super) fn map_media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaItem> {
        Ok(MediaItem {
            id: row.get(0)?,
            file_path: row.get(1)?,
            file_hash: row.get(2)?,
            telegram_media_id: row.get(3)?,
            mime_type: row.get(4)?,
            width: row.get(5)?,
            height: row.get(6)?,
            duration: row.get(7)?,
            size_bytes: row.get(8)?,
            created_at: row.get(9)?,
            uploaded_at: row.get(10)?,
            thumbnail_path: row.get(11)?,
            date_taken: row.get(12)?,
            latitude: row.get(13)?,
            longitude: row.get(14)?,
            camera_make: row.get(15)?,
            camera_model: row.get(16)?,
            is_favorite: row.get::<_, i32>(17)? != 0,
            rating: row.get(18)?,
            is_deleted: row.get::<_, i32>(19)? != 0,
            deleted_at: row.get(20)?,
            is_archived: row
                .get::<_, Option<i32>>(21)?
                .map(|v| v != 0)
                .unwrap_or(false),
            archived_at: row.get(22)?,
            is_cloud_only: row
                .get::<_, Option<i32>>(23)?
                .map(|v| v != 0)
                .unwrap_or(false),
        })
    }

    /// Full-text search using FTS5 with optional filters
    pub fn search_fts(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause based on filters.
        //
        // The shape of the clause varies, the values in it never do: every filter
        // contributes an anonymous `?` and pushes its value here, in the same order.
        // `camera_make` used to be interpolated with doubled quotes, which is the one
        // filter carrying a user-controlled string and so the one place where an
        // escaping mistake would have been an injection into the query text.
        let mut conditions = vec![
            "(is_deleted = 0 OR is_deleted IS NULL)".to_string(),
            "(is_archived = 0 OR is_archived IS NULL)".to_string(),
        ];
        let mut filter_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if filters.favorites_only {
            conditions.push("is_favorite = 1".to_string());
        }

        if let Some(min_rating) = filters.min_rating {
            conditions.push("rating >= ?".to_string());
            filter_values.push(Box::new(min_rating.clamp(0, 5)));
        }

        if let Some(date_from) = filters.date_from {
            conditions.push("created_at >= ?".to_string());
            filter_values.push(Box::new(date_from));
        }

        if let Some(date_to) = filters.date_to {
            conditions.push("created_at <= ?".to_string());
            filter_values.push(Box::new(date_to));
        }

        if let Some(camera) = &filters.camera_make {
            if !camera.is_empty() {
                conditions.push("camera_make LIKE ? ESCAPE '\\'".to_string());
                filter_values.push(Box::new(format!("%{}%", escape_like_value(camera))));
            }
        }

        if let Some(has_location) = filters.has_location {
            if has_location {
                conditions.push("latitude IS NOT NULL AND longitude IS NOT NULL".to_string());
            } else {
                conditions.push("(latitude IS NULL OR longitude IS NULL)".to_string());
            }
        }

        let where_clause = conditions.join(" AND ");

        // If query is empty, just return filtered results without FTS
        if query.trim().is_empty() {
            let sql = format!(
                "SELECT {MEDIA_COLUMNS}
                 FROM media
                 WHERE {}
                 ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC
                 LIMIT ? OFFSET ?",
                where_clause
            );

            // Positional binding: the filter values in clause order, then the page.
            let mut values = filter_values;
            values.push(Box::new(limit));
            values.push(Box::new(offset));

            // Not `prepare_cached`: this SQL is built per call, and rusqlite's cache is a
            // small LRU, so variable statements would evict the fixed ones that repeat.
            let mut stmt = conn.prepare(&sql)?;
            let media_iter = stmt.query_map(
                rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
                Self::map_media_row,
            )?;

            let mut media = Vec::new();
            for item in media_iter {
                media.push(item?);
            }
            return Ok(media);
        }

        // FTS5 search with JOIN to media table
        // Escape FTS5 special characters and add prefix matching
        let fts_query = query
            .split_whitespace()
            .map(|word| format!("\"{}\"*", word.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");

        let sql = format!(
            "SELECT {MEDIA_COLUMNS_M}
             FROM media m
             JOIN media_fts fts ON m.id = fts.rowid
             WHERE fts.media_fts MATCH ? AND {}
             ORDER BY rank, COALESCE(m.date_taken, datetime(m.created_at, 'unixepoch')) DESC
             LIMIT ? OFFSET ?",
            where_clause
        );

        // The MATCH placeholder comes first in the text, so it binds first.
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
        values.extend(filter_values);
        values.push(Box::new(limit));
        values.push(Box::new(offset));

        // Not `prepare_cached`: this SQL is built per call, and rusqlite's cache is a
        // small LRU, so variable statements would evict the fixed ones that repeat.
        let mut stmt = conn.prepare(&sql)?;
        let media_iter = stmt.query_map(
            rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
            Self::map_media_row,
        )?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    pub fn media_exists_by_hash(&self, hash: &str) -> Result<bool> {
        let conn = self.get_conn()?;
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM media WHERE file_hash = ?1",
            [hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn is_media_uploaded(&self, hash: &str) -> Result<bool> {
        let conn = self.get_conn()?;
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM media WHERE file_hash = ?1 AND uploaded_at IS NOT NULL",
            [hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // --- Bulk Operations ---

    /// Set favorite status for multiple media items
    pub fn bulk_set_favorite(&self, media_ids: &[i64], is_favorite: bool) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
        }
        if media_ids.len() > MAX_SQL_VARIABLES {
            let mut total = 0;
            for chunk in media_ids.chunks(MAX_SQL_VARIABLES) {
                total += self.bulk_set_favorite(chunk, is_favorite)?;
            }
            return Ok(total);
        }
        let conn = self.get_conn()?;
        let placeholders = media_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE media SET is_favorite = ?1 WHERE id IN ({})",
            placeholders
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(if is_favorite { 1 } else { 0 })];
        for id in media_ids {
            params.push(Box::new(*id));
        }
        let count = conn.execute(
            &sql,
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        )?;
        Ok(count)
    }

    /// Soft delete multiple media items
    pub fn bulk_soft_delete(&self, media_ids: &[i64]) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
        }
        if media_ids.len() > MAX_SQL_VARIABLES {
            let mut total = 0;
            for chunk in media_ids.chunks(MAX_SQL_VARIABLES) {
                total += self.bulk_soft_delete(chunk)?;
            }
            return Ok(total);
        }
        let conn = self.get_conn()?;
        let deleted_at = OffsetDateTime::now_utc().unix_timestamp();
        let placeholders = media_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE media SET is_deleted = 1, deleted_at = ?1 WHERE id IN ({})",
            placeholders
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(deleted_at)];
        for id in media_ids {
            params.push(Box::new(*id));
        }
        let count = conn.execute(
            &sql,
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        )?;
        Ok(count)
    }

    /// Add multiple media items to an album
    pub fn bulk_add_to_album(&self, album_id: i64, media_ids: &[i64]) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let mut count = 0;
        for media_id in media_ids {
            // Use INSERT OR IGNORE to skip duplicates
            let result = tx.execute(
                "INSERT OR IGNORE INTO album_media (album_id, media_id, added_at) VALUES (?1, ?2, ?3)",
                (album_id, media_id, now),
            )?;
            count += result;
        }
        tx.commit()?;
        Ok(count)
    }

    // --- Favorites & Ratings ---

    /// Toggle favorite status for a media item. Returns new favorite status.
    pub fn toggle_favorite(&self, media_id: i64) -> Result<bool> {
        let conn = self.get_conn()?;
        // One statement, because the update and the read back used to be two: a second
        // toggle landing between them returned the other caller's value, so the star in
        // the UI could end up showing the opposite of what was stored.
        let is_favorite: i32 = conn.query_row(
            "UPDATE media SET is_favorite = NOT COALESCE(is_favorite, 0)
             WHERE id = ?1
             RETURNING is_favorite",
            [media_id],
            |row| row.get(0),
        )?;

        Ok(is_favorite != 0)
    }

    /// Set rating (0-5 stars) for a media item.
    pub fn set_rating(&self, media_id: i64, rating: i32) -> Result<()> {
        let rating = rating.clamp(0, 5);
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET rating = ?1 WHERE id = ?2",
            params![rating, media_id],
        )?;
        Ok(())
    }

    /// Get all favorite media items.
    pub fn get_favorites(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE is_favorite = 1 AND (is_deleted = 0 OR is_deleted IS NULL) AND (is_archived = 0 OR is_archived IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2")
        )?;

        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Soft delete a media item (move to trash).
    pub fn soft_delete(&self, media_id: i64) -> Result<()> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_deleted = 1, deleted_at = ?1 WHERE id = ?2",
            params![now, media_id],
        )?;
        Ok(())
    }

    /// Restore a soft-deleted media item.
    pub fn restore_from_trash(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_deleted = 0, deleted_at = NULL WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    /// Get all items in trash.
    pub fn get_trash(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE is_deleted = 1
             ORDER BY deleted_at DESC 
             LIMIT ?1 OFFSET ?2"
        ))?;

        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Permanently delete items that have been in trash for more than 30 days.
    // No command or worker calls this yet, so trash grows without bound. Wiring it up
    // is behaviour work, not lint work, so it keeps the retention policy documented here
    // until then.
    #[allow(dead_code)]
    pub fn empty_old_trash(&self) -> Result<usize> {
        let thirty_days_ago = OffsetDateTime::now_utc().unix_timestamp() - (30 * 24 * 60 * 60);
        let conn = self.get_conn()?;
        let deleted = conn.execute(
            "DELETE FROM media WHERE is_deleted = 1 AND deleted_at < ?1",
            [thirty_days_ago],
        )?;
        Ok(deleted)
    }

    /// Permanently delete a single media item.
    /// Deletes local file and thumbnail, removes DB row.
    /// Returns the telegram_media_id if it exists (for optional Telegram deletion).
    pub fn permanent_delete(&self, media_id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.get_conn()?;

        // Get file paths before deleting
        let query_result = conn.query_row(
            "SELECT file_path, thumbnail_path, telegram_media_id FROM media WHERE id = ?1",
            [media_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        );

        let (file_path, thumbnail_path, telegram_media_id) = match query_result {
            Ok(data) => data,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                anyhow::bail!("Media item not found");
            }
            Err(e) => return Err(e.into()),
        };

        // Order matters, and it used to be the other way round. The files were unlinked
        // first, so a failure deleting the row left the library pointing at bytes that
        // no longer exist, which reads as corruption rather than as a failed delete.
        // Committing first means the worst case is an orphaned file on disk: wasted
        // space, and nothing the user notices.
        conn.execute("DELETE FROM media WHERE id = ?1", [media_id])?;
        log::info!("Permanently deleted media id {} from database", media_id);

        // Released before touching the filesystem: unlinking can block on a slow or
        // networked volume, and every other database caller is waiting on this lock.
        drop(conn);

        // Both paths are confined to the managed directories before any unlink.
        if self.delete_managed_file(&file_path) {
            log::info!("Deleted local file: {}", file_path);
        }
        if let Some(ref thumb_path) = thumbnail_path {
            if self.delete_managed_file(thumb_path) {
                log::info!("Deleted thumbnail: {}", thumb_path);
            }
        }

        Ok(telegram_media_id)
    }

    /// Permanently delete all items in trash.
    /// Returns count of deleted items and list of telegram_media_ids for optional Telegram deletion.
    pub fn empty_trash(&self) -> Result<(usize, Vec<String>)> {
        let mut conn = self.get_conn()?;

        // Get all trashed items
        let items: Vec<(i64, String, Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id, file_path, thumbnail_path, telegram_media_id FROM media WHERE is_deleted = 1",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut telegram_ids = Vec::new();
        let mut deleted_count = 0;
        // Collected inside the transaction, unlinked after it commits. Doing it inline
        // meant a rollback left rows pointing at files that had already been deleted,
        // turning a failed "empty trash" into silent data loss for everything the loop
        // had already reached.
        let mut paths_to_unlink: Vec<String> = Vec::new();

        // Use a transaction for all deletions
        let tx = conn.transaction()?;

        for (id, file_path, thumbnail_path, telegram_media_id) in items {
            paths_to_unlink.push(file_path);
            if let Some(thumb_path) = thumbnail_path {
                paths_to_unlink.push(thumb_path);
            }

            // First, clear cover_face_id in persons table for any faces belonging to this media
            // This avoids FK constraint violations
            tx.execute(
                "UPDATE persons SET cover_face_id = NULL 
                 WHERE cover_face_id IN (SELECT id FROM faces WHERE media_id = ?1)",
                [id],
            )?;

            // Delete faces for this media
            tx.execute("DELETE FROM faces WHERE media_id = ?1", [id])?;

            // Delete media_tags for this media
            tx.execute("DELETE FROM media_tags WHERE media_id = ?1", [id])?;

            // Delete media_albums for this media
            tx.execute("DELETE FROM album_media WHERE media_id = ?1", [id])?;

            // Delete the media row
            tx.execute("DELETE FROM media WHERE id = ?1", [id])?;
            deleted_count += 1;

            // Collect telegram IDs
            if let Some(tg_id) = telegram_media_id {
                telegram_ids.push(tg_id);
            }
        }

        tx.commit()?;

        // Only now, with the rows durably gone, are the files removed. Released first
        // so the unlinks do not hold every other database caller behind them.
        drop(conn);
        for path in &paths_to_unlink {
            // Confined to the managed directories, like every other unlink.
            self.delete_managed_file(path);
        }

        log::info!("Emptied trash: {} items permanently deleted", deleted_count);
        Ok((deleted_count, telegram_ids))
    }

    // --- Duplicate Detection (FR-12) ---

    // --- Duplicate Detection (FR-12) ---

    /// Update the perceptual hash for a media item
    /// Get media items that don't have a phash computed yet
    /// Returns (id, file_path) pairs for images only (not videos)
    pub fn get_media_without_phash(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path FROM media 
             WHERE phash IS NULL 
             AND is_deleted = 0 
             AND (mime_type LIKE 'image/%' OR mime_type IS NULL)
             ORDER BY id ASC",
        )?;

        let items: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(items)
    }

    /// Get all image media items eligible for pHash scanning.
    /// Useful for full rescans to recover from stale/invalid hashes.
    pub fn get_all_media_for_phash_scan(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path FROM media
             WHERE is_deleted = 0
             AND (mime_type LIKE 'image/%' OR mime_type IS NULL)
             ORDER BY id ASC",
        )?;

        let items: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(items)
    }

    // --- Archive Operations (FR-NEW) ---

    /// Archive a media item (hide from timeline but keep in albums/search).
    pub fn archive_media(&self, media_id: i64) -> Result<()> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_archived = 1, archived_at = ?1 WHERE id = ?2",
            params![now, media_id],
        )?;
        Ok(())
    }

    /// Unarchive a media item (return to timeline).
    pub fn unarchive_media(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_archived = 0, archived_at = NULL WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    // --- Cloud-Only Mode ---

    /// Set the cloud-only status for a media item.
    pub fn set_cloud_only(&self, media_id: i64, is_cloud_only: bool) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_cloud_only = ?1 WHERE id = ?2",
            params![if is_cloud_only { 1 } else { 0 }, media_id],
        )?;
        Ok(())
    }

    /// Reconcile cloud-only flags against filesystem state.
    /// If local file is missing but Telegram ID exists, mark as cloud-only.
    ///
    /// This runs at startup over the whole library. It used to hold the connection
    /// while stat-ing every candidate file and then commit one implicit transaction
    /// per missing file, which is one fsync each on a path where nothing else can
    /// touch the database. The stat pass now happens with the lock released, and the
    /// writes go in as a single transaction.
    pub fn reconcile_cloud_only_flags(&self) -> Result<usize> {
        let candidates: Vec<(i64, String)> = {
            let conn = self.get_conn()?;
            let mut stmt = conn.prepare_cached(
                "SELECT id, file_path
                 FROM media
                 WHERE (is_deleted = 0 OR is_deleted IS NULL)
                   AND telegram_media_id IS NOT NULL
                   AND telegram_media_id != ''
                   AND (is_cloud_only IS NULL OR is_cloud_only = 0)",
            )?;

            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let missing: Vec<i64> = candidates
            .into_iter()
            .filter(|(_, file_path)| !Path::new(file_path).exists())
            .map(|(media_id, _)| media_id)
            .collect();

        if missing.is_empty() {
            return Ok(0);
        }

        self.mark_cloud_only(&missing)
    }

    /// Mark a batch of media items as cloud-only, in one transaction.
    pub fn mark_cloud_only(&self, media_ids: &[i64]) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
        }

        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let mut updated = 0usize;
        {
            let mut stmt = tx.prepare("UPDATE media SET is_cloud_only = 1 WHERE id = ?1")?;
            for media_id in media_ids {
                updated += stmt.execute([media_id])?;
            }
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Store a batch of perceptual hashes in one transaction.
    ///
    /// The scan paths compute thousands of these. Writing them one statement at a
    /// time meant one implicit transaction, and one fsync, per photo.
    pub fn update_phashes(&self, hashes: &[(i64, String)]) -> Result<usize> {
        if hashes.is_empty() {
            return Ok(0);
        }

        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let mut updated = 0usize;
        {
            let mut stmt = tx.prepare("UPDATE media SET phash = ?1 WHERE id = ?2")?;
            for (media_id, phash) in hashes {
                updated += stmt.execute(params![phash, media_id])?;
            }
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Get a single media item by ID.
    pub fn get_media_by_id(&self, media_id: i64) -> Result<Option<MediaItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media WHERE id = ?1"
        ))?;

        stmt.query_row([media_id], Self::map_media_row).optional()
    }

    /// Check if media with the given Telegram ID is marked as cloud-only.
    pub fn is_cloud_only_by_telegram_id(&self, telegram_id: &str) -> Result<bool> {
        let conn = self.get_conn()?;
        let mut stmt =
            conn.prepare_cached("SELECT is_cloud_only FROM media WHERE telegram_media_id = ?1")?;

        let mut rows = stmt.query([telegram_id])?;
        if let Some(row) = rows.next()? {
            let is_cloud_only: Option<i32> = row.get(0)?;
            Ok(is_cloud_only.map(|v| v != 0).unwrap_or(false))
        } else {
            Ok(false)
        }
    }

    /// Get all archived media items.
    pub fn get_archived_media(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE is_archived = 1 AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY archived_at DESC 
             LIMIT ?1 OFFSET ?2"
        ))?;

        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Find potential duplicates based on perceptual hash.
    ///
    /// Returns groups of media items with similar pHash values, oldest first
    /// within a group and largest group first.
    pub fn find_duplicates(&self) -> Result<Vec<Vec<MediaItem>>> {
        let candidates: Vec<(MediaItem, String)> = {
            let conn = self.get_conn()?;
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {MEDIA_COLUMNS}, phash
                 FROM media
                 WHERE phash IS NOT NULL AND is_deleted = 0
                 ORDER BY created_at ASC"
            ))?;

            let rows = stmt
                .query_map([], |row| Ok((Self::map_media_row(row)?, row.get(24)?)))?
                .filter_map(|r| r.ok())
                .collect();
            rows
            // The connection guard is dropped here, deliberately: clustering is pure
            // computation over data already in memory, and holding the single
            // database lock across it blocked every other query in the process.
        };

        Ok(cluster_by_phash(candidates, PHASH_DISTANCE_THRESHOLD))
    }
}
