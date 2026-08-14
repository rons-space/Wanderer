//! Integrity checks for the ONNX models and tokenizer the app downloads at runtime.
//!
//! These files are fetched over the network and then handed straight to a parser that
//! has had at least one out-of-bounds read reported against it (RUSTSEC-2026-0217,
//! tracked in #81). A model is also, functionally, code: whatever it computes is what
//! the app believes about the user's photos. Neither property survives "the download
//! returned 200 and the file is bigger than a megabyte", which is all that was checked
//! before.
//!
//! So every artifact is pinned by SHA-256 and by size, verified after download and
//! again before the first parse, and mirrors are treated as interchangeable only when
//! they serve exactly the pinned bytes. A mirror that has been repointed fails the
//! check and the next one is tried, which is the behaviour that would have contained a
//! compromised mirror rather than spreading it.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// A file this application will download and then parse, pinned to exact content.
#[derive(Debug, Clone, Copy)]
pub struct PinnedArtifact {
    /// Name used in log lines and error messages.
    pub name: &'static str,
    /// Lowercase hex SHA-256 of the exact bytes expected.
    pub sha256: &'static str,
    /// Expected length in bytes. Checked first because it is free and rules out the
    /// common failure, a truncated or partially written download, without reading the
    /// whole file.
    pub size: u64,
}

impl PinnedArtifact {
    /// Verify a file on disk against the pin.
    ///
    /// Returns `Ok(())` only for an exact match. Any other outcome, including a file
    /// that cannot be read, is an error: a model that cannot be proven to be the right
    /// one is not a model this app is willing to parse.
    pub fn verify(&self, path: &Path) -> Result<(), String> {
        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("Cannot read {} at {}: {}", self.name, path.display(), e))?;

        if metadata.len() != self.size {
            return Err(format!(
                "{} is {} bytes, expected {}. The download is incomplete or the source \
                 has changed.",
                self.name,
                metadata.len(),
                self.size
            ));
        }

        let actual = sha256_file(path)?;
        if !actual.eq_ignore_ascii_case(self.sha256) {
            return Err(format!(
                "{} failed its integrity check. Expected SHA-256 {}, got {}. Refusing to \
                 load it.",
                self.name, self.sha256, actual
            ));
        }

        Ok(())
    }

    /// Verify, and delete the file if it does not match.
    ///
    /// Used on the download path, where leaving a rejected file behind would mean the
    /// next run finds it, decides the model is already present and skips the download
    /// that would have replaced it.
    pub fn verify_or_remove(&self, path: &Path) -> Result<(), String> {
        match self.verify(path) {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Err(remove_err) = std::fs::remove_file(path) {
                    log::warn!(
                        "Failed to remove the rejected file at {}: {}",
                        path.display(),
                        remove_err
                    );
                }
                Err(e)
            }
        }
    }
}

/// Stream a file through SHA-256. Chunked because the largest of these is 335 MB and
/// reading it into memory to hash it would be the biggest allocation in the process.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|e| format!("Failed to open {} for hashing: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("Failed to read {} while hashing: {}", path.display(), e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{:02x}", b);
            acc
        })
}

// The pins themselves. Each was taken from the upstream source at the revision named
// beside it: HuggingFace exposes the LFS SHA-256 as `x-linked-etag`, GitHub exposes it
// in the LFS pointer, and both were confirmed against the bytes actually served.

/// ArcFace face recognition, `w600k_r50.onnx`.
///
/// All four mirrors in `ai::download_arcface_model` were confirmed to serve these
/// exact bytes, so one pin covers the fallback chain.
pub const ARCFACE: PinnedArtifact = PinnedArtifact {
    name: "ArcFace (w600k_r50.onnx)",
    sha256: "4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43",
    size: 174_383_860,
};

/// MobileNet V2 classification, `mobilenetv2-7.onnx`, from onnx/models at commit
/// 4c46cd00fbdb7cd30b6c1c17ab54f2e1f4f7b177.
pub const MOBILENET: PinnedArtifact = PinnedArtifact {
    name: "MobileNet V2 (mobilenetv2-7.onnx)",
    sha256: "c1c513582d56afceff8516c73804e484c81c6a830712ab6d682253f4a3cd042f",
    size: 14_246_826,
};

/// CLIP ViT-B/32 vision tower, from Xenova/clip-vit-base-patch32 at commit
/// d15189d7028b43f1d3e65039190477f6af591c2a.
pub const CLIP_VISUAL: PinnedArtifact = PinnedArtifact {
    name: "CLIP vision model",
    sha256: "fd6e1402a588279d1723c7534d4bcba5bc0b14b47dfab0e46f8c47b8270d7d40",
    size: 351_685_709,
};

/// CLIP ViT-B/32 quantized text tower, same revision.
pub const CLIP_TEXTUAL: PinnedArtifact = PinnedArtifact {
    name: "CLIP text model",
    sha256: "73baab855d406190da9faa498cfedf65f15cf309f4cc7385b7b032e6d08e5c3a",
    size: 64_504_507,
};

/// CLIP tokenizer, same revision.
pub const CLIP_TOKENIZER: PinnedArtifact = PinnedArtifact {
    name: "CLIP tokenizer",
    sha256: "f7f3b7af117d467b58374797691a6438d3e6b9e9cef800dfd5dced7f697a90cd",
    size: 2_224_119,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn artifact_for(bytes: &[u8], sha256: &'static str) -> PinnedArtifact {
        PinnedArtifact {
            name: "test artifact",
            sha256,
            size: bytes.len() as u64,
        }
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("wanderer-integrity-{}", name));
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    // Empty input has a well known digest, which keeps this test independent of the
    // implementation it is checking.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_match_the_known_digest_for_empty_input() {
        let path = write_temp("empty", b"");
        assert_eq!(sha256_file(&path).unwrap(), EMPTY_SHA256);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn accepts_the_exact_bytes() {
        let path = write_temp("exact", b"");
        assert!(artifact_for(b"", EMPTY_SHA256).verify(&path).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_a_wrong_digest_at_the_right_length() {
        let bytes = b"not empty";
        let path = write_temp("wrong-digest", bytes);
        let artifact = artifact_for(bytes, EMPTY_SHA256);
        let err = artifact.verify(&path).unwrap_err();
        assert!(err.contains("failed its integrity check"), "{}", err);
        let _ = std::fs::remove_file(path);
    }

    // The size check exists to fail fast on a truncated download, so it should report
    // the length rather than hashing a file it already knows is wrong.
    #[test]
    fn rejects_a_wrong_length_before_hashing() {
        let path = write_temp("wrong-length", b"short");
        let artifact = PinnedArtifact {
            name: "test artifact",
            sha256: EMPTY_SHA256,
            size: 999,
        };
        let err = artifact.verify(&path).unwrap_err();
        assert!(err.contains("expected 999"), "{}", err);
        let _ = std::fs::remove_file(path);
    }

    // A rejected download must not survive to be mistaken for a present model.
    #[test]
    fn verify_or_remove_deletes_a_mismatched_file() {
        let path = write_temp("removed", b"not empty");
        let artifact = artifact_for(b"not empty", EMPTY_SHA256);
        assert!(artifact.verify_or_remove(&path).is_err());
        assert!(!path.exists(), "the rejected file should have been removed");
    }

    #[test]
    fn missing_files_are_an_error_not_a_pass() {
        let path = std::env::temp_dir().join("wanderer-integrity-absent");
        let _ = std::fs::remove_file(&path);
        assert!(ARCFACE.verify(&path).is_err());
    }
}
