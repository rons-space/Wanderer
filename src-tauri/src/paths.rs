//! Confinement for paths that the backend did not construct itself.
//!
//! Three kinds of path reach this code and none of them can be trusted at face
//! value:
//!
//! - **Frontend-supplied**, from commands like `import_files`, `export_media`
//!   and `backup_database`. The webview is one script-injection away from being
//!   attacker-controlled, and these commands copy, create and delete files.
//! - **EXIF-derived**, such as the year and month folders in an export. That
//!   data comes from whoever produced the photo.
//! - **Database-supplied**, such as `media.file_path`. These were trusted
//!   absolutely by the delete paths, so a single bad row meant unlinking an
//!   arbitrary file.
//!
//! The rule everywhere is the same: resolve the path, then require it to sit
//! under a root the app owns or the user just chose.

use std::path::{Component, Path, PathBuf};

/// Extensions the importer accepts.
///
/// Imports land in the backup directory, which the watcher indexes and the
/// upload worker sends to Telegram, so an unfiltered import is an exfiltration
/// primitive: `import_files(["C:\\Users\\me\\.ssh\\id_rsa"])` would copy the key
/// into the library and upload it. Only formats the app can actually display
/// are allowed through.
pub const IMPORTABLE_EXTENSIONS: &[&str] = &[
    // Images
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif",
    // Video
    "mp4", "mov", "m4v", "avi", "mkv", "webm", "mpg", "mpeg", "wmv", "3gp",
];

/// True when the extension is one the importer accepts, including RAW formats.
pub fn is_importable(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_lowercase();
            IMPORTABLE_EXTENSIONS.contains(&ext.as_str())
                || crate::raw_support::is_raw_extension(&ext)
        }
        None => false,
    }
}

/// Resolve a path for comparison.
///
/// Canonicalizes as much of the path as exists, which resolves symlinks and is
/// the only way to stop a link inside a managed directory from pointing out of
/// it, then re-appends the components that do not exist yet. Doing it this way
/// matters on Windows, where `canonicalize` returns a `\\?\` verbatim prefix:
/// comparing a canonicalized root against a merely normalized candidate would
/// never match, and every confined write would fail.
pub fn resolve(path: &Path) -> PathBuf {
    let normalized = normalize_lexically(path);
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = normalized.as_path();

    loop {
        if let Ok(canonical) = std::fs::canonicalize(current) {
            let mut out = canonical;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                suffix.push(name.to_os_string());
                current = parent;
            }
            // Nothing along the path exists (or we reached the root).
            _ => return normalized,
        }
    }
}

/// Resolve `.` and `..` textually, without touching the filesystem.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Never pop past the root: `/../..` is `/`, not an escape.
                if out
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, Component::Normal(_)))
                {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True when `candidate` is `root` or sits underneath it, after resolution.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    let root = resolve(root);
    let candidate = resolve(candidate);
    candidate.starts_with(&root)
}

/// True when `candidate` sits under any of `roots`.
pub fn is_within_any(roots: &[PathBuf], candidate: &Path) -> bool {
    roots.iter().any(|root| is_within(root, candidate))
}

/// Require `candidate` to sit under `root`, returning the resolved path.
pub fn confine(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let resolved = resolve(candidate);
    if is_within(root, &resolved) {
        Ok(resolved)
    } else {
        Err(format!(
            "Refusing to use a path outside {}: {}",
            root.display(),
            candidate.display()
        ))
    }
}

/// Decrypted thumbnails, written so the webview can load them by path.
pub fn thumb_cache_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-thumb-cache")
}

/// Decrypted media, materialized for the viewer.
pub fn view_cache_materialized_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-view-cache-materialized")
}

/// Blobs as they arrive from Telegram, before they are decrypted into place.
pub fn download_staging_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-download-staging")
}

/// Where a restored archive is unpacked before it is put in place.
pub fn local_restore_staging_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-local-restore-staging")
}

