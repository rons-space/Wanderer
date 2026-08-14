//! Shared media processing utilities.
//!
//! This module contains common functionality for hashing files and generating
//! thumbnails, used by both the file watcher and sync worker.

use log::{info, warn};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Name of the ffmpeg executable on this platform.
#[cfg(windows)]
const FFMPEG_EXE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG_EXE: &str = "ffmpeg";

/// Absolute path to ffmpeg, resolved once per process.
///
/// `Command::new("ffmpeg")` resolves the name at spawn time, and on Windows
/// `CreateProcess` searches the process working directory before `PATH`. Either way an
/// `ffmpeg.exe` dropped somewhere the user can write runs with Wanderer's privileges
/// the next time a video is imported. Resolving here means the path is decided once,
/// from directories chosen deliberately, and the spawn is by absolute path.
fn ffmpeg_path() -> Option<&'static Path> {
    static FFMPEG: OnceLock<Option<PathBuf>> = OnceLock::new();
    FFMPEG
        .get_or_init(|| {
            let resolved = resolve_ffmpeg();
            match &resolved {
                Some(path) => info!("Using ffmpeg at {:?}", path),
                None => warn!(
                    "No ffmpeg found beside the executable or on an absolute PATH entry; \
                     video thumbnails are disabled"
                ),
            }
            resolved
        })
        .as_deref()
}

fn resolve_ffmpeg() -> Option<PathBuf> {
    // A copy shipped beside the executable wins: it is inside the installation
    // directory, which is the only place with the same trust as the binary itself.
    if let Some(sidecar) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(FFMPEG_EXE)))
    {
        if is_executable_file(&sidecar) {
            return Some(sidecar);
        }
    }

    let path_var = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    find_executable(&dirs, FFMPEG_EXE)
}

/// First directory holding an executable `name`, ignoring relative entries.
///
/// A relative `PATH` entry resolves against whatever the working directory happens to
/// be, and the empty entry that a stray `;` produces on Windows *is* the working
/// directory. Neither is a location this process should be taking a binary from.
fn find_executable(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Hash a file using Blake3 with streaming to avoid loading entire file into memory.
///
/// This is safe for large files (videos can be 10GB+) as it reads in chunks.
pub fn hash_file_streaming(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;

    // Reject empty files
    if metadata.len() == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "File is empty",
        ));
    }

    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();

    std::io::copy(&mut reader, &mut hasher)?;

    Ok(hasher.finalize().to_hex().to_string())
}

/// Generate a perceptual hash for an image file.
///
/// Perceptual hashes are similar for visually similar images,
/// enabling duplicate detection regardless of resolution/compression.
pub fn generate_phash(path: &Path) -> Option<String> {
    use img_hash::{HasherConfig, ImageHash};

    // Decode via explicitly configured image 0.23 dependency (with codecs enabled).
    // This matches img_hash's expected image types while ensuring JPEG/PNG decode works.
    let img = image_023::open(path).ok()?;
    let hasher = HasherConfig::new()
        .hash_size(8, 8) // 64-bit hash
        .to_hasher();
    let hash: ImageHash = hasher.hash_image(&img);
    Some(hash.to_base64())
}

/// Generate a thumbnail for an image file.
///
/// Returns `Ok(Some(path))` if thumbnail was created successfully,
/// `Ok(None)` if the file is not an image/unsupported format,
/// `Err` for actual errors.
///
/// This function now supports RAW camera files (CR2, NEF, ARW, etc.) by
/// extracting their embedded JPEG preview.
pub async fn generate_thumbnail(
    source_path: &Path,
    cache_dir: &Path,
    hash: &str,
    max_size: u32,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let thumb_dir = cache_dir.join("thumbnails");
    if !thumb_dir.exists() {
        std::fs::create_dir_all(&thumb_dir)?;
    }

    let thumb_path = thumb_dir.join(format!("{}.jpg", hash));

    // Skip if thumbnail already exists
    if thumb_path.exists() {
        return Ok(Some(thumb_path));
    }

    let source_clone = source_path.to_path_buf();
    let thumb_clone = thumb_path.clone();

    // Check if this is a RAW file
    let is_raw = source_path
        .extension()
        .map(|ext| crate::raw_support::is_raw_extension(&ext.to_string_lossy()))
        .unwrap_or(false);

    let result = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        if is_raw {
            // Handle RAW files by extracting embedded JPEG
            match crate::raw_support::extract_embedded_jpeg(&source_clone) {
                Ok(jpeg_bytes) => {
                    // Decode the extracted JPEG
                    match image::load_from_memory(&jpeg_bytes) {
                        Ok(img) => {
                            let thumb = img.thumbnail(max_size, max_size);
                            if let Err(e) = thumb.save(&thumb_clone) {
                                return Err(format!("Failed to save RAW thumbnail: {}", e));
                            }
                            info!(
                                "Generated thumbnail from RAW embedded JPEG: {:?}",
                                source_clone
                            );
                            Ok(true)
                        }
                        Err(e) => Err(format!("Failed to decode extracted JPEG from RAW: {}", e)),
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to extract embedded JPEG from RAW {:?}: {}",
                        source_clone, e
                    );
                    Err(e)
                }
            }
        } else {
            // Handle regular image files
            match image::open(&source_clone) {
                Ok(img) => {
                    let thumb = img.thumbnail(max_size, max_size);
                    if let Err(e) = thumb.save(&thumb_clone) {
                        return Err(format!("Failed to save thumbnail: {}", e));
                    }
                    Ok(true)
                }
                Err(e) => {
                    // Not an image or unsupported format - this is expected for non-image files
                    Err(format!("Image open failed (likely not an image): {}", e))
                }
            }
        }
    })
    .await?;

    match result {
        Ok(true) => {
            info!("Thumbnail generated: {:?}", thumb_path);
            Ok(Some(thumb_path))
        }
        Ok(false) => Ok(None),
        Err(e) => {
            warn!("Skipping thumbnail for {:?}: {}", source_path, e);
            Ok(None)
        }
    }
}

