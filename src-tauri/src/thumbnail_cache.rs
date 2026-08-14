//! A size bound on the thumbnails directory.
//!
//! Thumbnails are 300px JPEGs written once per imported file and kept forever,
//! so a large library grows a few gigabytes of them that nothing ever removes.
//! `cache_size_mb` has existed in the settings since the first migration and
//! has never been read by anything; this is what it means.
//!
//! The rule that matters here is the one the old `ThumbnailCache` broke (#13):
//! `media.thumbnail_path` must stop pointing at a file before that file is
//! deleted. Its moka eviction listener unlinked JPEGs and left the column
//! alone, so the gallery went on asking for files that were no longer there.
//! Every eviction below clears the column first and unlinks second, which
//! makes the worst case an orphaned file rather than a broken row.

use crate::database::Database;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One file in the thumbnails directory, as the planner sees it.
#[derive(Debug, Clone)]
pub struct CachedThumbnail {
    pub path: PathBuf,
    pub size: u64,
    /// Access time where the platform records it, modification time otherwise.
    pub last_used: SystemTime,
}

/// Which thumbnails to evict to bring the directory under `max_size_bytes`.
///
/// Least recently used first, which for thumbnails means the parts of the
/// library the user has not scrolled past in the longest time. Regenerating one
/// costs a decode and a resize of the original, so evicting the wrong file is
/// slow rather than destructive.
pub fn plan_eviction(mut files: Vec<CachedThumbnail>, max_size_bytes: u64) -> Vec<CachedThumbnail> {
    let mut total: u64 = files.iter().map(|file| file.size).sum();
    if total <= max_size_bytes {
        return Vec::new();
    }

    files.sort_by_key(|file| file.last_used);

    let mut evicted = Vec::new();
    for file in files {
        if total <= max_size_bytes {
            break;
        }
        total = total.saturating_sub(file.size);
        evicted.push(file);
    }

    evicted
}

fn read_directory(thumb_dir: &Path) -> std::io::Result<Vec<CachedThumbnail>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(thumb_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let metadata = entry.metadata()?;
        // Access time is not recorded on every filesystem, and on those mounted
        // `noatime` it never moves, so modification time is the fallback.
        let last_used = metadata
            .accessed()
            .or_else(|_| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        files.push(CachedThumbnail {
            path,
            size: metadata.len(),
            last_used,
        });
    }

    Ok(files)
}

