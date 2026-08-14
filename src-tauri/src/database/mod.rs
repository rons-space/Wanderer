use image_hasher::ImageHash;
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use time::OffsetDateTime;

// The methods live in one module per domain, each re-opening `impl Database`.
// They are private because nothing outside addresses them by module: callers
// hold a `Database` and reach every method through it.
mod ai;
mod albums;
mod config;
mod media;
mod migrations;
mod people;
mod queue;
mod sync;

#[cfg(test)]
mod tests;

/// The columns `map_media_row` reads, in the order it reads them.
///
/// Every query that feeds that mapper must select exactly this list: the mapper
/// addresses columns by index, so a query that selects them in another order, or
/// adds one in the middle, silently loads the wrong field into each item rather
/// than failing. Naming the list once is what keeps the two in step.
const MEDIA_COLUMNS: &str = "id, file_path, file_hash, telegram_media_id, mime_type, width, height, \
     duration, size_bytes, created_at, uploaded_at, thumbnail_path, date_taken, latitude, longitude, \
     camera_make, camera_model, is_favorite, rating, is_deleted, deleted_at, is_archived, archived_at, \
     is_cloud_only";

/// `MEDIA_COLUMNS` qualified with the `m` alias, for the queries that join.
const MEDIA_COLUMNS_M: &str = "m.id, m.file_path, m.file_hash, m.telegram_media_id, m.mime_type, \
     m.width, m.height, m.duration, m.size_bytes, m.created_at, m.uploaded_at, m.thumbnail_path, \
     m.date_taken, m.latitude, m.longitude, m.camera_make, m.camera_model, m.is_favorite, m.rating, \
     m.is_deleted, m.deleted_at, m.is_archived, m.archived_at, m.is_cloud_only";

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

/// Largest pHash distance still considered the same picture.
///
/// The banding in `cluster_by_phash` is derived from this value, so the two have to
/// move together: `THRESHOLD + 1` bands is what makes the bucketing exact.
const PHASH_DISTANCE_THRESHOLD: u32 = 10;

/// A perceptual hash parsed into raw bits, once.
///
/// The stored text is base64 from `image_hasher` for anything imported by this
/// application, but older rows hold a hex `u64`, and both forms have to keep
/// comparing. Parsing on every pair, which is what comparing the strings did,
/// meant two base64 decodes per comparison and O(n^2) of them.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ParsedHash {
    bits: Vec<u8>,
}

impl ParsedHash {
    fn parse(text: &str) -> Option<Self> {
        // base64 first, matching the order the string comparison used: a hex hash
        // is also valid base64, so changing the order here would reinterpret every
        // legacy hash and silently change which items are duplicates.
        //
        // The annotation picks `ImageHash`'s default byte container; without it the
        // generic parameter is ambiguous at the call site.
        let decoded: std::result::Result<ImageHash, _> = ImageHash::from_base64(text);
        if let Ok(hash) = decoded {
            return Some(Self {
                bits: hash.as_bytes().to_vec(),
            });
        }

        u64::from_str_radix(text, 16).ok().map(|value| Self {
            bits: value.to_be_bytes().to_vec(),
        })
    }

    fn width_bits(&self) -> usize {
        self.bits.len() * 8
    }

    fn distance(&self, other: &Self) -> u32 {
        debug_assert_eq!(self.bits.len(), other.bits.len());
        self.bits
            .iter()
            .zip(other.bits.iter())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum()
    }

    /// The bits of the half-open range `[from, to)`, as a bucket key.
    fn band(&self, from: usize, to: usize) -> u64 {
        let mut key = 0u64;
        for bit in from..to {
            key <<= 1;
            let byte = self.bits[bit / 8];
            key |= ((byte >> (7 - (bit % 8))) & 1) as u64;
        }
        key
    }
}

/// Group items whose perceptual hashes are within `threshold` bits of each other.
///
/// The pairwise scan this replaces was O(n^2) *distance computations*, each one
/// re-parsing two base64 strings, and it ran with the database lock held. Here each
/// hash is parsed once, and pairs are proposed by bucketing rather than by trying
/// all of them.
///
/// The bucketing is exact rather than approximate. Splitting a hash into
/// `threshold + 1` bands means two hashes within `threshold` bits differ in at most
/// `threshold` bands, so at least one band is identical and the pair always meets in
/// some bucket. Every proposed pair is still checked with the real distance, so the
/// result is what the exhaustive scan produced.
fn cluster_by_phash(candidates: Vec<(MediaItem, String)>, threshold: u32) -> Vec<Vec<MediaItem>> {
    let parsed: Vec<(MediaItem, Option<ParsedHash>)> = candidates
        .into_iter()
        .map(|(item, text)| {
            let hash = ParsedHash::parse(&text);
            (item, hash)
        })
        .collect();

    let n = parsed.len();
    if n < 2 {
        return Vec::new();
    }

    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank = vec![0usize; n];

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    fn union(parent: &mut [usize], rank: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra == rb {
            return;
        }
        match rank[ra].cmp(&rank[rb]) {
            std::cmp::Ordering::Less => parent[ra] = rb,
            std::cmp::Ordering::Greater => parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                parent[rb] = ra;
                rank[ra] += 1;
            }
        }
    }

    let band_count = threshold as usize + 1;

    // Hashes of different widths are not comparable, so bucket within a width.
    let mut by_width: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for (index, (_, hash)) in parsed.iter().enumerate() {
        if let Some(hash) = hash {
            by_width.entry(hash.width_bits()).or_default().push(index);
        }
    }

    for (width, indices) in by_width {
        if width < band_count {
            // Too narrow to band: fall back to comparing the group directly. This is
            // the degenerate case of a hash shorter than 11 bits, which no supported
            // configuration produces.
            for (a, &i) in indices.iter().enumerate() {
                for &j in &indices[a + 1..] {
                    let (Some(hi), Some(hj)) = (&parsed[i].1, &parsed[j].1) else {
                        continue;
                    };
                    if hi.distance(hj) <= threshold {
                        union(&mut parent, &mut rank, i, j);
                    }
                }
            }
            continue;
        }

        let mut buckets: std::collections::HashMap<(usize, u64), Vec<usize>> =
            std::collections::HashMap::new();

        for band in 0..band_count {
            let from = width * band / band_count;
            let to = width * (band + 1) / band_count;
            for &index in &indices {
                let Some(hash) = &parsed[index].1 else {
                    continue;
                };
                buckets
                    .entry((band, hash.band(from, to)))
                    .or_default()
                    .push(index);
            }
        }

        for members in buckets.into_values() {
            if members.len() < 2 {
                continue;
            }
            for (a, &i) in members.iter().enumerate() {
                for &j in &members[a + 1..] {
                    if find(&mut parent, i) == find(&mut parent, j) {
                        continue;
                    }
                    let (Some(hi), Some(hj)) = (&parsed[i].1, &parsed[j].1) else {
                        continue;
                    };
                    if hi.distance(hj) <= threshold {
                        union(&mut parent, &mut rank, i, j);
                    }
                }
            }
        }
    }

    let mut grouped: std::collections::HashMap<usize, Vec<MediaItem>> =
        std::collections::HashMap::new();
    for (index, (item, _)) in parsed.into_iter().enumerate() {
        let root = find(&mut parent, index);
        grouped.entry(root).or_default().push(item);
    }

    let mut groups: Vec<Vec<MediaItem>> = grouped
        .into_values()
        .filter(|items| items.len() > 1)
        .collect();

    for group in &mut groups {
        group.sort_by_key(|item| item.created_at);
    }

    groups.sort_by_key(|group| std::cmp::Reverse(group.len()));
    groups
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
}
