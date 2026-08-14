//! Multi-Device Sync via Telegram Saved Messages
//!
//! This module implements metadata synchronization across devices using
//! a JSON manifest stored in Telegram Saved Messages.
//!
//! ## Architecture
//! - Each device can export its local metadata to a sync manifest
//! - The manifest is uploaded to Telegram as a JSON file
//! - Other devices can download and merge the manifest using Last-Write-Wins (LWW)
//!
//! ## Sync Manifest Format
//! ```json
//! {
//!   "version": 1,
//!   "last_updated": "2026-01-20T12:00:00Z",
//!   "device_id": "uuid-of-device",
//!   "media": {
//!     "hash_abc123": {
//!       "is_favorite": true,
//!       "rating": 5,
//!       "albums": ["vacation", "family"],
//!       "last_modified": "2026-01-20T11:00:00Z"
//!     }
//!   },
//!   "albums": {
//!     "vacation": { "name": "Vacation 2026", "created": "2026-01-15T10:00:00Z" }
//!   }
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// The current sync manifest format version
pub const MANIFEST_VERSION: u32 = 1;

/// The filename used for the sync manifest in Telegram
pub const MANIFEST_FILENAME: &str = "wanderer_sync_manifest.json";

/// Largest manifest accepted from disk or from Telegram.
///
/// A manifest is untrusted input: it arrives from another device over Telegram
/// Saved Messages, or from whatever path the frontend passes in. 32 MiB is far
/// beyond a realistic library's metadata and keeps `read_to_string` from
/// pulling an arbitrary file into memory.
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// Largest number of entries accepted in either map.
const MAX_MANIFEST_ENTRIES: usize = 1_000_000;

/// Metadata for a single media item in the sync manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMetadata {
    /// Whether the item is favorited
    #[serde(default)]
    pub is_favorite: bool,

    /// Rating from 0-5
    #[serde(default)]
    pub rating: i32,

    /// Album names this item belongs to
    #[serde(default)]
    pub albums: Vec<String>,

    /// ISO timestamp of last modification for LWW conflict resolution
    pub last_modified: String,
}

/// Album definition in the sync manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumMetadata {
    /// Display name of the album
    pub name: String,

    /// ISO timestamp when the album was created
    pub created: String,
}

/// The complete sync manifest structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    /// Manifest format version for future compatibility
    pub version: u32,

    /// ISO timestamp of when this manifest was last updated
    pub last_updated: String,

    /// Unique identifier for the device that created this manifest
    pub device_id: String,

    /// Media metadata keyed by blake3 hash
    pub media: HashMap<String, MediaMetadata>,

    /// Album definitions keyed by album name (lowercase/normalized)
    pub albums: HashMap<String, AlbumMetadata>,
}

impl SyncManifest {
    /// Create a new empty manifest for this device
    pub fn new(device_id: String) -> Self {
        Self {
            version: MANIFEST_VERSION,
            last_updated: current_timestamp(),
            device_id,
            media: HashMap::new(),
            albums: HashMap::new(),
        }
    }

