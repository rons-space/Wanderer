//! Reads the sync manifest and the backup exporter need, which cut across
//! media, albums and tags.

use super::*;

impl Database {
    // --- Sync Helper Methods ---

    /// Get all media items with their sync-relevant fields (for export)
    pub fn get_all_media_for_sync(&self) -> Result<Vec<MediaItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE (is_deleted = 0 OR is_deleted IS NULL)"
        ))?;

        let items: Vec<MediaItem> = stmt
            .query_map([], Self::map_media_row)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(items)
    }

    /// Album names for every media item that is in at least one album.
    ///
    /// The sync manifest needs names and nothing else. Its previous per-photo query
    /// ran two correlated cover subqueries over the whole library for each photo, to
    /// build a cover path the manifest then threw away. This is the same information
    /// in a single scan, and it was that query's only caller, so the query is gone.
    pub fn album_names_by_media(&self) -> Result<std::collections::HashMap<i64, Vec<String>>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT am.media_id, a.name
             FROM album_media am
             INNER JOIN albums a ON a.id = am.album_id
             ORDER BY am.media_id",
        )?;

        let mut by_media: std::collections::HashMap<i64, Vec<String>> =
            std::collections::HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (media_id, name) = row?;
            by_media.entry(media_id).or_default().push(name);
        }

        Ok(by_media)
    }

    /// Get a media item by its blake3 hash
    pub fn get_media_by_hash(&self, hash: &str) -> Result<Option<MediaItem>> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            &format!(
                "SELECT {MEDIA_COLUMNS}
             FROM media WHERE file_hash = ?1"
            ),
            [hash],
            Self::map_media_row,
        );

        match result {
            Ok(item) => Ok(Some(item)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get an album by its name
    pub fn get_album_by_name(&self, name: &str) -> Result<Option<Album>> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            "SELECT id, name, created_at, NULL as cover_path FROM albums WHERE name = ?1",
            [name],
            |row| {
                Ok(Album {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    cover_path: row.get(3)?,
                })
            },
        );

        match result {
            Ok(album) => Ok(Some(album)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Set the favorite status of a media item (used by sync)
    pub fn set_favorite(&self, media_id: i64, is_favorite: bool) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_favorite = ?1 WHERE id = ?2",
            (is_favorite as i32, media_id),
        )?;
        Ok(())
    }
}
