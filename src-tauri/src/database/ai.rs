//! Reads and writes for the on-device AI pipeline: face detection and
//! recognition, CLIP embeddings, auto-tagging, and the scan queue that feeds
//! all three.

use super::*;

impl Database {
    // --- Face Operations ---

    pub fn add_faces(&self, media_id: i64, faces: &[crate::ai::Face]) -> Result<()> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;

        // Clear existing faces for this media item to prevent duplicates on rescan
        tx.execute("DELETE FROM faces WHERE media_id = ?1", [media_id])?;

        for face in faces {
            tx.execute(
                "INSERT INTO faces (media_id, x, y, width, height, score) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![media_id, face.x, face.y, face.width, face.height, face.score],
            )?;
        }

        // Mark media as scanned and face-scan complete (including zero-face result).
        tx.execute(
            "UPDATE media SET scan_status = 'scanned', face_status = 'done' WHERE id = ?1",
            [media_id],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn store_face_embedding(&self, face_id: i64, embedding: &[f32]) -> Result<Option<i64>> {
        let conn = self.get_conn()?;

        // Convert f32 vector to bytes
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for &val in embedding {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        // Match face to person (Simple Greedy Clustering)
        let person_id = self.match_face_to_person(&conn, embedding)?;

        // Update face record

        // Two `PRAGMA` reads and a `SELECT` per face, printed to stdout, used to sit
        // here to debug a foreign-key failure that no longer happens. They ran on every
        // embedding write, and `stmt.query_map(...)?` on a schema pragma is also a
        // plausible source of the panic that used to poison the connection mutex for
        // the rest of the process.
        conn.execute(
            "UPDATE faces SET embedding = ?1, person_id = ?2 WHERE rowid = ?3",
            rusqlite::params![bytes, person_id, face_id],
        )
        .inspect_err(|e| log::error!("Failed to update face {}: {}", face_id, e))?;

        // Update Person Cover if needed
        if let Some(pid) = person_id {
            // Check if person has a cover
            let has_cover: bool = conn.query_row(
                "SELECT cover_face_id FROM persons WHERE id = ?1",
                [pid],
                |row| row.get::<_, Option<i64>>(0).map(|id| id.is_some()),
            )?;

            if !has_cover {
                conn.execute(
                    "UPDATE persons SET cover_face_id = ?1 WHERE id = ?2",
                    [face_id, pid],
                )?;
            }
        }

        Ok(person_id)
    }

    // Simple clustering logic
    fn match_face_to_person(&self, conn: &Connection, embedding: &[f32]) -> Result<Option<i64>> {
        // Threshold for cosine similarity (0.0 to 1.0, higher is better)
        // ArcFace/MobileFaceNet usually uses 0.4 - 0.6
        const THRESHOLD: f32 = 0.5;

        // Fetch all persons and their cover faces embeddings?
        // For scalability, we should probably fetch centroids or just iterate all faces (slow)
        // For MVP: Iterate existing Persons, get ONE face (cover) and compare.

        let mut best_match: Option<i64> = None;
        let mut max_score = -1.0;

        let mut stmt = conn.prepare_cached(
            "SELECT p.id, f.embedding 
             FROM persons p 
             JOIN faces f ON p.cover_face_id = f.rowid 
             WHERE f.embedding IS NOT NULL",
        )?;

        let person_iter = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, bytes))
        })?;

        for p in person_iter {
            let (pid, bytes) = p?;
            // Decode embedding
            if bytes.len() % 4 != 0 {
                continue;
            }
            let count = bytes.len() / 4;
            let mut stored_emb = Vec::with_capacity(count);
            for i in 0..count {
                stored_emb.push(f32::from_le_bytes(
                    bytes[i * 4..(i + 1) * 4].try_into().unwrap(),
                ));
            }

            // Cosine Similarity
            let score = cosine_similarity(embedding, &stored_emb);
            if score > max_score {
                max_score = score;
                best_match = Some(pid);
            }
        }

        if max_score > THRESHOLD {
            log::debug!(
                "Face matched to person {} (score: {:.3})",
                best_match.unwrap(),
                max_score
            );
            return Ok(best_match);
        }

        log::debug!(
            "No face match (max score {:.3}), creating a new person",
            max_score
        );

        // No match found -> Create new person
        // Name defaults to "Person {id}" or similar?
        // We'll insert with a temp name and update later or handle in UI

        // We need to execute on conn.
        // Warning: if match_face_to_person is called inside a txn, this might fail?
        // But store_face_embedding gets a managed conn, which is a MutexGuard.

        conn.execute("INSERT INTO persons (name) VALUES ('New Person')", [])?;
        let new_id = conn.last_insert_rowid();

        // Update name to "Person {id}"
        conn.execute(
            "UPDATE persons SET name = ?1 WHERE id = ?2",
            rusqlite::params![format!("Person {}", new_id), new_id],
        )?;

        Ok(Some(new_id))
    }

    // --- CLIP Operations ---

    pub fn store_clip_embedding(&self, media_id: i64, embedding: &[f32]) -> Result<()> {
        let conn = self.get_conn()?;

        // Convert f32 vector to bytes (Little Endian)
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for &val in embedding {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        conn.execute(
            "UPDATE media SET clip_embedding = ?1, clip_status = 'scanned' WHERE id = ?2",
            rusqlite::params![bytes, media_id],
        )?;
        Ok(())
    }

    pub fn mark_clip_failed(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET clip_status = 'failed' WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    pub fn get_pending_clip_items(&self, limit: i32) -> Result<Vec<(i64, String)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path 
             FROM media 
             WHERE (clip_status = 'pending' OR clip_status IS NULL) 
               AND (is_deleted = 0 OR is_deleted IS NULL)
               AND mime_type LIKE 'image/%'
             LIMIT ?1",
        )?;

        let items = stmt
            .query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(items)
    }

    pub fn get_all_clip_embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, clip_embedding FROM media WHERE clip_embedding IS NOT NULL",
        )?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;

                // Convert bytes back to f32
                if !bytes.len().is_multiple_of(4) {
                    // Return empty or handle error? silently skip bad data
                    return Ok((id, Vec::new()));
                }

                let count = bytes.len() / 4;
                let mut embedding = Vec::with_capacity(count);
                for i in 0..count {
                    let start = i * 4;
                    let end = start + 4;
                    let slice = &bytes[start..end];
                    // unwrap safe because confirmed 4 bytes
                    let val = f32::from_le_bytes(slice.try_into().unwrap());
                    embedding.push(val);
                }

                Ok((id, embedding))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    pub fn get_next_item_to_scan(&self) -> Result<Option<MediaItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            &format!("SELECT {MEDIA_COLUMNS}
             FROM media 
             WHERE (scan_status = 'pending' OR scan_status IS NULL) AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY created_at DESC 
             LIMIT 1")
        )?;

        stmt.query_row([], Self::map_media_row).optional()
    }

    pub fn mark_media_scan_failed(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET scan_status = 'failed', face_status = 'failed' WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    pub fn get_faces(&self, media_id: i64) -> Result<Vec<crate::ai::Face>> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT x, y, width, height, score FROM faces WHERE media_id = ?1")?;

        let face_iter = stmt.query_map([media_id], |row| {
            Ok(crate::ai::Face {
                x: row.get(0)?,
                y: row.get(1)?,
                width: row.get(2)?,
                height: row.get(3)?,
                score: row.get(4)?,
            })
        })?;

        let mut faces = Vec::new();
        for face in face_iter {
            faces.push(face?);
        }
        Ok(faces)
    }

    pub fn get_all_faces_for_media(&self, media_id: i64) -> Result<Vec<(i64, crate::ai::Face)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare("SELECT rowid, x, y, width, height, score FROM faces WHERE media_id = ?1")?;

        let face_iter = stmt.query_map([media_id], |row| {
            Ok((
                row.get(0)?,
                crate::ai::Face {
                    x: row.get(1)?,
                    y: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    score: row.get(5)?,
                },
            ))
        })?;

        let mut faces = Vec::new();
        for face in face_iter {
            faces.push(face?);
        }
        Ok(faces)
    }

    // --- Tag Operations ---

    pub fn get_all_tags(&self) -> Result<Vec<Tag>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT t.id, t.name, COUNT(mt.media_id) as count 
             FROM tags t
             LEFT JOIN media_tags mt ON t.id = mt.tag_id
             GROUP BY t.id
             ORDER BY count DESC, t.name ASC",
        )?;

        let tags_iter = stmt.query_map([], |row| {
            Ok(Tag {
                id: row.get(0)?,
                name: row.get(1)?,
                media_count: row.get(2)?,
            })
        })?;

        tags_iter.collect()
    }

    pub fn get_media_by_tag(
        &self,
        tag_name: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(0, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS_M}
             FROM media m
             JOIN media_tags mt ON m.id = mt.media_id
             JOIN tags t ON mt.tag_id = t.id
             WHERE t.name = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL)
             ORDER BY m.created_at DESC
             LIMIT ?2 OFFSET ?3"
        ))?;

        let media_iter = stmt.query_map(params![tag_name, limit, offset], Self::map_media_row)?;

        media_iter.collect()
    }

    pub fn add_tags(&self, media_id: i64, tags: &[(String, f64)]) -> Result<()> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;

        {
            let mut insert_tag = tx.prepare("INSERT OR IGNORE INTO tags (name) VALUES (?1)")?;
            let mut get_tag_id = tx.prepare("SELECT id FROM tags WHERE name = ?1")?;
            let mut insert_media_tag = tx.prepare("INSERT OR REPLACE INTO media_tags (media_id, tag_id, confidence) VALUES (?1, ?2, ?3)")?;

            for (tag_name, confidence) in tags {
                insert_tag.execute([tag_name])?;
                let tag_id: i64 = get_tag_id.query_row([tag_name], |row| row.get(0))?;
                insert_media_tag.execute(params![media_id, tag_id, confidence])?;
            }

            // Mark as done
            tx.execute(
                "UPDATE media SET tags_status = 'done' WHERE id = ?1",
                [media_id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    // The tag worker logs failures instead of recording them, so nothing calls this.
    // Kept as the counterpart to `mark_clip_failed` until the worker uses it.
    #[allow(dead_code)]
    pub fn mark_tags_failed(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET tags_status = 'failed' WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    /// Requeue image items that still need object-tag processing.
    /// Returns number of items marked pending.
    pub fn queue_pending_tag_scans(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        let updated = conn.execute(
            "UPDATE media
             SET scan_status = 'pending'
             WHERE (is_deleted = 0 OR is_deleted IS NULL)
               AND (mime_type LIKE 'image/%' OR mime_type IS NULL)
               AND (tags_status IS NULL OR tags_status != 'done')",
            [],
        )?;
        Ok(updated)
    }

    /// Requeue image items that still need face processing.
    /// Uses dedicated face_status so zero-face results are not requeued endlessly.
    pub fn queue_pending_face_scans(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        let updated = conn.execute(
            "UPDATE media
             SET scan_status = 'pending', face_status = 'pending'
             WHERE (is_deleted = 0 OR is_deleted IS NULL)
               AND (mime_type LIKE 'image/%' OR mime_type IS NULL)
               AND (face_status IS NULL OR face_status != 'done')",
            [],
        )?;
        Ok(updated)
    }

    pub fn mark_media_scanned(&self, media_id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET scan_status = 'scanned' WHERE id = ?1",
            [media_id],
        )?;
        Ok(())
    }

    // Recovery helper with no caller: the startup path requeues through
    // `queue_pending_tag_scans` instead. Kept because the NULL-embedding case it repairs
    // is real and undetected elsewhere.
    #[allow(dead_code)]
    pub fn reset_stuck_scans(&self) -> Result<usize> {
        let conn = self.get_conn()?;

        // Find media_ids that have faces with NULL embedding (incomplete processing)
        let mut stmt =
            conn.prepare_cached("SELECT DISTINCT media_id FROM faces WHERE embedding IS NULL")?;

        let media_ids: Vec<i64> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>>>()?;

        if media_ids.is_empty() {
            return Ok(0);
        }

        log::info!(
            "Found {} items with incomplete AI processing. Resetting...",
            media_ids.len()
        );

        let tx = conn.unchecked_transaction()?;

        // 1. Delete the partial face records
        tx.execute("DELETE FROM faces WHERE embedding IS NULL", [])?;

        // 2. Mark media as pending
        let mut update_stmt =
            tx.prepare("UPDATE media SET scan_status = 'pending' WHERE id = ?1")?;
        for id in &media_ids {
            update_stmt.execute([id])?;
        }

        drop(update_stmt);
        tx.commit()?;
        Ok(media_ids.len())
    }

    pub fn get_tags_for_media(&self, media_id: i64) -> Result<Vec<String>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT t.name 
             FROM tags t
             JOIN media_tags mt ON t.id = mt.tag_id
             WHERE mt.media_id = ?1
             ORDER BY mt.confidence DESC",
        )?;

        let tags_iter = stmt.query_map([media_id], |row| row.get(0))?;
        tags_iter.collect()
    }
}