    /// Load a manifest from a JSON file.
    ///
    /// Everything here is validation of untrusted input: the size is bounded
    /// before the file is read, the format version is checked before the
    /// contents are believed, and both maps are bounded so a hostile manifest
    /// cannot drive unbounded work in the import loop.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let metadata =
            std::fs::metadata(path).map_err(|e| format!("Failed to read manifest file: {}", e))?;
        if !metadata.is_file() {
            return Err("Sync manifest is not a file".to_string());
        }
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "Sync manifest is too large: {} bytes (maximum {})",
                metadata.len(),
                MAX_MANIFEST_BYTES
            ));
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read manifest file: {}", e))?;
        let manifest: Self = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse manifest JSON: {}", e))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject manifests this build cannot safely apply.
    pub fn validate(&self) -> Result<(), String> {
        if self.version > MANIFEST_VERSION {
            return Err(format!(
                "Sync manifest version {} is newer than this app supports ({}). Update Wander(er) first.",
                self.version, MANIFEST_VERSION
            ));
        }
        if self.media.len() > MAX_MANIFEST_ENTRIES || self.albums.len() > MAX_MANIFEST_ENTRIES {
            return Err(format!(
                "Sync manifest is implausibly large: {} media entries, {} album entries",
                self.media.len(),
                self.albums.len()
            ));
        }
        Ok(())
    }

    /// Save the manifest to a JSON file
    pub fn to_file(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write manifest file: {}", e))
    }

    /// Merge a remote manifest into this one using Last-Write-Wins (LWW)
    ///
    /// For each media item, the version with the later `last_modified` timestamp wins.
    /// Albums are merged by name, with the remote version winning on conflict.
    // Exercised by the tests below but not by the sync worker, which uploads a freshly
    // built manifest instead of reconciling against the remote one. Kept because the
    // last-write-wins rule it encodes is the intended behaviour.
    #[allow(dead_code)]
    pub fn merge_from(&mut self, remote: &SyncManifest) {
        // Merge media metadata using LWW
        for (hash, remote_meta) in &remote.media {
            if let Some(local_meta) = self.media.get(hash) {
                // Compare timestamps - remote wins if later
                if remote_meta.last_modified > local_meta.last_modified {
                    self.media.insert(hash.clone(), remote_meta.clone());
                    log::debug!("LWW: Remote wins for media {}", hash);
                } else {
                    log::debug!("LWW: Local wins for media {}", hash);
                }
            } else {
                // New item from remote
                self.media.insert(hash.clone(), remote_meta.clone());
                log::debug!("Merged new media {} from remote", hash);
            }
        }

        // Merge albums - remote wins on conflict (simpler than per-album LWW)
        for (name, album) in &remote.albums {
            if !self.albums.contains_key(name) {
                self.albums.insert(name.clone(), album.clone());
                log::debug!("Merged new album '{}' from remote", name);
            }
        }

        // Update timestamp
        self.last_updated = current_timestamp();
    }

    /// Update metadata for a media item
    pub fn update_media(
        &mut self,
        hash: &str,
        is_favorite: bool,
        rating: i32,
        albums: Vec<String>,
    ) {
        self.media.insert(
            hash.to_string(),
            MediaMetadata {
                is_favorite,
                rating,
                albums,
                last_modified: current_timestamp(),
            },
        );
        self.last_updated = current_timestamp();
    }

    /// Add a new album
    pub fn add_album(&mut self, normalized_name: &str, display_name: &str) {
        if !self.albums.contains_key(normalized_name) {
            self.albums.insert(
                normalized_name.to_string(),
                AlbumMetadata {
                    name: display_name.to_string(),
                    created: current_timestamp(),
                },
            );
            self.last_updated = current_timestamp();
        }
    }
}

/// Get the current timestamp in ISO 8601 format
fn current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    // Simple ISO 8601 format using time crate
    let secs = duration.as_secs();
    let datetime = time::OffsetDateTime::from_unix_timestamp(secs as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

    datetime
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Generate a unique device ID (persisted in config)
pub fn generate_device_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Create a pseudo-unique ID based on hostname and random component
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let random: u64 = rand_simple();

    let mut hasher = DefaultHasher::new();
    hostname.hash(&mut hasher);
    random.hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

/// Simple random number without external crate
fn rand_simple() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    // XOR with nanoseconds for some entropy
    duration.as_nanos() as u64 ^ (duration.as_secs() << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_creation() {
        let manifest = SyncManifest::new("test-device".to_string());
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert_eq!(manifest.device_id, "test-device");
        assert!(manifest.media.is_empty());
        assert!(manifest.albums.is_empty());
    }

    #[test]
    fn test_lww_merge() {
        let mut local = SyncManifest::new("local".to_string());
        local.media.insert(
            "hash1".to_string(),
            MediaMetadata {
                is_favorite: true,
                rating: 3,
                albums: vec![],
                last_modified: "2026-01-20T10:00:00Z".to_string(),
            },
        );

        let mut remote = SyncManifest::new("remote".to_string());
        remote.media.insert(
            "hash1".to_string(),
            MediaMetadata {
                is_favorite: false,
                rating: 5,
                albums: vec!["vacation".to_string()],
                last_modified: "2026-01-20T11:00:00Z".to_string(), // Later
            },
        );

        local.merge_from(&remote);

        let merged = local.media.get("hash1").unwrap();
        assert!(!merged.is_favorite); // Remote value
        assert_eq!(merged.rating, 5); // Remote value
    }
}