/// Generate a thumbnail for a video file using FFmpeg.
///
/// Extracts a frame at 1 second (or first frame for short videos).
/// Returns `Ok(Some(path))` if thumbnail was created successfully,
/// `Ok(None)` if FFmpeg is not available or extraction failed,
/// `Err` for actual errors.
pub async fn generate_video_thumbnail(
    source_path: &Path,
    cache_dir: &Path,
    hash: &str,
    max_size: u32,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    use std::process::Command;

    let thumb_dir = cache_dir.join("thumbnails");
    if !thumb_dir.exists() {
        std::fs::create_dir_all(&thumb_dir)?;
    }

    let thumb_path = thumb_dir.join(format!("{}.jpg", hash));

    // Skip if thumbnail already exists
    if thumb_path.exists() {
        return Ok(Some(thumb_path));
    }

    let Some(ffmpeg) = ffmpeg_path() else {
        warn!("Skipping video thumbnail: ffmpeg is not available");
        return Ok(None);
    };

    let source_clone = source_path.to_path_buf();
    let thumb_clone = thumb_path.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        // Resolution above already proved the binary exists and is executable, so the
        // `-version` probe that used to run here (a process spawn per thumbnail) is gone.

        // Extract frame at 1 second mark
        let output = Command::new(ffmpeg)
            .args([
                "-ss",
                "1", // Seek to 1 second
                "-i",
                &source_clone.to_string_lossy(),
                "-vframes",
                "1", // Extract 1 frame
                "-vf",
                &format!(
                    "scale='min({},iw)':min'({},ih)':force_original_aspect_ratio=decrease",
                    max_size, max_size
                ),
                "-y", // Overwrite output
                &thumb_clone.to_string_lossy(),
            ])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                if thumb_clone.exists() {
                    Ok(true)
                } else {
                    Err("FFmpeg ran but no thumbnail created".to_string())
                }
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Try extracting from first frame if 1 second seek failed
                let fallback = Command::new(ffmpeg)
                    .args([
                        "-i",
                        &source_clone.to_string_lossy(),
                        "-vframes",
                        "1",
                        "-vf",
                        &format!(
                            "scale='min({},iw)':min'({},ih)':force_original_aspect_ratio=decrease",
                            max_size, max_size
                        ),
                        "-y",
                        &thumb_clone.to_string_lossy(),
                    ])
                    .output();

                match fallback {
                    Ok(f) if f.status.success() && thumb_clone.exists() => Ok(true),
                    _ => Err(format!("FFmpeg failed: {}", stderr)),
                }
            }
            Err(e) => Err(format!("Failed to run FFmpeg: {}", e)),
        }
    })
    .await?;

    match result {
        Ok(true) => {
            info!("Video thumbnail generated: {:?}", thumb_path);
            Ok(Some(thumb_path))
        }
        Ok(false) => Ok(None),
        Err(e) => {
            warn!("Skipping video thumbnail for {:?}: {}", source_path, e);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that removes itself, so the executable-lookup tests leave nothing
    /// behind in the temp directory.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wanderer-mediautils-{}-{}-{:?}",
                name,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn write_executable(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_executable_is_found_in_an_absolute_directory() {
        let dir = TempDir::new("found");
        let expected = dir.write_executable(FFMPEG_EXE);

        assert_eq!(
            find_executable(std::slice::from_ref(&dir.0), FFMPEG_EXE),
            Some(expected)
        );
    }

    /// The whole point of the change: a binary reachable only through a relative
    /// `PATH` entry, which is to say through the working directory, is not used.
    #[test]
    fn relative_path_entries_are_ignored() {
        let dir = TempDir::new("relative");
        dir.write_executable(FFMPEG_EXE);

        let relative = [
            PathBuf::from(""),
            PathBuf::from("."),
            PathBuf::from("tools"),
        ];
        assert_eq!(find_executable(&relative, FFMPEG_EXE), None);
    }

    #[test]
    fn the_first_absolute_directory_holding_it_wins() {
        let first = TempDir::new("first");
        let second = TempDir::new("second");
        let expected = first.write_executable(FFMPEG_EXE);
        second.write_executable(FFMPEG_EXE);

        assert_eq!(
            find_executable(&[first.0.clone(), second.0.clone()], FFMPEG_EXE),
            Some(expected)
        );
    }

    #[test]
    fn a_directory_is_not_mistaken_for_the_binary() {
        let dir = TempDir::new("dir-shaped");
        std::fs::create_dir_all(dir.0.join(FFMPEG_EXE)).expect("create decoy directory");

        assert_eq!(
            find_executable(std::slice::from_ref(&dir.0), FFMPEG_EXE),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_is_skipped() {
        let dir = TempDir::new("not-executable");
        std::fs::write(dir.0.join(FFMPEG_EXE), b"not executable").expect("write file");

        assert_eq!(
            find_executable(std::slice::from_ref(&dir.0), FFMPEG_EXE),
            None
        );
    }
}
