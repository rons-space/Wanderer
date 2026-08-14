//! The upload queue that feeds the Telegram worker.

use super::*;

impl Database {
    // --- Queue Operations ---

    pub fn add_to_queue(&self, file_path: &str) -> Result<()> {
        let conn = self.get_conn()?;

        // The dedupe used to be a SELECT COUNT followed by an INSERT, which the watcher
        // and an import can both pass before either writes, queueing the same file
        // twice and uploading it twice. The unique index from migration 20 decides it
        // instead.
        //
        // The WHERE on the conflict clause preserves the old semantics exactly: a row
        // that is already pending or uploading is left alone, and anything else, a
        // completed or failed upload of a path being queued again, is reset to pending.
        // Without it this would silently become "a file can only ever be uploaded
        // once", which is not what the count check did.
        let added_at = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "INSERT INTO upload_queue (file_path, status, added_at) VALUES (?1, 'pending', ?2)
             ON CONFLICT(file_path) DO UPDATE SET
                 status = 'pending',
                 retries = 0,
                 error_msg = NULL,
                 added_at = excluded.added_at
             WHERE upload_queue.status NOT IN ('pending', 'uploading')",
            (file_path, added_at),
        )?;
        Ok(())
    }

    pub fn get_next_pending_item(&self) -> Result<Option<QueueItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path, status, retries, error_msg, added_at 
             FROM upload_queue 
             WHERE status = 'pending' 
             ORDER BY added_at ASC 
             LIMIT 1",
        )?;

        stmt.query_row([], |row| {
            Ok(QueueItem {
                id: row.get(0)?,
                file_path: row.get(1)?,
                status: row.get(2)?,
                retries: row.get(3)?,
                error_msg: row.get(4)?,
                added_at: row.get(5)?,
            })
        })
        .optional()
    }

    pub fn get_queue_status(&self) -> Result<Vec<QueueItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_path, status, retries, error_msg, added_at
             FROM upload_queue
             ORDER BY added_at DESC
             LIMIT 50",
        )?;

        let iter = stmt.query_map([], |row| {
            Ok(QueueItem {
                id: row.get(0)?,
                file_path: row.get(1)?,
                status: row.get(2)?,
                retries: row.get(3)?,
                error_msg: row.get(4)?,
                added_at: row.get(5)?,
            })
        })?;

        let mut items = Vec::new();
        for i in iter {
            items.push(i?);
        }
        Ok(items)
    }

    pub fn mark_media_uploaded_by_path(&self, path: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let uploaded_at = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "UPDATE media SET uploaded_at = ?1 WHERE file_path = ?2",
            (uploaded_at, path),
        )?;
        Ok(())
    }

    pub fn update_queue_status(
        &self,
        id: i64,
        status: &str,
        error_msg: Option<&str>,
    ) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE upload_queue SET status = ?1, error_msg = ?2 WHERE id = ?3",
            (status, error_msg, id),
        )?;
        Ok(())
    }

    pub fn get_queue_counts(&self) -> Result<QueueCounts> {
        let conn = self.get_conn()?;

        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM upload_queue WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;

        let uploading: i64 = conn.query_row(
            "SELECT COUNT(*) FROM upload_queue WHERE status = 'uploading'",
            [],
            |row| row.get(0),
        )?;

        let failed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM upload_queue WHERE status = 'failed'",
            [],
            |row| row.get(0),
        )?;

        Ok(QueueCounts {
            pending,
            uploading,
            failed,
        })
    }

    pub fn retry_failed_item(&self, id: i64) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE upload_queue SET status = 'pending', error_msg = NULL, retries = retries + 1 WHERE id = ?1 AND status = 'failed'",
            [id],
        )?;
        Ok(())
    }
}
