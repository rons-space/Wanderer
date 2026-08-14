use img_hash::ImageHash;
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: i64,
    pub file_path: String,
    pub thumbnail_path: Option<String>,
    pub file_hash: Option<String>,
    pub telegram_media_id: Option<String>,
    pub mime_type: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration: Option<i32>,
    pub size_bytes: Option<i64>,
    pub created_at: i64,
    pub uploaded_at: Option<i64>,
    // New PRD fields
    pub date_taken: Option<String>, // EXIF date, then file mtime/ctime fallback
    pub latitude: Option<f64>,      // GPS coordinates
    pub longitude: Option<f64>,
    pub camera_make: Option<String>, // EXIF camera info
    pub camera_model: Option<String>,
    pub is_favorite: bool, // Heart icon
    pub rating: i32,       // 0-5 stars
    pub is_deleted: bool,  // Soft delete (trash)
    pub deleted_at: Option<i64>,
    pub is_archived: bool, // Archive (hidden from timeline)
    pub archived_at: Option<i64>,
    pub is_cloud_only: bool, // Local file removed, exists only on Telegram
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: i64,
    pub file_path: String,
    pub status: String,
    pub retries: i32,
    pub error_msg: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueueCounts {
    pub pending: i64,
    pub uploading: i64,
    pub failed: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchFilters {
    pub favorites_only: bool,
    pub min_rating: Option<i32>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub camera_make: Option<String>,
    pub has_location: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub cover_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SmartAlbumCounts {
    pub videos: i32,
    pub recent: i32,
    pub top_rated: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub media_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub face_count: i64,
    pub cover_path: Option<String>,
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn hamming_distance(hash1: &str, hash2: &str) -> u32 {
    let parsed_base64 = || -> Option<u32> {
        let h1: ImageHash = ImageHash::from_base64(hash1).ok()?;
        let h2: ImageHash = ImageHash::from_base64(hash2).ok()?;
        Some(h1.dist(&h2))
    };

    if let Some(distance) = parsed_base64() {
        return distance;
    }

    let parsed_hex = || -> Option<u32> {
        let h1 = u64::from_str_radix(hash1, 16).ok()?;
        let h2 = u64::from_str_radix(hash2, 16).ok()?;
        Some((h1 ^ h2).count_ones())
    };

    parsed_hex().unwrap_or(u32::MAX)
}

pub struct Database {
    conn: Mutex<Connection>,
    /// Directories this database is allowed to delete files from.
    ///
    /// `file_path` and `thumbnail_path` are just text columns. The delete paths
    /// used to unlink whatever they contained, so one bad row, one buggy
    /// importer or one crafted sync manifest turned "empty the trash" into
    /// "delete an arbitrary file". Derived from the database's own location, so
    /// it needs no plumbing from the caller.
    managed_roots: Vec<PathBuf>,
}

impl Database {
    /// Get a connection, recovering from poisoned mutex if needed.
    pub fn get_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| {
            // Recover from poisoned mutex - the previous holder panicked
            log::warn!("Recovering from poisoned database mutex");
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("Mutex poisoned: {}", e)),
            )
        })
    }

    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let app_data = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let conn = Connection::open(path)?;

        // Enable foreign keys
        conn.execute("PRAGMA foreign_keys = ON;", [])?;

        // Initialize/Migrate
        Self::migrate(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            managed_roots: crate::paths::managed_roots(&app_data),
        })
    }

    /// Unlink a path from a media row, refusing anything outside the managed
    /// directories. Returns false when the path was rejected or missing.
    fn delete_managed_file(&self, raw_path: &str) -> bool {
        let path = Path::new(raw_path);
        if !crate::paths::is_within_any(&self.managed_roots, path) {
            log::error!(
                "Refusing to delete a file outside the managed library: {}",
                raw_path
            );
            return false;
        }
        if !path.exists() {
            return false;
        }
        match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("Failed to delete {}: {}", raw_path, e);
                false
            }
        }
    }

    fn migrate(conn: &Connection) -> Result<()> {
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
            // version = 5;
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
        }

        // Migration 9: Add is_cloud_only column for Cloud-Only mode
        if version < 9 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE media ADD COLUMN is_cloud_only INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 9;
                 COMMIT;",
            )?;
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
            // Migration 15: Cleanup ghost persons (created during failed FK runs)
            conn.execute_batch(
                 "BEGIN;
                  DELETE FROM persons WHERE id NOT IN (SELECT DISTINCT person_id FROM faces WHERE person_id IS NOT NULL);
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
                let mut stmt = conn.prepare("PRAGMA table_info('tags')")?;
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

        Ok(())
    }

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

        if let Some(pid) = person_id {
            // DEBUG: Check existence
            let exists: bool = conn
                .query_row("SELECT 1 FROM persons WHERE id = ?1", [pid], |_| Ok(true))
                .unwrap_or(false);
            println!(
                "DEBUG: Person {} exists in 'persons' table? {}",
                pid, exists
            );

            // DEBUG: Check FK definition
            let mut stmt = conn.prepare("PRAGMA foreign_key_list('faces')")?;
            let fks = stmt.query_map([], |row| {
                Ok(format!(
                    "table={}, from={}, to={}",
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?
                ))
            })?;
            for fk in fks {
                println!("DEBUG FK: faces -> {}", fk.unwrap());
            }
        }

        match conn.execute(
            "UPDATE faces SET embedding = ?1, person_id = ?2 WHERE rowid = ?3",
            rusqlite::params![bytes, person_id, face_id],
        ) {
            Ok(_) => {}
            Err(e) => {
                println!("CRITICAL DB ERROR updating faces: {}", e);
                return Err(e.into());
            }
        }

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

        let mut stmt = conn.prepare(
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
            println!(
                "Face matched to Person {} (score: {:.3})",
                best_match.unwrap(),
                max_score
            );
            return Ok(best_match);
        }

        println!(
            "No match found (max_score: {:.3}). Creating new person.",
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

    pub fn get_persons(&self) -> Result<Vec<Person>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, 
                    (SELECT COUNT(DISTINCT f2.media_id) 
                     FROM faces f2 
                     JOIN media m2 ON f2.media_id = m2.id 
                     WHERE f2.person_id = p.id 
                       AND (m2.is_deleted = 0 OR m2.is_deleted IS NULL)) as face_count,
                    m.file_path -- cover path
             FROM persons p
             LEFT JOIN faces f ON p.cover_face_id = f.rowid
             LEFT JOIN media m ON f.media_id = m.id
             ORDER BY face_count DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(Person {
                id: row.get(0)?,
                name: row.get(1)?,
                face_count: row.get(2)?,
                cover_path: row.get(3)?,
            })
        })?;

        rows.collect()
    }

    // --- CLIP Operations ---

    pub fn store_clip_embedding(&self, media_id: i64, embedding: &[f32]) -> Result<()> {
        let mut conn = self.get_conn()?;

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
        let mut stmt = conn.prepare(
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
        let mut stmt =
            conn.prepare("SELECT id, clip_embedding FROM media WHERE clip_embedding IS NOT NULL")?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;

                // Convert bytes back to f32
                if bytes.len() % 4 != 0 {
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
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE (scan_status = 'pending' OR scan_status IS NULL) AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY created_at DESC 
             LIMIT 1"
        )?;

        stmt.query_row([], |row| {
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
        })
        .optional()
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
        let mut stmt =
            conn.prepare("SELECT x, y, width, height, score FROM faces WHERE media_id = ?1")?;

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

    // --- Media Operations ---

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

        // Also insert into FTS5 table for full-text search
        let _ = conn.execute("INSERT INTO media_fts (file_path) VALUES (?1)", [file_path]);

        Ok(media_id)
    }

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

    pub fn get_uploaded_unencrypted_media(
        &self,
        limit: i32,
    ) -> Result<Vec<(i64, String, String, Option<String>)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
        let mut stmt = conn.prepare(
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

    pub fn get_media(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE (is_deleted = 0 OR is_deleted IS NULL) AND (is_archived = 0 OR is_archived IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        )?;

        let media_iter = stmt.query_map([limit, offset], |row| {
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
        })?;

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
        let conn = self.get_conn()?;
        let placeholders = media_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, 
                    width, height, duration, size_bytes, created_at, uploaded_at, 
                    thumbnail_path, date_taken, latitude, longitude, 
                    camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media WHERE id IN ({}) AND is_deleted = 0",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::ToSql>> = media_ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
            .collect();
        let media_iter = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| {
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
            },
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
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE mime_type LIKE 'video/%' AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        )?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Get recent media (last 30 days)
    pub fn get_recent(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE created_at >= strftime('%s', 'now', '-30 days') AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        )?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Get top rated media (4+ stars)
    pub fn get_top_rated(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE rating >= 4 AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY rating DESC, COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        )?;
        let media_iter = stmt.query_map([limit, offset], Self::map_media_row)?;
        media_iter.collect()
    }

    /// Helper function to map a row to MediaItem
    fn map_media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaItem> {
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

    pub fn search_media(&self, query: &str, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        // Escape LIKE wildcards to prevent pattern injection
        let escaped = crate::media_utils::escape_like_pattern(query);
        let pattern = format!("%{}%", escaped);
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE (file_path LIKE ?1 OR mime_type LIKE ?1) AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?2 OFFSET ?3"
        )?;

        let media_iter = stmt.query_map(params![pattern, limit, offset], |row| {
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
        })?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Full-text search using FTS5 with optional filters
    pub fn search_fts(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<MediaItem>> {
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause based on filters
        let mut conditions = vec![
            "(is_deleted = 0 OR is_deleted IS NULL)".to_string(),
            "(is_archived = 0 OR is_archived IS NULL)".to_string(),
        ];

        if filters.favorites_only {
            conditions.push("is_favorite = 1".to_string());
        }

        if let Some(min_rating) = filters.min_rating {
            conditions.push(format!("rating >= {}", min_rating.max(0).min(5)));
        }

        if let Some(date_from) = filters.date_from {
            conditions.push(format!("created_at >= {}", date_from));
        }

        if let Some(date_to) = filters.date_to {
            conditions.push(format!("created_at <= {}", date_to));
        }

        if let Some(camera) = &filters.camera_make {
            if !camera.is_empty() {
                conditions.push(format!(
                    "camera_make LIKE '%{}%'",
                    camera.replace('\'', "''")
                ));
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
                "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                        date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
                 FROM media
                 WHERE {}
                 ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC
                 LIMIT ?1 OFFSET ?2",
                where_clause
            );

            let mut stmt = conn.prepare(&sql)?;
            let media_iter = stmt.query_map(params![limit, offset], |row| {
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
            })?;

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
            "SELECT m.id, m.file_path, m.file_hash, m.telegram_media_id, m.mime_type, m.width, m.height, m.duration, m.size_bytes, m.created_at, m.uploaded_at, m.thumbnail_path,
                    m.date_taken, m.latitude, m.longitude, m.camera_make, m.camera_model, m.is_favorite, m.rating, m.is_deleted, m.deleted_at, m.is_archived, m.archived_at, m.is_cloud_only
             FROM media m
             JOIN media_fts fts ON m.file_path = fts.file_path
             WHERE fts.media_fts MATCH ?1 AND {}
             ORDER BY rank, COALESCE(m.date_taken, datetime(m.created_at, 'unixepoch')) DESC
             LIMIT ?2 OFFSET ?3",
            where_clause
        );

        let mut stmt = conn.prepare(&sql)?;
        let media_iter = stmt.query_map(params![fts_query, limit, offset], |row| {
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
        })?;

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

    // --- Queue Operations ---

    pub fn add_to_queue(&self, file_path: &str) -> Result<()> {
        let conn = self.get_conn()?;

        // Check if already in queue (pending or uploading)
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM upload_queue WHERE file_path = ?1 AND status IN ('pending', 'uploading')",
            [file_path],
            |row| row.get(0),
        )?;

        if count > 0 {
            // Already queued, skip
            return Ok(());
        }

        let added_at = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "INSERT INTO upload_queue (file_path, status, added_at) VALUES (?1, 'pending', ?2)",
            (file_path, added_at),
        )?;
        Ok(())
    }

    pub fn get_next_pending_item(&self) -> Result<Option<QueueItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
        let mut stmt = conn.prepare(
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

    // --- Bulk Operations ---

    /// Set favorite status for multiple media items
    pub fn bulk_set_favorite(&self, media_ids: &[i64], is_favorite: bool) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
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
        let mut stmt = conn.prepare(
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
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.file_path, m.file_hash, m.telegram_media_id, m.mime_type, m.width, m.height, m.duration, m.size_bytes, m.created_at, m.uploaded_at, m.thumbnail_path,
                    m.date_taken, m.latitude, m.longitude, m.camera_make, m.camera_model, m.is_favorite, m.rating, m.is_deleted, m.deleted_at, m.is_archived, m.archived_at, m.is_cloud_only
             FROM media m
             INNER JOIN album_media am ON m.id = am.media_id
             WHERE am.album_id = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL) AND (m.is_archived = 0 OR m.is_archived IS NULL)
             ORDER BY am.added_at DESC
             LIMIT ?2 OFFSET ?3"
        )?;

        let media_iter = stmt.query_map(params![album_id, limit, offset], |row| {
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
        })?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    // --- Favorites & Ratings ---

    /// Toggle favorite status for a media item. Returns new favorite status.
    pub fn toggle_favorite(&self, media_id: i64) -> Result<bool> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET is_favorite = NOT COALESCE(is_favorite, 0) WHERE id = ?1",
            [media_id],
        )?;

        let is_favorite: i32 = conn.query_row(
            "SELECT COALESCE(is_favorite, 0) FROM media WHERE id = ?1",
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
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE is_favorite = 1 AND (is_deleted = 0 OR is_deleted IS NULL) AND (is_archived = 0 OR is_archived IS NULL)
             ORDER BY COALESCE(date_taken, datetime(created_at, 'unixepoch')) DESC 
             LIMIT ?1 OFFSET ?2"
        )?;

        let media_iter = stmt.query_map([limit, offset], |row| {
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
        })?;

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
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE is_deleted = 1
             ORDER BY deleted_at DESC 
             LIMIT ?1 OFFSET ?2"
        )?;

        let media_iter = stmt.query_map([limit, offset], |row| {
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
        })?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Permanently delete items that have been in trash for more than 30 days.
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

        // Both paths are confined to the managed directories before any unlink.
        if self.delete_managed_file(&file_path) {
            log::info!("Deleted local file: {}", file_path);
        }
        if let Some(ref thumb_path) = thumbnail_path {
            if self.delete_managed_file(thumb_path) {
                log::info!("Deleted thumbnail: {}", thumb_path);
            }
        }

        // Delete DB row
        conn.execute("DELETE FROM media WHERE id = ?1", [media_id])?;
        log::info!("Permanently deleted media id {} from database", media_id);

        Ok(telegram_media_id)
    }

    /// Permanently delete all items in trash.
    /// Returns count of deleted items and list of telegram_media_ids for optional Telegram deletion.
    pub fn empty_trash(&self) -> Result<(usize, Vec<String>)> {
        let mut conn = self.get_conn()?;

        // Get all trashed items
        let items: Vec<(i64, String, Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT id, file_path, thumbnail_path, telegram_media_id FROM media WHERE is_deleted = 1",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut telegram_ids = Vec::new();
        let mut deleted_count = 0;

        // Use a transaction for all deletions
        let tx = conn.transaction()?;

        for (id, file_path, thumbnail_path, telegram_media_id) in items {
            // Confined to the managed directories, like every other unlink.
            self.delete_managed_file(&file_path);
            if let Some(ref thumb_path) = thumbnail_path {
                self.delete_managed_file(thumb_path);
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

        log::info!("Emptied trash: {} items permanently deleted", deleted_count);
        Ok((deleted_count, telegram_ids))
    }

    // --- Duplicate Detection (FR-12) ---

    // --- Duplicate Detection (FR-12) ---

    /// Update the perceptual hash for a media item
    pub fn update_phash(&self, media_id: i64, phash: &str) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE media SET phash = ?1 WHERE id = ?2",
            (phash, media_id),
        )?;
        Ok(())
    }

    /// Get media items that don't have a phash computed yet
    /// Returns (id, file_path) pairs for images only (not videos)
    pub fn get_media_without_phash(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
        let mut stmt = conn.prepare(
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
    pub fn reconcile_cloud_only_flags(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path
             FROM media
             WHERE (is_deleted = 0 OR is_deleted IS NULL)
               AND telegram_media_id IS NOT NULL
               AND telegram_media_id != ''
               AND (is_cloud_only IS NULL OR is_cloud_only = 0)",
        )?;

        let candidates: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let mut updated = 0usize;
        for (media_id, file_path) in candidates {
            if !Path::new(&file_path).exists() {
                conn.execute(
                    "UPDATE media SET is_cloud_only = 1 WHERE id = ?1",
                    [media_id],
                )?;
                updated += 1;
            }
        }

        Ok(updated)
    }

    /// Get a single media item by ID.
    pub fn get_media_by_id(&self, media_id: i64) -> Result<Option<MediaItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media WHERE id = ?1"
        )?;

        stmt.query_row([media_id], |row| {
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
        })
        .optional()
    }

    /// Check if media with the given Telegram ID is marked as cloud-only.
    pub fn is_cloud_only_by_telegram_id(&self, telegram_id: &str) -> Result<bool> {
        let conn = self.get_conn()?;
        let mut stmt =
            conn.prepare("SELECT is_cloud_only FROM media WHERE telegram_media_id = ?1")?;

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
        let limit = limit.max(0).min(1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE is_archived = 1 AND (is_deleted = 0 OR is_deleted IS NULL)
             ORDER BY archived_at DESC 
             LIMIT ?1 OFFSET ?2"
        )?;

        let media_iter = stmt.query_map([limit, offset], |row| {
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
        })?;

        let mut media = Vec::new();
        for item in media_iter {
            media.push(item?);
        }
        Ok(media)
    }

    /// Find potential duplicates based on perceptual hash
    /// Returns groups of media items with similar pHash values.
    pub fn find_duplicates(&self) -> Result<Vec<Vec<MediaItem>>> {
        let conn = self.get_conn()?;
        const PHASH_DISTANCE_THRESHOLD: u32 = 10;

        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, 
                    duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, 
                    is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only, phash
             FROM media
             WHERE phash IS NOT NULL AND is_deleted = 0
             ORDER BY created_at ASC",
        )?;

        let candidates: Vec<(MediaItem, String)> = stmt
            .query_map([], |row| {
                Ok((
                    MediaItem {
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
                    },
                    row.get(24)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let n = candidates.len();
        if n < 2 {
            return Ok(Vec::new());
        }

        let mut parent: Vec<usize> = (0..n).collect();
        let mut rank = vec![0usize; n];

        fn find(parent: &mut [usize], x: usize) -> usize {
            if parent[x] != x {
                let root = find(parent, parent[x]);
                parent[x] = root;
            }
            parent[x]
        }

        fn union(parent: &mut [usize], rank: &mut [usize], a: usize, b: usize) {
            let ra = find(parent, a);
            let rb = find(parent, b);
            if ra == rb {
                return;
            }
            if rank[ra] < rank[rb] {
                parent[ra] = rb;
            } else if rank[ra] > rank[rb] {
                parent[rb] = ra;
            } else {
                parent[rb] = ra;
                rank[ra] += 1;
            }
        }

        for i in 0..n {
            for j in (i + 1)..n {
                let distance = hamming_distance(&candidates[i].1, &candidates[j].1);
                if distance <= PHASH_DISTANCE_THRESHOLD {
                    union(&mut parent, &mut rank, i, j);
                }
            }
        }

        let mut grouped: std::collections::HashMap<usize, Vec<MediaItem>> =
            std::collections::HashMap::new();

        for idx in 0..n {
            let root = find(&mut parent, idx);
            grouped
                .entry(root)
                .or_default()
                .push(candidates[idx].0.clone());
        }

        let mut groups: Vec<Vec<MediaItem>> = grouped
            .into_values()
            .filter(|items| items.len() > 1)
            .collect();

        for group in &mut groups {
            group.sort_by_key(|item| item.created_at);
        }

        groups.sort_by(|a, b| b.len().cmp(&a.len()));
        Ok(groups)
    }

    // --- People / Face Recognition (FR-6) ---

    /// Get all people with face counts
    /// Get all people with face counts
    pub fn get_people(&self) -> Result<Vec<Person>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT m.id, m.file_path, m.file_hash, m.telegram_media_id, m.mime_type, 
                    m.width, m.height, m.duration, m.size_bytes, m.created_at, m.uploaded_at, 
                    m.thumbnail_path, m.date_taken, m.latitude, m.longitude, m.camera_make, 
                    m.camera_model, m.is_favorite, m.rating, m.is_deleted, m.deleted_at, m.is_archived, m.archived_at, m.is_cloud_only
             FROM media m
             JOIN faces f ON f.media_id = m.id
             WHERE f.person_id = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL) AND (m.is_archived = 0 OR m.is_archived IS NULL)
             ORDER BY m.created_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;

        let items = stmt.query_map((person_id, limit, offset), |row| {
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
        })?;

        let mut result = Vec::new();
        for item in items {
            result.push(item?);
        }
        Ok(result)
    }
}

