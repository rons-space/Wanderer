-- Generated from the migration chain in src/database.rs. Do not edit by hand.
-- Refresh with: WANDERER_BLESS_SCHEMA=1 cargo test schema
-- PRAGMA user_version = 22;

CREATE TABLE album_media (
    album_id INTEGER NOT NULL,
    media_id INTEGER NOT NULL,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (album_id, media_id),
    FOREIGN KEY(album_id) REFERENCES albums(id) ON DELETE CASCADE,
    FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE
);

CREATE TABLE albums (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE config (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE "faces" (
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

CREATE TABLE media (
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
, thumbnail_path TEXT, scan_status TEXT DEFAULT 'pending', date_taken TEXT, latitude REAL, longitude REAL, camera_make TEXT, camera_model TEXT, is_favorite INTEGER DEFAULT 0, rating INTEGER DEFAULT 0, is_deleted INTEGER DEFAULT 0, deleted_at INTEGER, phash TEXT, is_archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER, is_cloud_only INTEGER NOT NULL DEFAULT 0, clip_embedding BLOB, clip_status TEXT DEFAULT 'pending', tags_status TEXT DEFAULT 'pending', face_status TEXT DEFAULT 'pending', is_encrypted INTEGER DEFAULT 0);

CREATE VIRTUAL TABLE media_fts USING fts5(
    file_path,
    content = 'media',
    content_rowid = 'id',
    tokenize = 'porter'
);

CREATE TABLE media_tags (
    media_id INTEGER NOT NULL,
    tag_id INTEGER NOT NULL,
    confidence REAL NOT NULL DEFAULT 1.0,
    PRIMARY KEY (media_id, tag_id),
    FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE,
    FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
);

CREATE TABLE people (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,
    representative_embedding BLOB,
    photo_count INTEGER DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE "persons" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    cover_face_id INTEGER,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    FOREIGN KEY(cover_face_id) REFERENCES faces(id) ON DELETE SET NULL
);

CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE upload_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', -- pending, uploading, completed, failed
    retries INTEGER DEFAULT 0,
    error_msg TEXT,
    added_at INTEGER NOT NULL
);

CREATE INDEX idx_album_media_media ON album_media(media_id);

CREATE INDEX idx_faces_media ON faces(media_id);

CREATE INDEX idx_media_clip_status ON media(clip_status);

CREATE INDEX idx_media_created_at ON media(created_at);

CREATE INDEX idx_media_date_taken ON media(date_taken);

CREATE INDEX idx_media_face_status ON media(face_status);

CREATE UNIQUE INDEX idx_media_file_path ON media(file_path);

CREATE INDEX idx_media_is_archived ON media(is_archived);

CREATE INDEX idx_media_is_deleted ON media(is_deleted);

CREATE INDEX idx_media_phash ON media(phash);

CREATE INDEX idx_media_scan_status ON media(scan_status);

CREATE INDEX idx_media_tags_status ON media(tags_status);

CREATE INDEX idx_media_tags_tag ON media_tags(tag_id);

CREATE INDEX idx_media_telegram_media_id ON media(telegram_media_id);

CREATE UNIQUE INDEX idx_upload_queue_file_path
ON upload_queue(file_path);

CREATE TRIGGER media_fts_delete AFTER DELETE ON media BEGIN
    INSERT INTO media_fts (media_fts, rowid, file_path)
    VALUES ('delete', old.id, old.file_path);
END;

CREATE TRIGGER media_fts_insert AFTER INSERT ON media BEGIN
    INSERT INTO media_fts (rowid, file_path) VALUES (new.id, new.file_path);
END;

CREATE TRIGGER media_fts_update AFTER UPDATE OF file_path ON media BEGIN
    INSERT INTO media_fts (media_fts, rowid, file_path)
    VALUES ('delete', old.id, old.file_path);
    INSERT INTO media_fts (rowid, file_path) VALUES (new.id, new.file_path);
END;
