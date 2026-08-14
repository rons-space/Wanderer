//! People, which are clusters of face embeddings the user has named.

use super::*;

impl Database {
    // --- People / Face Recognition (FR-6) ---

    /// Get all people with face counts
    /// Get all people with face counts
    pub fn get_people(&self) -> Result<Vec<Person>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT p.id, p.name, 
                    (SELECT COUNT(*) FROM faces f WHERE f.person_id = p.id) as face_count,
                    (SELECT m.thumbnail_path FROM faces f2 
                     JOIN media m ON f2.media_id = m.id 
                     WHERE f2.person_id = p.id LIMIT 1) as cover_path
             FROM persons p
             ORDER BY face_count DESC",
        )?;

        let persons = stmt.query_map([], |row| {
            Ok(Person {
                id: row.get(0)?,
                name: row.get(1)?,
                face_count: row.get(2)?,
                cover_path: row.get(3)?,
            })
        })?;

        let mut result = Vec::new();
        for p in persons {
            result.push(p?);
        }
        Ok(result)
    }

    /// Update a person's name
    pub fn update_person_name(&self, person_id: i64, name: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "UPDATE persons SET name = ?1, updated_at = ?2 WHERE id = ?3",
            (name, now, person_id),
        )?;
        Ok(())
    }

    /// Merge multiple persons into a target person
    pub fn merge_persons(&self, target_id: i64, source_ids: &[i64]) -> Result<()> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;

        for &source_id in source_ids {
            // Move faces to target person
            tx.execute(
                "UPDATE faces SET person_id = ?1 WHERE person_id = ?2",
                rusqlite::params![target_id, source_id],
            )?;

            // Delete source person
            tx.execute("DELETE FROM persons WHERE id = ?1", [source_id])?;
        }

        // Update target person's face_count and cover info implicitly by next query?
        // Or updated_at?
        let now = OffsetDateTime::now_utc().unix_timestamp();
        tx.execute(
            "UPDATE persons SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, target_id],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Get all media items containing a specific person's face
    pub fn get_media_by_person(
        &self,
        person_id: i64,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<MediaItem>> {
        // Clamped like every other paginated read: a negative limit reaches SQLite as
        // "no limit" and a negative offset is an error, and both arrive straight from
        // the frontend.
        let limit = limit.clamp(0, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT DISTINCT {MEDIA_COLUMNS_M}
             FROM media m
             JOIN faces f ON f.media_id = m.id
             WHERE f.person_id = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL) AND (m.is_archived = 0 OR m.is_archived IS NULL)
             ORDER BY m.created_at DESC
             LIMIT ?2 OFFSET ?3"),
        )?;

        let items = stmt.query_map((person_id, limit, offset), Self::map_media_row)?;

        let mut result = Vec::new();
        for item in items {
            result.push(item?);
        }
        Ok(result)
    }
}
