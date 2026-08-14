//! Albums and their membership.

use super::*;

impl Database {
    // --- Album Operations ---

    /// Create a new album with the given name.
    ///
    /// # Errors
    /// Returns an error if the name is empty or whitespace-only.
    pub fn create_album(&self, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName(
                "Album name cannot be empty".to_string(),
            ));
        }

        let conn = self.get_conn()?;
        let created_at = OffsetDateTime::now_utc().unix_timestamp();

        conn.execute(
            "INSERT INTO albums (name, created_at) VALUES (?1, ?2)",
            (name, created_at),
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_albums(&self) -> Result<Vec<Album>> {
        let conn = self.get_conn()?;
        // Use a subquery to get the first non-archived, non-deleted media item for cover
        let mut stmt = conn.prepare_cached(
            "SELECT a.id, a.name, a.created_at,
                    (SELECT m.thumbnail_path FROM album_media am2
                     JOIN media m ON am2.media_id = m.id
                     WHERE am2.album_id = a.id
                       AND (m.is_deleted = 0 OR m.is_deleted IS NULL)
                       AND (m.is_archived = 0 OR m.is_archived IS NULL)
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_thumbnail,
                    (SELECT m.file_path FROM album_media am2
                     JOIN media m ON am2.media_id = m.id
                     WHERE am2.album_id = a.id
                       AND (m.is_deleted = 0 OR m.is_deleted IS NULL)
                       AND (m.is_archived = 0 OR m.is_archived IS NULL)
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_file_path
             FROM albums a
             ORDER BY a.created_at DESC",
        )?;

        let albums_iter = stmt.query_map([], |row| {
            let thumbnail_path: Option<String> = row.get(3)?;
            let file_path: Option<String> = row.get(4)?;
            let cover = thumbnail_path.or(file_path);

            Ok(Album {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                cover_path: cover,
            })
        })?;

        let mut result = Vec::new();
        for album in albums_iter {
            result.push(album?);
        }
        Ok(result)
    }

    pub fn add_media_to_album(&self, album_id: i64, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        let added_at = OffsetDateTime::now_utc().unix_timestamp();

        conn.execute(
            "INSERT INTO album_media (album_id, media_id, added_at) VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
            (album_id, media_id, added_at),
        )?;
        Ok(())
    }

    pub fn get_album_media(
        &self,
        album_id: i64,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT {MEDIA_COLUMNS_M}
             FROM media m
             INNER JOIN album_media am ON m.id = am.media_id
             WHERE am.album_id = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL) AND (m.is_archived = 0 OR m.is_archived IS NULL)
             ORDER BY am.added_at DESC
             LIMIT ?2 OFFSET ?3")
        )?;

        let media_iter = stmt.query_map(params![album_id, limit, offset], Self::map_media_row)?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }
}
