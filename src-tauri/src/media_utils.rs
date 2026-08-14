//! Shared media processing utilities.
//!
//! This module contains common functionality for hashing files and generating
//! thumbnails, used by both the file watcher and sync worker.

use log::{info, warn};
use std::io::BufReader;
use std::path::{Path, PathBuf};

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

    let source_clone = source_path.to_path_buf();
    let thumb_clone = thumb_path.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        // Check if FFmpeg is available
        let ffmpeg_check = Command::new("ffmpeg").arg("-version").output();
        if ffmpeg_check.is_err() {
            return Err("FFmpeg not found in PATH".to_string());
        }

        // Extract frame at 1 second mark
        let output = Command::new("ffmpeg")
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
                let fallback = Command::new("ffmpeg")
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

/// Escape special characters in LIKE patterns to prevent SQL injection issues.
// Broken as written: it inserts backslashes without an `ESCAPE '\\'` clause, so the
// pattern it produces matches literal backslashes. Its only caller,
// `Database::search_media`, is itself dead, and T55 (issue #63) deletes both.
#[allow(dead_code)]
pub fn escape_like_pattern(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_like_pattern() {
        assert_eq!(escape_like_pattern("test"), "test");
        assert_eq!(escape_like_pattern("100%"), "100\\%");
        assert_eq!(escape_like_pattern("a_b"), "a\\_b");
        assert_eq!(escape_like_pattern("c:\\path"), "c:\\\\path");
    }
}