/// Ciphertext staged for upload. Encrypted, but still this app's litter.
pub fn encrypted_uploads_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-encrypted-uploads")
}

/// Raw downloads on the view path, before they are re-encrypted into the cache.
pub fn view_cache_staging_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-view-cache-staging")
}

/// Plaintext copies made while migrating a library to encrypted storage.
pub fn migration_staging_dir() -> PathBuf {
    std::env::temp_dir().join("wanderer-migration")
}

/// Every scratch directory this app writes outside its own data directory.
///
/// All but one of these hold decrypted bytes at some point, which is the whole
/// problem: locking the vault cleared the key in memory and left the plaintext on
/// disk, where it survived the lock, the window closing and the reboot after it.
pub fn scratch_dirs() -> Vec<PathBuf> {
    vec![
        thumb_cache_dir(),
        view_cache_materialized_dir(),
        download_staging_dir(),
        local_restore_staging_dir(),
        encrypted_uploads_dir(),
        view_cache_staging_dir(),
        migration_staging_dir(),
    ]
}

/// Delete every scratch directory, returning how many were removed.
///
/// Deliberately best effort. A file the viewer still has open cannot be unlinked on
/// Windows, and failing the lock because of it would leave the user with no way to
/// lock at all, so each failure is logged and the rest still go.
///
/// Whole directories rather than a tracked list of files: a tracked list is only as
/// good as the code that remembers to add to it, and the thing being cleaned up is a
/// history of code paths that forgot.
///
/// This includes the directories holding in-flight work, so purging while an upload
/// or a download is staged through one of them will fail that transfer. That is the
/// intended trade: a failed transfer is retried from a source file that is still
/// there, whereas plaintext left behind by a lock is the thing being fixed.
pub fn purge_scratch_dirs() -> usize {
    let mut removed = 0;
    for dir in scratch_dirs() {
        if !dir.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!(
                "Could not purge decrypted scratch directory {}: {}",
                dir.display(),
                e
            ),
        }
    }
    removed
}

/// The directories the app owns inside its data directory.
///
/// Anything the app deletes on the user's behalf must live in one of these.
pub fn managed_roots(app_data: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        app_data.join("backup"),
        app_data.join("cache"),
        app_data.join("view_cache"),
        app_data.join("backups"),
    ];
    roots.extend(scratch_dirs());
    roots
}

