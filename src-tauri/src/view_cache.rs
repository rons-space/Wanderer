use log::{info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn cleanup_cache(
    cache_dir: &Path,
    max_size_bytes: u64,
    retention_secs: u64,
) -> std::io::Result<()> {
    if !cache_dir.exists() {
        return Ok(());
    }

    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total_size: u64 = 0;
    let now = SystemTime::now();

    // 1. Scan directory
    for entry in fs::read_dir(cache_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let metadata = fs::metadata(&path)?;
            let size = metadata.len();
            let accessed = metadata.accessed().or_else(|_| metadata.modified())?; // Use access time if available, else modified

            // Retention check
            if let Ok(age) = now.duration_since(accessed) {
                if age.as_secs() > retention_secs {
                    info!(
                        "ViewCache: Removing expired file {:?} (age: {:?})",
                        path, age
                    );
                    if let Err(e) = fs::remove_file(&path) {
                        warn!("ViewCache: Failed to remove file {:?}: {}", path, e);
                    }
                    continue; // Skip adding to list
                }
            }

            files.push((path, size, accessed));
            total_size += size;
        }
    }

    // 2. Size Check
    if total_size > max_size_bytes {
        info!(
            "ViewCache: Total size {} exceeds limit {}. Cleaning up...",
            total_size, max_size_bytes
        );

        // Sort by accessed time (oldest first)
        files.sort_by_key(|(_, _, accessed)| *accessed);

        for (path, size, _) in files {
            if total_size <= max_size_bytes {
                break;
            }

            info!("ViewCache: Removing file due to size limit: {:?}", path);
            if let Err(e) = fs::remove_file(&path) {
                warn!("ViewCache: Failed to remove file {:?}: {}", path, e);
            } else {
                total_size -= size;
            }
        }
    }

    Ok(())
}