impl Database {
    // --- Config Operations (Settings) ---

    /// Get a config value by key
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        let conn = self.get_conn()?;
        let result: rusqlite::Result<String> =
            conn.query_row("SELECT value FROM config WHERE key = ?1", [key], |row| {
                row.get(0)
            });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a config value
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value, updated_at) VALUES (?1, ?2, ?3)",
            (key, value, now),
        )?;
        Ok(())
    }

    /// Delete a config key
    pub fn remove_config(&self, key: &str) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute("DELETE FROM config WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Get all config values as key-value pairs
    pub fn get_all_config(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare("SELECT key, value FROM config")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut config = std::collections::HashMap::new();
        for row in rows {
            let (key, value) = row?;
            config.insert(key, value);
        }
        Ok(config)
    }
}

impl Database {
    // --- Sync Helper Methods ---

    /// Get all media items with their sync-relevant fields (for export)
    pub fn get_all_media_for_sync(&self) -> Result<Vec<MediaItem>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media 
             WHERE (is_deleted = 0 OR is_deleted IS NULL)"
        )?;

        let items: Vec<MediaItem> = stmt
            .query_map([], |row| {
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
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(items)
    }

    /// Get albums that a specific media item belongs to
    pub fn get_albums_for_media(&self, media_id: i64) -> Result<Vec<Album>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, a.created_at, \
                    (SELECT m.thumbnail_path FROM album_media am2 \
                     JOIN media m ON am2.media_id = m.id \
                     WHERE am2.album_id = a.id \
                       AND (m.is_deleted = 0 OR m.is_deleted IS NULL) \
                       AND (m.is_archived = 0 OR m.is_archived IS NULL) \
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_thumbnail, \
                    (SELECT m.file_path FROM album_media am2 \
                     JOIN media m ON am2.media_id = m.id \
                     WHERE am2.album_id = a.id \
                       AND (m.is_deleted = 0 OR m.is_deleted IS NULL) \
                       AND (m.is_archived = 0 OR m.is_archived IS NULL) \
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_file_path \
             FROM albums a \
             INNER JOIN album_media am ON a.id = am.album_id \
             WHERE am.media_id = ?1",
        )?;

        let albums: Vec<Album> = stmt
            .query_map([media_id], |row| {
                let thumbnail_path: Option<String> = row.get(3)?;
                let file_path: Option<String> = row.get(4)?;
                let cover = thumbnail_path.or(file_path);

                Ok(Album {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    cover_path: cover,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(albums)
    }

    /// Get a media item by its blake3 hash
    pub fn get_media_by_hash(&self, hash: &str) -> Result<Option<MediaItem>> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                    date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
             FROM media WHERE file_hash = ?1",
            [hash],
            |row| {
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
            },
        );

        match result {
            Ok(item) => Ok(Some(item)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
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
            Err(e) => Err(e.into()),
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

    // --- Tag Operations ---

    pub fn get_all_tags(&self) -> Result<Vec<Tag>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.file_path, m.file_hash, m.telegram_media_id, m.mime_type, m.width, m.height, m.duration, m.size_bytes, m.created_at, m.uploaded_at, m.thumbnail_path,
                    m.date_taken, m.latitude, m.longitude, m.camera_make, m.camera_model, m.is_favorite, m.rating, m.is_deleted, m.deleted_at, m.is_archived, m.archived_at, m.is_cloud_only
             FROM media m
             JOIN media_tags mt ON m.id = mt.media_id
             JOIN tags t ON mt.tag_id = t.id
             WHERE t.name = ?1 AND (m.is_deleted = 0 OR m.is_deleted IS NULL)
             ORDER BY m.created_at DESC
             LIMIT ?2 OFFSET ?3"
         )?;

        let media_iter = stmt.query_map(params![tag_name, limit, offset], |row| {
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
        })?;

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

    pub fn reset_stuck_scans(&self) -> Result<usize> {
        let conn = self.get_conn()?;

        // Find media_ids that have faces with NULL embedding (incomplete processing)
        let mut stmt =
            conn.prepare("SELECT DISTINCT media_id FROM faces WHERE embedding IS NULL")?;

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

    pub fn reset_all_scans(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        // Reset ALL scan status
        let count = conn.execute("UPDATE media SET scan_status = 'pending'", [])?;
        log::info!("Forced reset of {} media items to pending state", count);
        Ok(count)
    }

    // Original broken function signature was here:

    pub fn get_tags_for_media(&self, media_id: i64) -> Result<Vec<String>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
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
