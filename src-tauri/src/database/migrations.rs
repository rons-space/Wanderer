//! The migration chain.
//!
//! One numbered block per schema change, each guarded by `user_version` and each
//! ending by setting it, so the chain is idempotent and a database from any
//! released version reaches the current one by running the blocks it missed.
//! Blocks are append-only: editing a released one leaves the databases that
//! already ran it untouched and the two schemas diverge.

use super::*;

impl Database {
    // Every step in the chain closes with `version = N;` so the next one can be appended
    // without reading the one above it. That makes the final assignment dead by
    // construction, and deleting it would leave the last step shaped differently from
    // all the others and the next author with a silently skipped migration.
    #[allow(unused_assignments)]
    pub(super) fn migrate(conn: &Connection) -> Result<()> {
        let mut version: i32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
        log::info!("Database schema version: {}", version);

        if version < 1 {
            // Initial Schema
            conn.execute_batch(
                "BEGIN;
                CREATE TABLE IF NOT EXISTS config (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS media (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    file_path TEXT NOT NULL,       -- Local path
                    file_hash TEXT UNIQUE,         -- Blake3 hash for deduplication
                    telegram_media_id TEXT,        -- Grammers/TL media reference (serialized)
                    mime_type TEXT,
                    width INTEGER,
                    height INTEGER,
                    duration INTEGER,
                    size_bytes INTEGER,
                    created_at INTEGER NOT NULL,   -- Unix timestamp
                    uploaded_at INTEGER            -- Unix timestamp, NULL if not uploaded
                );

                CREATE TABLE IF NOT EXISTS upload_queue (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    file_path TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending', -- pending, uploading, completed, failed
                    retries INTEGER DEFAULT 0,
                    error_msg TEXT,
                    added_at INTEGER NOT NULL
                );
                
                PRAGMA user_version = 1;
                COMMIT;",
            )?;
            version = 1;
        }

        if version < 2 {
            // Migration 2: Add thumbnail_path
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN thumbnail_path TEXT;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
            version = 2;
        }

        if version < 3 {
            // Migration 3: Add albums tables
            conn.execute_batch(
                "BEGIN;
                  CREATE TABLE IF NOT EXISTS albums (
                      id INTEGER PRIMARY KEY AUTOINCREMENT,
                      name TEXT NOT NULL,
                      created_at INTEGER NOT NULL
                  );

                  CREATE TABLE IF NOT EXISTS album_media (
                      album_id INTEGER NOT NULL,
                      media_id INTEGER NOT NULL,
                      added_at INTEGER NOT NULL,
                      PRIMARY KEY (album_id, media_id),
                      FOREIGN KEY(album_id) REFERENCES albums(id) ON DELETE CASCADE,
                      FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE
                  );
                  PRAGMA user_version = 3;
                  COMMIT;",
            )?;
            version = 3; // Ensure version is updated
        }

        if version < 4 {
            // Migration 4: Add faces table and scan_status to media
            // Note: SQLite doesn't support ADD COLUMN IF NOT EXISTS easily for multiple columns or with certain checks,
            // but ADD COLUMN is widely supported.
            // We adding scan_status column.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN scan_status TEXT DEFAULT 'pending'; -- pending, scanned, failed
                 
                 CREATE TABLE IF NOT EXISTS faces (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     media_id INTEGER NOT NULL,
                     x REAL NOT NULL,
                     y REAL NOT NULL,
                     width REAL NOT NULL,
                     height REAL NOT NULL,
                     score REAL NOT NULL,
                     label TEXT,
                     FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
            version = 4;
        }

        if version < 5 {
            // Migration 5: Add PRD fields - favorites, ratings, EXIF, GPS, soft delete, FTS5, people
            conn.execute_batch(
                "BEGIN;
                 -- Add new columns to media table
                 ALTER TABLE media ADD COLUMN date_taken TEXT;
                 ALTER TABLE media ADD COLUMN latitude REAL;
                 ALTER TABLE media ADD COLUMN longitude REAL;
                 ALTER TABLE media ADD COLUMN camera_make TEXT;
                 ALTER TABLE media ADD COLUMN camera_model TEXT;
                 ALTER TABLE media ADD COLUMN is_favorite INTEGER DEFAULT 0;
                 ALTER TABLE media ADD COLUMN rating INTEGER DEFAULT 0;
                 ALTER TABLE media ADD COLUMN is_deleted INTEGER DEFAULT 0;
                 ALTER TABLE media ADD COLUMN deleted_at INTEGER;
                 
                 -- Create FTS5 virtual table for full-text search
                 CREATE VIRTUAL TABLE IF NOT EXISTS media_fts USING fts5(
                     file_path,
                     tags,
                     people,
                     tokenize = 'porter'
                 );
                 
                 -- Tags table for AI-generated labels
                 CREATE TABLE IF NOT EXISTS tags (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     media_id INTEGER NOT NULL,
                     tag TEXT NOT NULL,
                     confidence REAL DEFAULT 1.0,
                     created_at INTEGER NOT NULL,
                     FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_tags_media ON tags(media_id);
                 CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
                 
                 -- People table for face recognition clustering
                 CREATE TABLE IF NOT EXISTS people (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT,
                     representative_embedding BLOB,
                     photo_count INTEGER DEFAULT 0,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 
                 -- Update faces table to add person_id and embedding
                 ALTER TABLE faces ADD COLUMN person_id INTEGER REFERENCES people(id) ON DELETE SET NULL;
                 ALTER TABLE faces ADD COLUMN embedding BLOB;
                 CREATE INDEX IF NOT EXISTS idx_faces_person ON faces(person_id);
                 
                 PRAGMA user_version = 5;
                 COMMIT;",
            )?;
            version = 5;
        }

        if version < 6 {
            // Migration 6: Add Perceptual Hash for duplicate detection
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN phash TEXT;
                 CREATE INDEX IF NOT EXISTS idx_media_phash ON media(phash);
                 PRAGMA user_version = 6;
                 COMMIT;",
            )?;
            version = 6;
        }

        if version < 7 {
            // Migration 7: Add config table for user settings
            // Drop existing config table if it exists with different schema
            conn.execute_batch(
                "BEGIN;
                 DROP TABLE IF EXISTS config;
                 CREATE TABLE config (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 -- Insert default settings
                 INSERT INTO config (key, value, updated_at) VALUES 
                     ('cache_size_mb', '5000', strftime('%s', 'now')),
                     ('ai_face_enabled', 'false', strftime('%s', 'now')),
                     ('ai_tags_enabled', 'false', strftime('%s', 'now')),
                     ('day_separators', 'true', strftime('%s', 'now'));
                 PRAGMA user_version = 7;
                 COMMIT;",
            )?;
            version = 7;
        }

        // Migration 8: Add is_archived column for Archive feature
        if version < 8 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN is_archived INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE media ADD COLUMN archived_at INTEGER;
                 PRAGMA user_version = 8;
                 COMMIT;",
            )?;
            version = 8;
        }

        // Migration 9: Add is_cloud_only column for Cloud-Only mode
        if version < 9 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN is_cloud_only INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 9;
                 COMMIT;",
            )?;
            version = 9;
        }

        // Migration 10: Add clip_embedding and clip_status
        if version < 10 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN clip_embedding BLOB;
                 ALTER TABLE media ADD COLUMN clip_status TEXT DEFAULT 'pending';
                 PRAGMA user_version = 10;
                 COMMIT;",
            )?;
            version = 10;
        }

        // Migration 11: Add tags and media_tags tables for object detection
        if version < 11 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS tags (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT NOT NULL UNIQUE
                 );
                 CREATE TABLE IF NOT EXISTS media_tags (
                     media_id INTEGER NOT NULL,
                     tag_id INTEGER NOT NULL,
                     confidence REAL NOT NULL DEFAULT 1.0,
                     PRIMARY KEY (media_id, tag_id),
                     FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE,
                     FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags(tag_id);
                 ALTER TABLE media ADD COLUMN tags_status TEXT DEFAULT 'pending';
                 PRAGMA user_version = 11;
                 COMMIT;",
            )?;
            version = 11;
        }

        // Migration 12: Add embedding to faces and create persons table (FR-6)
        if version < 12 {
            // Migration 12: Add embedding to faces and create persons table (FR-6)
            // Idempotent checks for columns
            let embedding_exists: bool = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('faces') WHERE name='embedding'",
                    [],
                    |row| row.get::<_, i32>(0),
                )
                .unwrap_or(0)
                > 0;

            if !embedding_exists {
                conn.execute("ALTER TABLE faces ADD COLUMN embedding BLOB", [])?;
            }

            let person_id_exists: bool = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('faces') WHERE name='person_id'",
                    [],
                    |row| row.get::<_, i32>(0),
                )
                .unwrap_or(0)
                > 0;

            if !person_id_exists {
                conn.execute(
                    "ALTER TABLE faces ADD COLUMN person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL",
                    [],
                )?;
            }

            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS persons (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT NOT NULL,
                     cover_face_id INTEGER,
                     created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                     updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                     FOREIGN KEY(cover_face_id) REFERENCES faces(id) ON DELETE SET NULL
                 );
                 PRAGMA user_version = 12;
                 COMMIT;",
            )?;
            version = 12;
        }

        // Migration 13: Fix foreign key in persons table (rowid -> id)
        if version < 13 {
            // Recreate persons table with correct FK to faces(id) instead of faces(rowid)
            conn.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 BEGIN;
                 CREATE TABLE persons_new (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     name TEXT NOT NULL,
                     cover_face_id INTEGER,
                     created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                     updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                     FOREIGN KEY(cover_face_id) REFERENCES faces(id) ON DELETE SET NULL
                 );
                 INSERT INTO persons_new SELECT id, name, cover_face_id, created_at, updated_at FROM persons;
                 DROP TABLE persons;
                 ALTER TABLE persons_new RENAME TO persons;
                 PRAGMA user_version = 13;
                 COMMIT;
                 PRAGMA foreign_keys = ON;",
            )?;
            version = 13;
        }
        if version < 14 {
            // Migration 14: Repair 'faces' table FK pointing to 'people' (should be 'persons')
            conn.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 BEGIN;
                 CREATE TABLE faces_new (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     media_id INTEGER NOT NULL,
                     x REAL NOT NULL,
                     y REAL NOT NULL,
                     width REAL NOT NULL,
                     height REAL NOT NULL,
                     score REAL NOT NULL,
                     label TEXT,
                     embedding BLOB,
                     person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL,
                     FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE
                 );
                 INSERT INTO faces_new SELECT id, media_id, x, y, width, height, score, label, embedding, person_id FROM faces;
                 DROP TABLE faces;
                 ALTER TABLE faces_new RENAME TO faces;
                 PRAGMA user_version = 14;
                 COMMIT;
                 PRAGMA foreign_keys = ON;",
            )?;
            version = 14;
        }

        if version < 15 {
            // Migration 15: Cleanup ghost persons (created during failed FK runs).
            //
            // The EXISTS guard is load-bearing. `NOT IN` against an empty subquery is
            // true for every row, so without it this deletes every named person on any
            // library where no face has been assigned one yet, which is the normal
            // state for a user who has not run face detection. The guard makes the
            // statement a no-op in exactly that case, and leaves the intended cleanup
            // untouched once at least one assignment exists.
            conn.execute_batch(
                "BEGIN;
                  DELETE FROM persons
                  WHERE EXISTS (SELECT 1 FROM faces WHERE person_id IS NOT NULL)
                    AND id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);
                  PRAGMA user_version = 15;
                  COMMIT;",
            )?;
            version = 15;
        }

        if version < 16 {
            // Migration 16: Normalize tag schema.
            // Legacy DBs used `tags(media_id, tag, confidence, created_at)`.
            // Current schema uses `tags(name)` + `media_tags(media_id, tag_id, confidence)`.
            let tag_columns: Vec<String> = {
                let mut stmt = conn.prepare_cached("PRAGMA table_info('tags')")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                rows.filter_map(|r| r.ok()).collect()
            };

            let has_name = tag_columns.iter().any(|c| c == "name");
            let is_legacy = tag_columns.iter().any(|c| c == "tag")
                && tag_columns.iter().any(|c| c == "media_id");

            if is_legacy && !has_name {
                conn.execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     BEGIN;
                     ALTER TABLE tags RENAME TO tags_legacy;
                     DROP TABLE IF EXISTS media_tags;

                     CREATE TABLE tags (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         name TEXT NOT NULL UNIQUE
                     );

                     CREATE TABLE media_tags (
                         media_id INTEGER NOT NULL,
                         tag_id INTEGER NOT NULL,
                         confidence REAL NOT NULL DEFAULT 1.0,
                         PRIMARY KEY (media_id, tag_id),
                         FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE,
                         FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
                     );
                     CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags(tag_id);

                     INSERT OR IGNORE INTO tags (name)
                     SELECT DISTINCT tag
                     FROM tags_legacy
                     WHERE tag IS NOT NULL AND TRIM(tag) <> '';

                     INSERT OR REPLACE INTO media_tags (media_id, tag_id, confidence)
                     SELECT tl.media_id, t.id, COALESCE(tl.confidence, 1.0)
                     FROM tags_legacy tl
                     JOIN tags t ON t.name = tl.tag
                     WHERE tl.media_id IS NOT NULL;

                     DROP TABLE tags_legacy;
                     PRAGMA user_version = 16;
                     COMMIT;
                     PRAGMA foreign_keys = ON;",
                )?;
            } else {
                conn.execute_batch(
                    "BEGIN;
                     CREATE TABLE IF NOT EXISTS tags (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         name TEXT NOT NULL UNIQUE
                     );
                     CREATE TABLE IF NOT EXISTS media_tags (
                         media_id INTEGER NOT NULL,
                         tag_id INTEGER NOT NULL,
                         confidence REAL NOT NULL DEFAULT 1.0,
                         PRIMARY KEY (media_id, tag_id),
                         FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE,
                         FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
                     );
                     CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags(tag_id);
                     PRAGMA user_version = 16;
                     COMMIT;",
                )?;
            }

            version = 16;
        }

        if version < 17 {
            // Migration 17: Ensure key settings exist and default AI toggles to OFF
            // for fresh/partial installs without overriding explicit user choices.
            conn.execute_batch(
                "BEGIN;
                 INSERT OR IGNORE INTO config (key, value, updated_at) VALUES
                     ('cache_size_mb', '5000', strftime('%s', 'now')),
                     ('view_cache_max_size_mb', '2000', strftime('%s', 'now')),
                     ('view_cache_retention_hours', '24', strftime('%s', 'now')),
                     ('ai_face_enabled', 'false', strftime('%s', 'now')),
                     ('ai_tags_enabled', 'false', strftime('%s', 'now')),
                     ('timeline_grouping', 'day', strftime('%s', 'now'));
                 PRAGMA user_version = 17;
                 COMMIT;",
            )?;
            version = 17;
        }

        if version < 18 {
            // Migration 18: Track face scan completion independently from shared scan_status.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN face_status TEXT DEFAULT 'pending';
                 UPDATE media
                 SET face_status = 'done'
                 WHERE EXISTS (SELECT 1 FROM faces f WHERE f.media_id = media.id);
                 PRAGMA user_version = 18;
                 COMMIT;",
            )?;
            version = 18;
        }

        if version < 19 {
            // Migration 19: Security state defaults and encrypted-upload tracking.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN is_encrypted INTEGER DEFAULT 0;
                 INSERT OR IGNORE INTO config (key, value, updated_at) VALUES
                     ('security_mode', 'unset', strftime('%s', 'now')),
                     ('security_onboarding_complete', 'false', strftime('%s', 'now'));
                 PRAGMA user_version = 19;
                 COMMIT;",
            )?;
            version = 19;
        }

        if version < 20 {
            // Migration 20: make the search index maintain itself, index the columns
            // every list query filters on, and stop the upload queue holding duplicates.
            //
            // The FTS table was a standalone fts5 written by exactly one manual INSERT
            // on the import path. Anything ingested by sync was never indexed, and
            // nothing deleted was ever removed, so search returned stale rows and
            // missed new ones. External-content FTS5 keyed to `media.id` with triggers
            // makes that structurally impossible: the index cannot drift from the table
            // because SQLite maintains it.
            //
            // The `tags` and `people` columns are dropped rather than carried over. No
            // code ever wrote to them, so they only ever matched empty strings.
            conn.execute_batch(
                "BEGIN;
                 DROP TABLE IF EXISTS media_fts;
                 CREATE VIRTUAL TABLE media_fts USING fts5(
                     file_path,
                     content = 'media',
                     content_rowid = 'id',
                     tokenize = 'porter'
                 );
                 INSERT INTO media_fts (rowid, file_path) SELECT id, file_path FROM media;

                 CREATE TRIGGER media_fts_insert AFTER INSERT ON media BEGIN
                     INSERT INTO media_fts (rowid, file_path) VALUES (new.id, new.file_path);
                 END;
                 CREATE TRIGGER media_fts_delete AFTER DELETE ON media BEGIN
                     INSERT INTO media_fts (media_fts, rowid, file_path)
                     VALUES ('delete', old.id, old.file_path);
                 END;
                 CREATE TRIGGER media_fts_update AFTER UPDATE OF file_path ON media BEGIN
                     INSERT INTO media_fts (media_fts, rowid, file_path)
                     VALUES ('delete', old.id, old.file_path);
                     INSERT INTO media_fts (rowid, file_path) VALUES (new.id, new.file_path);
                 END;

                 -- Every one of these backs a WHERE or ORDER BY that the timeline,
                 -- trash, archive and AI workers run on every pass.
                 -- Not UNIQUE, deliberately. Nothing has ever stopped two rows sharing
                 -- a path, so a uniqueness constraint here would abort this migration
                 -- on exactly the libraries that have the problem, and an aborted
                 -- migration means an application that will not start. Deduplicating
                 -- media rows means deciding what happens to the album memberships,
                 -- faces and tags hanging off the losing row, which is its own change.
                 CREATE INDEX IF NOT EXISTS idx_media_file_path ON media(file_path);
                 CREATE INDEX IF NOT EXISTS idx_media_is_deleted ON media(is_deleted);
                 CREATE INDEX IF NOT EXISTS idx_media_is_archived ON media(is_archived);
                 CREATE INDEX IF NOT EXISTS idx_media_created_at ON media(created_at);
                 CREATE INDEX IF NOT EXISTS idx_media_date_taken ON media(date_taken);
                 CREATE INDEX IF NOT EXISTS idx_media_telegram_media_id ON media(telegram_media_id);
                 CREATE INDEX IF NOT EXISTS idx_media_scan_status ON media(scan_status);
                 CREATE INDEX IF NOT EXISTS idx_media_face_status ON media(face_status);
                 CREATE INDEX IF NOT EXISTS idx_media_clip_status ON media(clip_status);
                 CREATE INDEX IF NOT EXISTS idx_media_tags_status ON media(tags_status);
                 CREATE INDEX IF NOT EXISTS idx_album_media_media ON album_media(media_id);
                 CREATE INDEX IF NOT EXISTS idx_faces_media ON faces(media_id);

                 -- The queue was deduplicated by a SELECT COUNT before each INSERT,
                 -- which two workers can both pass before either writes. Existing
                 -- duplicates are collapsed to the oldest row before the constraint
                 -- goes on, or the index would fail to build.
                 DELETE FROM upload_queue WHERE id NOT IN (
                     SELECT MIN(id) FROM upload_queue GROUP BY file_path
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_upload_queue_file_path
                     ON upload_queue(file_path);

                 PRAGMA user_version = 20;
                 COMMIT;",
            )?;
            version = 20;
        }

        // Rows left in `uploading` by a process that died mid-transfer are invisible to
        // `get_next_pending_item` forever, so the file silently never uploads. Nothing
        // can legitimately be uploading at startup, before any worker has run.
        let requeued = conn.execute(
            "UPDATE upload_queue SET status = 'pending' WHERE status = 'uploading'",
            [],
        )?;
        if requeued > 0 {
            log::warn!(
                "Requeued {} upload(s) stranded in the uploading state by an earlier run",
                requeued
            );
        }

        Ok(())
    }
}
