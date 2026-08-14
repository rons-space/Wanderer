use img_hash::ImageHash;
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use time::OffsetDateTime;

/// Largest page any paginated read will return.
///
/// The limit and offset on these methods come from the frontend, where a negative
/// limit means "no limit" to SQLite and a negative offset is an error. Clamping here
/// rather than at each call site means a caller that forgets cannot ask the database
/// to materialize an entire library into memory.
const MAX_PAGE_SIZE: i32 = 1000;

/// Most values bound into one statement built from a variable-length list.
///
/// SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 32766 on current builds and 999 on
/// older ones. Selecting or updating every item in a large library would exceed that
/// and fail the whole call, so those queries are chunked well under the smaller limit.
const MAX_SQL_VARIABLES: usize = 500;

/// Escape a value for use in a `LIKE` pattern.
///
/// The escape character has to be declared by the query with `ESCAPE '\\'`; without
/// that clause SQLite treats the backslashes as literal text and the pattern silently
/// stops matching. Unescaped, a `%` typed by the user turns their filter into a
/// wildcard, which is a correctness bug rather than an injection one now that the
/// value is bound.
fn escape_like_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

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

/// One row of `get_uploaded_unencrypted_media`: media id, local path, Telegram media id
/// and the thumbnail path where the item has one.
pub type UnencryptedUpload = (i64, String, String, Option<String>);

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
    /// Get a connection, stepping over a poisoned mutex.
    ///
    /// The comment here used to say "recovering", but the code turned poisoning into an
    /// error, and since a `Mutex` stays poisoned forever, one panic anywhere under this
    /// lock disabled every database method for the rest of the process. The application
    /// then looked broken in a way no restart-free action could fix.
    ///
    /// Poisoning only means a previous holder panicked while holding the guard. SQLite
    /// is unharmed: any transaction that was open is rolled back when its statement is
    /// dropped during the unwind. So take the connection back and carry on, which is
    /// what `into_inner` is for.
    pub fn get_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        Ok(self.conn.lock().unwrap_or_else(|poisoned| {
            log::warn!(
                "Database mutex was poisoned by a panic in an earlier call; continuing \
                 with the connection"
            );
            poisoned.into_inner()
        }))
    }

    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let app_data = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let conn = Connection::open(path)?;

        // Set before the migration chain runs, so the migrations themselves get the
        // same durability and locking behaviour as everything after them.
        //
        // WAL: readers stop blocking the writer, and a crash mid-write leaves the
        // last committed state rather than a torn page. `synchronous = NORMAL` is the
        // pairing WAL is designed for: fsync at checkpoints instead of every commit,
        // which trades a crash-during-checkpoint window for an order of magnitude on
        // write throughput, and this application commits per imported file.
        //
        // busy_timeout: the AI worker, the upload worker, the watcher and the sync
        // worker all reach this connection. Without a timeout, any contention is an
        // immediate SQLITE_BUSY error surfaced to the user rather than a short wait.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;

        // Initialize/Migrate
        Self::migrate(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            managed_roots: crate::paths::managed_roots(&app_data),
        })
    }

    /// Write a consistent snapshot of the database to `dest`.
    ///
    /// `VACUUM INTO` rather than a file copy. Copying `library.db` was always a race
    /// against whatever transaction happened to be open, and with WAL enabled it is
    /// worse than that: the most recent commits live in `library.db-wal` until a
    /// checkpoint, so a copy of the main file alone can be missing data the user was
    /// just told is backed up. This takes a read lock, writes a defragmented snapshot,
    /// and produces a file that is a valid database on its own.
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        let conn = self.get_conn()?;
        // VACUUM INTO refuses to overwrite an existing file, which is the behaviour we
        // want: callers name their own timestamped file, so a collision means something
        // is wrong and silently clobbering a previous backup would be the worst answer.
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
        Ok(())
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

    // Every step in the chain closes with `version = N;` so the next one can be appended
    // without reading the one above it. That makes the final assignment dead by
    // construction, and deleting it would leave the last step shaped differently from
    // all the others and the next author with a silently skipped migration.
    #[allow(unused_assignments)]
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

    // Superseded by `get_people`; `search_media` is the only caller of the broken
    // `escape_like_pattern`. Both go in T55 (issue #63), which owns their removal.
    #[allow(dead_code)]
    pub fn get_persons(&self) -> Result<Vec<Person>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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

    pub fn get_media(&self, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
            "SELECT id, file_path, file_hash, telegram_media_id, mime_type, 
                    width, height, duration, size_bytes, created_at, uploaded_at, 
                    thumbnail_path, date_taken, latitude, longitude, 
                    camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
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
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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

    // Unreachable: FTS5 search replaced it. Removed in T55 (issue #63) together with
    // `escape_like_pattern`, whose only caller it is.
    #[allow(dead_code)]
    pub fn search_media(&self, query: &str, limit: i32, offset: i32) -> Result<Vec<MediaItem>> {
        // Validate and clamp pagination parameters
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        // Escape LIKE wildcards to prevent pattern injection
        let escaped = crate::media_utils::escape_like_pattern(query);
        let pattern = format!("%{}%", escaped);
        let mut stmt = conn.prepare_cached(
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
                "SELECT id, file_path, file_hash, telegram_media_id, mime_type, width, height, duration, size_bytes, created_at, uploaded_at, thumbnail_path,
                        date_taken, latitude, longitude, camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, is_cloud_only
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
        let limit = limit.clamp(0, 1000);
        let offset = offset.max(0);

        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
    pub fn reconcile_cloud_only_flags(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let mut stmt = conn.prepare_cached(
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
        let mut stmt = conn.prepare_cached(
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

        let mut stmt = conn.prepare_cached(
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

        for (idx, candidate) in candidates.iter().enumerate() {
            let root = find(&mut parent, idx);
            grouped.entry(root).or_default().push(candidate.0.clone());
        }

        let mut groups: Vec<Vec<MediaItem>> = grouped
            .into_values()
            .filter(|items| items.len() > 1)
            .collect();

        for group in &mut groups {
            group.sort_by_key(|item| item.created_at);
        }

        groups.sort_by_key(|group| std::cmp::Reverse(group.len()));
        Ok(groups)
    }

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
            Err(e) => Err(e),
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
        let mut stmt = conn.prepare_cached("SELECT key, value FROM config")?;
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
        let mut stmt = conn.prepare_cached(
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
        let mut stmt = conn.prepare_cached(
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
        let mut stmt = conn.prepare_cached(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema version the chain is expected to reach. Update this together with a
    /// new migration, which is the point: forgetting makes the test fail loudly here
    /// rather than quietly at a user's next startup.
    const CURRENT_SCHEMA_VERSION: i32 = 20;

    fn migrated() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute("PRAGMA foreign_keys = ON;", []).unwrap();
        Database::migrate(&conn).expect("migration chain should run on an empty database");
        conn
    }

    fn user_version(conn: &Connection) -> i32 {
        conn.query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap()
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        names
    }

    #[test]
    fn migrating_from_empty_reaches_the_current_version() {
        let conn = migrated();
        assert_eq!(user_version(&conn), CURRENT_SCHEMA_VERSION);
    }

    /// Every block assigns the local `version` after its `PRAGMA user_version`. When
    /// they disagree, the chain still happens to work in one direction and breaks the
    /// next time a migration is inserted, so assert they agree rather than waiting.
    #[test]
    fn running_the_chain_twice_is_a_no_op() {
        let conn = migrated();
        let before = table_names(&conn);
        Database::migrate(&conn).expect("re-running the chain should be a no-op");
        assert_eq!(user_version(&conn), CURRENT_SCHEMA_VERSION);
        assert_eq!(before, table_names(&conn));
    }

    /// The bug this guards against: a migration block sets `PRAGMA user_version` in
    /// SQL but forgets to assign the local `version` in Rust. Eight blocks had drifted
    /// that way. Nothing breaks immediately, because the blocks run in order and each
    /// one only widens the schema, so the omission sits latent until someone inserts a
    /// migration whose guard then reads a stale version and skips or repeats work.
    ///
    /// Reading the source is unusual for a test, but the alternative is a runtime
    /// assertion that cannot fire until the damage is already possible.
    #[test]
    fn every_migration_block_updates_the_local_version() {
        let source = include_str!("database.rs");
        for n in 1..=CURRENT_SCHEMA_VERSION {
            assert!(
                source.contains(&format!("PRAGMA user_version = {};", n)),
                "migration {} does not set PRAGMA user_version",
                n
            );
            // Matched as a whole line, because `contains` on "version = 12;" also
            // matches the "PRAGMA user_version = 12;" two lines above it, which would
            // make this assertion pass for exactly the code it exists to reject.
            let assignment = format!("version = {};", n);
            assert!(
                source
                    .lines()
                    .any(|line| line.trim().starts_with(&assignment)),
                "migration {} sets PRAGMA user_version but never assigns the local \
                 `version`, so a later migration guard will read a stale value",
                n
            );
        }
    }

    #[test]
    fn the_expected_tables_exist() {
        let conn = migrated();
        let tables = table_names(&conn);
        for required in [
            "media",
            "albums",
            "album_media",
            "upload_queue",
            "tags",
            "media_tags",
            "faces",
            "persons",
            "config",
        ] {
            assert!(
                tables.iter().any(|t| t == required),
                "missing table {}: {:?}",
                required,
                tables
            );
        }
    }

    /// Migration 15 deletes persons with no faces pointing at them. `NOT IN` against an
    /// empty subquery matches every row, so without its guard this wipes every named
    /// person on a library where face detection has not run, which is the normal state
    /// for most users.
    #[test]
    fn ghost_person_cleanup_keeps_people_when_no_face_is_assigned() {
        let conn = migrated();
        conn.execute("INSERT INTO persons (name) VALUES ('Ana')", [])
            .unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Bo')", [])
            .unwrap();

        // Re-run the cleanup exactly as migration 15 does.
        conn.execute_batch(
            "DELETE FROM persons
             WHERE EXISTS (SELECT 1 FROM faces WHERE person_id IS NOT NULL)
               AND id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);",
        )
        .unwrap();

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            remaining, 2,
            "named people were deleted with no faces present"
        );
    }

    /// The other half of the same guard: once assignments exist, the cleanup still
    /// removes the persons that nothing points at.
    #[test]
    fn ghost_person_cleanup_still_removes_unreferenced_people() {
        let conn = migrated();
        conn.execute(
            "INSERT INTO media (file_path, created_at) VALUES ('/tmp/a.jpg', 0)",
            [],
        )
        .unwrap();
        let media_id: i64 = conn.last_insert_rowid();
        conn.execute("INSERT INTO persons (name) VALUES ('Ana')", [])
            .unwrap();
        let ana: i64 = conn.last_insert_rowid();
        conn.execute("INSERT INTO persons (name) VALUES ('ghost')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO faces (media_id, x, y, width, height, score, person_id) VALUES (?, 0, 0, 1, 1, 1.0, ?)",
            rusqlite::params![media_id, ana],
        )
        .unwrap();

        conn.execute_batch(
            "DELETE FROM persons
             WHERE EXISTS (SELECT 1 FROM faces WHERE person_id IS NOT NULL)
               AND id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);",
        )
        .unwrap();

        let names: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM persons ORDER BY name")
                .unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(names, vec!["Ana".to_string()]);
    }

    fn insert_media(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO media (file_path, created_at) VALUES (?1, 0)",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Insert a row with a camera, for the filter tests.
    fn insert_media_with_camera(db: &Database, path: &str, camera: &str) -> i64 {
        let conn = db.get_conn().unwrap();
        conn.execute(
            "INSERT INTO media (file_path, created_at, camera_make) VALUES (?1, 0, ?2)",
            rusqlite::params![path, camera],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn camera_filter(camera: &str) -> SearchFilters {
        SearchFilters {
            favorites_only: false,
            min_rating: None,
            date_from: None,
            date_to: None,
            camera_make: Some(camera.to_string()),
            has_location: None,
        }
    }

    #[test]
    fn like_escaping_covers_the_three_special_characters() {
        assert_eq!(escape_like_value("Canon"), "Canon");
        assert_eq!(escape_like_value("100%"), "100\\%");
        assert_eq!(escape_like_value("a_b"), "a\\_b");
        // The backslash first, or escaping the others would double-escape it.
        assert_eq!(escape_like_value("a\\b"), "a\\\\b");
    }

    /// The filter value is user input reaching a `WHERE` clause. It used to be
    /// interpolated with doubled quotes; anything the escaping missed was query text.
    #[test]
    fn a_camera_filter_is_bound_rather_than_interpolated() {
        let temp = TempDb::new();
        insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");
        insert_media_with_camera(&temp.db, "/library/b.jpg", "Nikon");

        let hostile = "Canon' OR 1=1 --";
        let found = temp
            .db
            .search_fts("", &camera_filter(hostile), 100, 0)
            .unwrap();
        assert!(
            found.is_empty(),
            "a quote in the filter must be a value, not syntax: {:?}",
            found.iter().map(|m| &m.file_path).collect::<Vec<_>>()
        );

        let found = temp
            .db
            .search_fts("", &camera_filter("Canon"), 100, 0)
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_path, "/library/a.jpg");
    }

    /// `%` in a `LIKE` pattern is a wildcard, so an unescaped one silently widens the
    /// user's filter instead of matching what they typed.
    #[test]
    fn a_wildcard_in_a_camera_filter_matches_literally() {
        let temp = TempDb::new();
        insert_media_with_camera(&temp.db, "/library/literal.jpg", "C%N");
        insert_media_with_camera(&temp.db, "/library/canon.jpg", "CANON");

        let found = temp
            .db
            .search_fts("", &camera_filter("C%N"), 100, 0)
            .unwrap();
        assert_eq!(
            found
                .iter()
                .map(|m| m.file_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/library/literal.jpg"]
        );
    }

    /// Both come straight from the frontend. A negative limit means "no limit" to
    /// SQLite, and a negative offset is an error rather than a page.
    #[test]
    fn paginated_reads_clamp_their_limit_and_offset() {
        let temp = TempDb::new();
        insert_media_with_camera(&temp.db, "/library/a.jpg", "Canon");

        assert!(temp.db.get_media_by_person(1, -1, -5).is_ok());
        assert!(temp.db.get_media_by_tag("holiday", -1, -5).is_ok());
        assert!(temp
            .db
            .search_fts("", &camera_filter("Canon"), -1, -5)
            .is_ok());
    }

    /// More ids than SQLite will accept as bound variables in one statement.
    #[test]
    fn reads_and_bulk_writes_chunk_long_id_lists() {
        let temp = TempDb::new();
        let ids: Vec<i64> = (0..(MAX_SQL_VARIABLES * 2 + 1))
            .map(|i| insert_media_with_camera(&temp.db, &format!("/library/{}.jpg", i), "Canon"))
            .collect();

        let found = temp.db.get_media_by_ids(&ids).unwrap();
        assert_eq!(found.len(), ids.len());

        assert_eq!(temp.db.bulk_set_favorite(&ids, true).unwrap(), ids.len());
        assert_eq!(temp.db.bulk_soft_delete(&ids).unwrap(), ids.len());
    }

    fn fts_matches(conn: &Connection, query: &str) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT rowid FROM media_fts WHERE media_fts MATCH ?1 ORDER BY rowid")
            .unwrap();
        stmt.query_map([query], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    /// The whole point of migration 20: the index is no longer something the import
    /// path remembers to write. An insert through any code path indexes the row, an
    /// update re-indexes it, and a delete removes it.
    #[test]
    fn the_search_index_is_maintained_by_triggers() {
        let conn = migrated();

        // Inserted with plain SQL, deliberately: no call to `add_media`, because the
        // regression being guarded is precisely rows that arrive by some other path.
        let vacation = insert_media(&conn, "/library/vacation.jpg");
        insert_media(&conn, "/library/invoices.pdf");

        assert_eq!(fts_matches(&conn, "vacation"), vec![vacation]);

        conn.execute(
            "UPDATE media SET file_path = '/library/holiday.jpg' WHERE id = ?1",
            [vacation],
        )
        .unwrap();
        assert!(
            fts_matches(&conn, "vacation").is_empty(),
            "the old term still matches after a rename"
        );
        assert_eq!(fts_matches(&conn, "holiday"), vec![vacation]);

        conn.execute("DELETE FROM media WHERE id = ?1", [vacation])
            .unwrap();
        assert!(
            fts_matches(&conn, "holiday").is_empty(),
            "a deleted row is still in the search index"
        );
        assert_eq!(fts_matches(&conn, "invoices").len(), 1);
    }

    /// External-content FTS5 stores no copy of the text, so a mismatch between the
    /// index and `media` is corruption rather than staleness. `integrity-check`
    /// is the only thing that detects it.
    #[test]
    fn the_search_index_passes_its_own_integrity_check() {
        let conn = migrated();
        insert_media(&conn, "/library/vacation.jpg");
        conn.execute(
            "INSERT INTO media_fts (media_fts) VALUES ('integrity-check')",
            [],
        )
        .expect("fts5 integrity-check failed: the index disagrees with the media table");
    }

    #[test]
    fn the_upload_queue_rejects_duplicate_paths() {
        let conn = migrated();
        conn.execute(
            "INSERT INTO upload_queue (file_path, added_at) VALUES ('/library/a.jpg', 0)",
            [],
        )
        .unwrap();
        let second = conn.execute(
            "INSERT INTO upload_queue (file_path, added_at) VALUES ('/library/a.jpg', 0)",
            [],
        );
        assert!(
            second.is_err(),
            "the unique index did not stop a duplicate queue entry"
        );
    }

    /// Migration 20 has to collapse duplicates before it can add the constraint, or
    /// the index fails to build and every existing library that queued the same file
    /// twice under the old count-then-insert dedupe refuses to start.
    ///
    /// Run against a library put back into the pre-constraint state, rather than a
    /// hand-built schema, so the statements under test are the migration's own.
    #[test]
    fn collapsing_duplicate_queue_rows_keeps_the_oldest() {
        let conn = migrated();
        conn.execute_batch(
            "DROP INDEX idx_upload_queue_file_path;
             INSERT INTO upload_queue (file_path, status, added_at)
                 VALUES ('/library/a.jpg', 'failed', 100),
                        ('/library/a.jpg', 'pending', 200),
                        ('/library/b.jpg', 'pending', 300);",
        )
        .unwrap();

        conn.execute_batch(
            "DELETE FROM upload_queue WHERE id NOT IN (
                 SELECT MIN(id) FROM upload_queue GROUP BY file_path
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_upload_queue_file_path
                 ON upload_queue(file_path);",
        )
        .expect("the constraint should build once duplicates are collapsed");

        let rows: Vec<(String, String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT file_path, status, added_at FROM upload_queue ORDER BY file_path")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                ("/library/a.jpg".to_string(), "failed".to_string(), 100),
                ("/library/b.jpg".to_string(), "pending".to_string(), 300),
            ]
        );
    }

    /// A row abandoned in `uploading` by a process that died is invisible to
    /// `get_next_pending_item`, so the file never uploads and never reports an error.
    #[test]
    fn startup_requeues_uploads_stranded_by_a_previous_run() {
        let conn = migrated();
        conn.execute_batch(
            "INSERT INTO upload_queue (file_path, status, added_at)
                 VALUES ('/library/a.jpg', 'uploading', 0),
                        ('/library/b.jpg', 'completed', 0),
                        ('/library/c.jpg', 'failed', 0);",
        )
        .unwrap();

        Database::migrate(&conn).unwrap();

        let status_of = |path: &str| -> String {
            conn.query_row(
                "SELECT status FROM upload_queue WHERE file_path = ?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(status_of("/library/a.jpg"), "pending");
        assert_eq!(status_of("/library/b.jpg"), "completed");
        assert_eq!(status_of("/library/c.jpg"), "failed");
    }

    /// Render the schema of a migrated database as deterministic SQL.
    ///
    /// Ordered by kind and then name rather than by `sqlite_master` order, because the
    /// latter is the order the migrations happened to run in and would reshuffle the
    /// whole file every time a column is added. Shadow tables that fts5 maintains for
    /// itself are excluded: they are an implementation detail of the SQLite build, not
    /// of this schema, and pinning them would turn a library upgrade into a failing
    /// test.
    fn schema_snapshot(conn: &Connection) -> String {
        let mut stmt = conn
            .prepare(
                "SELECT type, name, sql FROM sqlite_master
                 WHERE sql IS NOT NULL
                   AND name NOT LIKE 'sqlite_%'
                   AND NOT (type = 'table' AND name LIKE 'media_fts_%')
                 ORDER BY
                     CASE type
                         WHEN 'table' THEN 0
                         WHEN 'index' THEN 1
                         WHEN 'trigger' THEN 2
                         WHEN 'view' THEN 3
                         ELSE 4
                     END,
                     name",
            )
            .unwrap();
        let entries = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        let mut out = String::new();
        out.push_str(
            "-- Generated from the migration chain in src/database.rs. Do not edit by hand.\n\
             -- Refresh with: WANDERER_BLESS_SCHEMA=1 cargo test schema\n",
        );
        out.push_str(&format!(
            "-- PRAGMA user_version = {};\n",
            user_version(conn)
        ));
        for (_, _, sql) in entries {
            out.push('\n');
            out.push_str(&dedent(sql.trim()));
            out.push_str(";\n");
        }
        out
    }

    /// SQLite stores the original text of a statement, so every definition here comes
    /// back indented to wherever it sat inside a Rust string literal. Strip that shared
    /// prefix, or the snapshot reads as a ragged quotation of `database.rs` instead of
    /// as a schema.
    fn dedent(sql: &str) -> String {
        let common = sql
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        sql.lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 || line.trim().is_empty() {
                    line.trim_end().to_string()
                } else {
                    line[common..].trim_end().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #43: the schema existed only as ~500 lines of string literals spread across
    /// twenty migration blocks, so nothing could be reviewed or diffed. This pins a
    /// readable snapshot and fails when the chain and the snapshot disagree, which
    /// also makes every schema change visible in the diff of the pull request that
    /// makes it.
    #[test]
    fn the_committed_schema_matches_the_migration_chain() {
        let conn = migrated();
        let actual = schema_snapshot(&conn);
        let committed = include_str!("../schema.sql");
        if actual == committed {
            return;
        }
        if std::env::var_os("WANDERER_BLESS_SCHEMA").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema.sql");
            std::fs::write(&path, &actual).expect("write the refreshed schema snapshot");
            return;
        }
        panic!(
            "src-tauri/schema.sql is out of date with the migration chain.\n\
             Refresh it with: WANDERER_BLESS_SCHEMA=1 cargo test schema\n\
             and commit the result with the migration that changed it."
        );
    }

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A file-backed database, for the handful of tests that exercise `Database`
    /// itself rather than the schema. Dropped with the directory.
    struct TempDb {
        dir: PathBuf,
        db: Database,
    }

    impl TempDb {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("wanderer-db-test-{pid}-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            let db = Database::new(dir.join("library.db")).expect("open the test database");
            Self { dir, db }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn queue_rows(db: &Database) -> Vec<(String, String, i64)> {
        let conn = db.get_conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT file_path, status, retries FROM upload_queue ORDER BY file_path")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// The upsert replaced a count-then-insert, and it has to preserve what the count
    /// did: queueing a path that is already waiting or in flight changes nothing.
    #[test]
    fn queueing_a_file_twice_leaves_the_first_entry_alone() {
        let tmp = TempDb::new();
        tmp.db.add_to_queue("/library/a.jpg").unwrap();
        tmp.db.add_to_queue("/library/a.jpg").unwrap();
        assert_eq!(
            queue_rows(&tmp.db),
            vec![("/library/a.jpg".to_string(), "pending".to_string(), 0)]
        );

        {
            let conn = tmp.db.get_conn().unwrap();
            conn.execute(
                "UPDATE upload_queue SET status = 'uploading' WHERE file_path = '/library/a.jpg'",
                [],
            )
            .unwrap();
        }
        tmp.db.add_to_queue("/library/a.jpg").unwrap();
        assert_eq!(
            queue_rows(&tmp.db),
            vec![("/library/a.jpg".to_string(), "uploading".to_string(), 0)],
            "an upload in flight was reset out from under the worker"
        );
    }

    /// The other half: a path that previously failed or completed is a genuine
    /// re-queue, and must go back to pending with its retry count cleared.
    #[test]
    fn queueing_a_failed_file_again_resets_it_to_pending() {
        let tmp = TempDb::new();
        tmp.db.add_to_queue("/library/a.jpg").unwrap();
        {
            let conn = tmp.db.get_conn().unwrap();
            conn.execute(
                "UPDATE upload_queue SET status = 'failed', retries = 3, error_msg = 'boom'
                 WHERE file_path = '/library/a.jpg'",
                [],
            )
            .unwrap();
        }

        tmp.db.add_to_queue("/library/a.jpg").unwrap();

        assert_eq!(
            queue_rows(&tmp.db),
            vec![("/library/a.jpg".to_string(), "pending".to_string(), 0)]
        );
        let error_msg: Option<String> = {
            let conn = tmp.db.get_conn().unwrap();
            conn.query_row(
                "SELECT error_msg FROM upload_queue WHERE file_path = '/library/a.jpg'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(error_msg, None, "the stale failure message survived");
    }

    /// The backup has to be a database, not a byte copy taken mid-write. With WAL on,
    /// the rows below are in the log and not yet in library.db, so a file copy of the
    /// main file would produce an empty library: exactly the failure being fixed.
    #[test]
    fn a_backup_is_a_complete_snapshot_of_the_live_database() {
        let tmp = TempDb::new();
        {
            let conn = tmp.db.get_conn().unwrap();
            insert_media(&conn, "/library/vacation.jpg");
            insert_media(&conn, "/library/invoices.pdf");
        }

        let dest = tmp.dir.join("backup.db");
        tmp.db.backup_to(&dest).expect("write the snapshot");

        let restored = Connection::open(&dest).unwrap();
        let paths: Vec<String> = {
            let mut stmt = restored
                .prepare("SELECT file_path FROM media ORDER BY file_path")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            paths,
            vec![
                "/library/invoices.pdf".to_string(),
                "/library/vacation.jpg".to_string()
            ]
        );
        assert_eq!(user_version(&restored), CURRENT_SCHEMA_VERSION);
        assert_eq!(fts_matches(&restored, "vacation").len(), 1);
    }

    /// Overwriting a backup silently would be worse than failing, so `VACUUM INTO`
    /// refusing an existing target is load-bearing rather than incidental.
    #[test]
    fn a_backup_refuses_to_overwrite_an_existing_file() {
        let tmp = TempDb::new();
        let dest = tmp.dir.join("backup.db");
        std::fs::write(&dest, b"not a database").unwrap();

        assert!(tmp.db.backup_to(&dest).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"not a database");
    }
}