/// Bring the thumbnails directory under its configured size, forgetting each
/// file in the database before removing it. Returns the number of files
/// removed.
pub fn enforce_budget(
    db: &Database,
    thumb_dir: &Path,
    max_size_bytes: u64,
) -> std::io::Result<usize> {
    if !thumb_dir.exists() {
        return Ok(0);
    }

    let evicted = plan_eviction(read_directory(thumb_dir)?, max_size_bytes);
    if evicted.is_empty() {
        return Ok(0);
    }

    let paths: Vec<String> = evicted
        .iter()
        .map(|file| file.path.to_string_lossy().to_string())
        .collect();

    // First, so that a crash between the two leaves a file with no row pointing
    // at it. The other order leaves rows pointing at nothing, which is the bug
    // this module exists to avoid.
    if let Err(e) = db.clear_thumbnail_paths(&paths) {
        log::error!("Not evicting thumbnails: failed to clear their paths first: {e}");
        return Ok(0);
    }

    let mut removed = 0;
    for file in evicted {
        match fs::remove_file(&file.path) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!("Failed to remove thumbnail {}: {}", file.path.display(), e),
        }
    }

    log::info!(
        "Thumbnail cache: removed {} file(s) to stay under {} MB",
        removed,
        max_size_bytes / (1024 * 1024)
    );
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn thumbnail(name: &str, size: u64, age_secs: u64) -> CachedThumbnail {
        CachedThumbnail {
            path: PathBuf::from(name),
            size,
            last_used: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
        }
    }

    #[test]
    fn a_directory_under_its_budget_is_left_alone() {
        let files = vec![thumbnail("a.jpg", 10, 1), thumbnail("b.jpg", 10, 2)];
        assert!(plan_eviction(files, 100).is_empty());
    }

    #[test]
    fn a_directory_exactly_at_its_budget_is_left_alone() {
        let files = vec![thumbnail("a.jpg", 50, 1), thumbnail("b.jpg", 50, 2)];
        assert!(plan_eviction(files, 100).is_empty());
    }

    #[test]
    fn the_least_recently_used_files_go_first() {
        let files = vec![
            thumbnail("recent.jpg", 40, 1),
            thumbnail("old.jpg", 40, 500),
            thumbnail("middle.jpg", 40, 100),
        ];

        let evicted = plan_eviction(files, 100);

        assert_eq!(
            evicted
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("old.jpg")]
        );
    }

    #[test]
    fn eviction_stops_as_soon_as_the_budget_is_met() {
        let files = vec![
            thumbnail("a.jpg", 30, 400),
            thumbnail("b.jpg", 30, 300),
            thumbnail("c.jpg", 30, 200),
            thumbnail("d.jpg", 30, 100),
        ];

        let evicted = plan_eviction(files, 60);

        // 120 bytes down to 60 needs two files, not three.
        assert_eq!(evicted.len(), 2);
        assert_eq!(evicted[0].path, PathBuf::from("a.jpg"));
        assert_eq!(evicted[1].path, PathBuf::from("b.jpg"));
    }

    /// A budget of zero is not reachable from the settings slider, but it must
    /// not loop or panic if it ever is.
    #[test]
    fn a_zero_budget_evicts_everything() {
        let files = vec![thumbnail("a.jpg", 10, 1), thumbnail("b.jpg", 10, 2)];
        assert_eq!(plan_eviction(files, 0).len(), 2);
    }

    #[test]
    fn an_empty_directory_needs_no_eviction() {
        assert!(plan_eviction(Vec::new(), 0).is_empty());
    }

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A database and a thumbnails directory on disk, removed on drop.
    struct TempLibrary {
        dir: PathBuf,
        db: Database,
    }

    impl TempLibrary {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "wanderer-thumb-test-{}-{}",
                std::process::id(),
                n
            ));
            fs::create_dir_all(dir.join("cache").join("thumbnails")).unwrap();
            let db = Database::new(dir.join("library.db")).expect("open the test database");
            Self { dir, db }
        }

        fn thumb_dir(&self) -> PathBuf {
            self.dir.join("cache").join("thumbnails")
        }

        /// A media row and the thumbnail file it points at.
        fn add(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.thumb_dir().join(name);
            fs::write(&path, vec![b'x'; bytes]).unwrap();
            let conn = self.db.get_conn().unwrap();
            conn.execute(
                "INSERT INTO media (file_path, thumbnail_path, created_at) VALUES (?1, ?2, 0)",
                rusqlite::params![
                    format!("/photos/{name}"),
                    path.to_string_lossy().to_string()
                ],
            )
            .unwrap();
            path
        }

        fn thumbnail_of(&self, name: &str) -> Option<String> {
            let conn = self.db.get_conn().unwrap();
            conn.query_row(
                "SELECT thumbnail_path FROM media WHERE file_path = ?1",
                [format!("/photos/{name}")],
                |row| row.get(0),
            )
            .unwrap()
        }
    }

    impl Drop for TempLibrary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_library_under_its_budget_keeps_every_thumbnail() {
        let library = TempLibrary::new();
        let kept = library.add("a.jpg", 100);

        let removed = enforce_budget(&library.db, &library.thumb_dir(), 10_000).unwrap();

        assert_eq!(removed, 0);
        assert!(kept.exists());
        assert!(library.thumbnail_of("a.jpg").is_some());
    }

    /// The regression #13 left behind: the file went and the column did not, so
    /// the gallery asked for a thumbnail that was no longer there.
    #[test]
    fn an_evicted_thumbnail_is_forgotten_as_well_as_deleted() {
        let library = TempLibrary::new();
        let evicted = library.add("old.jpg", 1_000);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let kept = library.add("new.jpg", 1_000);

        let removed = enforce_budget(&library.db, &library.thumb_dir(), 1_000).unwrap();

        assert_eq!(removed, 1);
        assert!(!evicted.exists(), "the evicted file must be gone");
        assert!(kept.exists(), "the newer thumbnail must survive");
        assert_eq!(
            library.thumbnail_of("old.jpg"),
            None,
            "the row must stop pointing at the file that was deleted"
        );
        assert!(library.thumbnail_of("new.jpg").is_some());
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let library = TempLibrary::new();
        let absent = library.dir.join("cache").join("nothing-here");
        assert_eq!(enforce_budget(&library.db, &absent, 0).unwrap(), 0);
    }
}