/// Reduce untrusted text to a single safe path component.
///
/// Returns `None` when nothing usable survives, so the caller can fall back
/// rather than create a directory named after whatever was in the file.
// Nothing calls it yet: export sanitises date fragments with `sanitize_date_fragment`
// below, and album names still reach the filesystem unsanitised. Kept for the caller that
// should be using it.
#[allow(dead_code)]
pub fn sanitize_component(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    let cleaned = cleaned.trim_matches(['-', '_']).to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Sanitize an EXIF-derived date fragment used as a folder name.
///
/// Export builds `<destination>/<year>/<month>` out of `media.date_taken`,
/// which originates in the photo's own metadata. Only digits can be a year or a
/// month, and anything else is rejected rather than cleaned up, so a crafted
/// value falls back to the current date instead of steering the write.
pub fn sanitize_date_fragment(raw: &str, max_len: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > max_len || !trimmed.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_resolved_and_rejected() {
        let root = Path::new("/srv/library");
        assert!(is_within(root, Path::new("/srv/library/backup/a.jpg")));
        assert!(is_within(root, Path::new("/srv/library/./backup/../a.jpg")));
        assert!(!is_within(
            root,
            Path::new("/srv/library/../secrets/id_rsa")
        ));
        assert!(!is_within(root, Path::new("/etc/passwd")));
        // A sibling whose name merely starts with the root's name must not pass.
        assert!(!is_within(root, Path::new("/srv/library-other/a.jpg")));
    }

    #[test]
    fn parent_components_cannot_escape_the_root() {
        assert_eq!(
            normalize_lexically(Path::new("/a/b/../../../../c")),
            PathBuf::from("/c")
        );
        assert_eq!(
            normalize_lexically(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }

    /// The case that breaks a naive implementation: the root exists so it
    /// canonicalizes (with a `\\?\` prefix on Windows), while the file being
    /// written does not exist yet. Both sides must resolve the same way.
    #[test]
    fn confinement_holds_for_paths_that_do_not_exist_yet() {
        let dir = std::env::temp_dir().join(format!("wanderer-paths-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("2026").join("01")).unwrap();

        let target = dir.join("2026").join("01").join("photo.jpg");
        assert!(!target.exists());
        assert!(confine(&dir, &target).is_ok());

        let escape = dir.join("2026").join("..").join("..").join("elsewhere.jpg");
        assert!(confine(&dir, &escape).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_reports_the_offending_path() {
        let err = confine(Path::new("/srv/library"), Path::new("/etc/passwd")).unwrap_err();
        assert!(err.contains("/etc/passwd"));
        assert!(confine(Path::new("/srv/library"), Path::new("/srv/library/x")).is_ok());
    }

    /// A scratch directory that is not a managed root cannot be deleted by
    /// `delete_managed_file`, so the two lists drifting apart is a silent way to
    /// reintroduce exactly the leak this purge exists to close.
    #[test]
    fn every_scratch_dir_is_a_managed_root() {
        let roots = managed_roots(Path::new("/srv/wanderer-data"));
        for dir in scratch_dirs() {
            assert!(
                roots.iter().any(|r| r == &dir),
                "{} is written to but is not a managed root",
                dir.display()
            );
        }
    }

    /// Touches the real scratch locations, because those paths are the thing under
    /// test. They belong to this application and no other test uses them.
    ///
    /// One test rather than two, because these share global directories and would
    /// race each other under the default parallel test runner.
    #[test]
    fn purging_removes_scratch_directories_and_tolerates_their_absence() {
        let dir = thumb_cache_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let plaintext = dir.join("decrypted.jpg");
        std::fs::write(&plaintext, b"decrypted bytes").unwrap();

        assert!(purge_scratch_dirs() >= 1);
        assert!(!plaintext.exists(), "decrypted file survived the purge");
        assert!(!dir.exists(), "scratch directory survived the purge");

        // Nothing left to remove is the normal case at startup, and must not be an
        // error or a second failure.
        assert_eq!(purge_scratch_dirs(), 0);
    }

    #[test]
    fn only_media_extensions_are_importable() {
        for ok in ["a.jpg", "a.JPEG", "b.mp4", "c.heic", "d.cr2", "e.NEF"] {
            assert!(is_importable(Path::new(ok)), "{ok} should be importable");
        }
        // The exfiltration cases: no extension, keys, documents, archives.
        for bad in [
            "id_rsa",
            "wallet.dat",
            "notes.docx",
            "library.db",
            "secrets.txt",
            "backup.zip",
            "script.exe",
        ] {
            assert!(!is_importable(Path::new(bad)), "{bad} must be refused");
        }
    }

    #[test]
    fn date_fragments_must_be_digits() {
        assert_eq!(sanitize_date_fragment("2026", 4).as_deref(), Some("2026"));
        assert_eq!(sanitize_date_fragment("01", 2).as_deref(), Some("01"));
        assert_eq!(sanitize_date_fragment("..", 4), None);
        assert_eq!(sanitize_date_fragment("../..", 4), None);
        assert_eq!(sanitize_date_fragment("2026/../..", 4), None);
        assert_eq!(sanitize_date_fragment("", 4), None);
        assert_eq!(sanitize_date_fragment("20260", 4), None);
    }

    #[test]
    fn components_are_reduced_to_safe_text() {
        assert_eq!(
            sanitize_component("holiday 2026!").as_deref(),
            Some("holiday2026")
        );
        assert_eq!(sanitize_component("../../etc").as_deref(), Some("etc"));
        assert!(sanitize_component("///").is_none());
    }
}
