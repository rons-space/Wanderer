//! Self-contained encrypted database backups.
//!
//! The master key is random and is only recoverable by unwrapping it with the
//! passphrase or the recovery key, and the only copy of that wrap lives in the
//! `config` table inside `library.db`. Encrypting a copy of `library.db` with
//! the master key therefore used to produce an artifact that sealed in the only
//! key material capable of opening it: with the database lost, the passphrase
//! and the recovery key were both useless, and so was every media blob in
//! Telegram, because they share that key.
//!
//! The fix is to carry the `SecurityBundle` alongside the ciphertext, in the
//! clear. That is safe: the bundle is exactly the Argon2id-wrapped key material
//! that already sits at rest on disk, and it is worthless without one of the two
//! secrets the user holds. It is written as a plaintext header rather than a
//! sidecar file on purpose, because a sidecar is the thing users lose when they
//! copy "the backup" to a new machine or upload it to Telegram.
//!
//! ```text
//! "WBAK01" | u8 version | u32 header_len (LE) | header JSON | WBENC1 stream
//! ```

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::security::{self, SecurityBundle};

const BACKUP_MAGIC: &[u8; 6] = b"WBAK01";
const BACKUP_FORMAT_VERSION: u8 = 1;

/// Upper bound on the JSON header, so a corrupt or hostile length field cannot
/// drive an unbounded allocation before anything has been authenticated.
const MAX_HEADER_LEN: u32 = 1024 * 1024;

/// The first 16 bytes of any SQLite database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Plaintext metadata carried at the front of an encrypted backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupHeader {
    pub format_version: u8,
    pub created_at: i64,
    pub app_version: String,
    /// Name of the file that was encrypted, for forward compatibility if a
    /// backup ever carries something other than `library.db`.
    pub source_file: String,
    /// The wrapped master key and its Argon2id salts. Useless without the
    /// passphrase or the recovery key.
    pub bundle: SecurityBundle,
}

/// Which of the user's two secrets is being used to open a backup.
pub enum BackupSecret<'a> {
    Passphrase(&'a str),
    RecoveryKey(&'a str),
}

/// True when `path` starts with the backup envelope magic.
// Only the tests call it: restore takes the path the user picked and fails on a bad
// header rather than probing first. Kept so the check exists for the file picker.
#[allow(dead_code)]
pub fn is_backup_envelope(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 6];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(&magic == BACKUP_MAGIC),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Write `source_db` to `out_path` as an encrypted backup with a plaintext
/// header carrying `bundle`.
pub fn write_encrypted_backup(
    source_db: &Path,
    out_path: &Path,
    bundle: &SecurityBundle,
    key: &[u8; 32],
    app_version: &str,
) -> Result<()> {
    let header = BackupHeader {
        format_version: BACKUP_FORMAT_VERSION,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        app_version: app_version.to_string(),
        source_file: source_db
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "library.db".to_string()),
        bundle: bundle.clone(),
    };
    let header_json = serde_json::to_vec(&header)?;
    if header_json.len() as u64 > MAX_HEADER_LEN as u64 {
        return Err(anyhow!("Backup header is implausibly large"));
    }

    let input = File::open(source_db).with_context(|| {
        format!(
            "Failed to open database for backup: {}",
            source_db.display()
        )
    })?;
    let mut reader = BufReader::new(input);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = File::create(out_path)
        .with_context(|| format!("Failed to create backup file: {}", out_path.display()))?;
    let mut writer = BufWriter::new(output);

    writer.write_all(BACKUP_MAGIC)?;
    writer.write_all(&[BACKUP_FORMAT_VERSION])?;
    writer.write_all(&(header_json.len() as u32).to_le_bytes())?;
    writer.write_all(&header_json)?;

    security::encrypt_stream(&mut reader, &mut writer, key)?;
    writer.flush()?;
    Ok(())
}

/// Read the plaintext header. Requires no secret, which is the whole point:
/// a recovery tool can inspect what it is holding before asking for one.
pub fn read_header(path: &Path) -> Result<BackupHeader> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open backup file: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    read_header_from(&mut reader)
}

fn read_header_from<R: Read>(reader: &mut R) -> Result<BackupHeader> {
    let mut magic = [0u8; 6];
    reader.read_exact(&mut magic)?;
    if &magic != BACKUP_MAGIC {
        return Err(anyhow!(
            "Not a Wanderer backup archive. Backups created before the archive \
             format was introduced carry no key material and cannot be restored."
        ));
    }

    let mut version = [0u8; 1];
    reader.read_exact(&mut version)?;
    if version[0] != BACKUP_FORMAT_VERSION {
        return Err(anyhow!("Unsupported backup format version: {}", version[0]));
    }

    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let header_len = u32::from_le_bytes(len_buf);
    if header_len == 0 || header_len > MAX_HEADER_LEN {
        return Err(anyhow!("Invalid backup header length"));
    }

    let mut header_bytes = vec![0u8; header_len as usize];
    reader.read_exact(&mut header_bytes)?;
    let header: BackupHeader =
        serde_json::from_slice(&header_bytes).context("Malformed backup header")?;
    Ok(header)
}

/// Decrypt `archive` to `out_db` using one of the user's secrets.
///
/// The output is written to a temporary file and renamed only after the SQLite
/// magic has been confirmed, so a failed restore never leaves a half-written
/// file where a database is expected.
pub fn restore_encrypted_backup(
    archive: &Path,
    out_db: &Path,
    secret: BackupSecret<'_>,
) -> Result<BackupHeader> {
    let file = File::open(archive)
        .with_context(|| format!("Failed to open backup file: {}", archive.display()))?;
    let mut reader = BufReader::new(file);
    let header = read_header_from(&mut reader)?;

    let key = match secret {
        BackupSecret::Passphrase(p) => header.bundle.unlock_with_passphrase(p),
        BackupSecret::RecoveryKey(k) => header.bundle.unlock_with_recovery_key(k),
    }
    .context("Could not unwrap the master key from this backup")?;

    if let Some(parent) = out_db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = out_db.with_extension("restore-partial");
    {
        let output = File::create(&staging).with_context(|| {
            format!(
                "Failed to create restore staging file: {}",
                staging.display()
            )
        })?;
        let mut writer = BufWriter::new(output);
        if let Err(e) = security::decrypt_stream(&mut reader, &mut writer, &key) {
            drop(writer);
            let _ = std::fs::remove_file(&staging);
            return Err(e);
        }
        writer.flush()?;
    }

    if let Err(e) = assert_sqlite_file(&staging) {
        let _ = std::fs::remove_file(&staging);
        return Err(e);
    }

    std::fs::rename(&staging, out_db)
        .with_context(|| format!("Failed to move restored database to {}", out_db.display()))?;
    Ok(header)
}

fn assert_sqlite_file(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 16];
    file.read_exact(&mut magic)
        .map_err(|_| anyhow!("Restored file is too short to be a database"))?;
    if &magic != SQLITE_MAGIC {
        return Err(anyhow!(
            "Restored file is not a SQLite database. The backup may be corrupt."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A byte pattern that starts with the SQLite magic, standing in for a real
    /// database file. Larger than one AES-GCM chunk would be pointless here and
    /// slow; chunking itself is covered by the security module's own tests.
    fn fake_db_bytes() -> Vec<u8> {
        let mut bytes = SQLITE_MAGIC.to_vec();
        bytes.extend((0u16..4096).flat_map(|n| n.to_le_bytes()));
        bytes
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("wanderer-backup-test-{pid}-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a backup exactly as the app would, then forget everything except
    /// the artifact: this is the "the drive died, I reinstalled" starting point.
    fn make_backup(dir: &TempDir) -> (std::path::PathBuf, Vec<u8>, String, String) {
        let passphrase = "correct horse battery".to_string();
        let (bundle, recovery_key, key) = SecurityBundle::new_encrypted(&passphrase).unwrap();

        let db_path = dir.path("library.db");
        let contents = fake_db_bytes();
        std::fs::write(&db_path, &contents).unwrap();

        let archive = dir.path("library_backup.wbak");
        write_encrypted_backup(&db_path, &archive, &bundle, &key, "test").unwrap();

        // The source database is gone from here on, which is the entire point.
        std::fs::remove_file(&db_path).unwrap();

        (archive, contents, passphrase, recovery_key)
    }

    #[test]
    fn restores_with_only_the_passphrase() {
        let dir = TempDir::new();
        let (archive, contents, passphrase, _recovery) = make_backup(&dir);

        let out = dir.path("restored.db");
        let header =
            restore_encrypted_backup(&archive, &out, BackupSecret::Passphrase(&passphrase))
                .unwrap();

        assert_eq!(header.format_version, BACKUP_FORMAT_VERSION);
        assert_eq!(std::fs::read(&out).unwrap(), contents);
    }

    #[test]
    fn restores_with_only_the_recovery_key() {
        let dir = TempDir::new();
        let (archive, contents, _passphrase, recovery) = make_backup(&dir);

        let out = dir.path("restored.db");
        restore_encrypted_backup(&archive, &out, BackupSecret::RecoveryKey(&recovery)).unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), contents);
    }

    #[test]
    fn header_is_readable_without_any_secret() {
        let dir = TempDir::new();
        let (archive, _contents, _passphrase, _recovery) = make_backup(&dir);

        let header = read_header(&archive).unwrap();
        assert!(header.bundle.passphrase_wrap.is_some());
        assert!(header.bundle.recovery.is_some());
        assert_eq!(header.source_file, "library.db");
        assert!(is_backup_envelope(&archive).unwrap());
    }

    #[test]
    fn wrong_passphrase_is_rejected_and_leaves_no_output() {
        let dir = TempDir::new();
        let (archive, _contents, _passphrase, _recovery) = make_backup(&dir);

        let out = dir.path("restored.db");
        assert!(
            restore_encrypted_backup(&archive, &out, BackupSecret::Passphrase("wrong")).is_err()
        );
        assert!(!out.exists());
        assert!(!out.with_extension("restore-partial").exists());
    }

    #[test]
    fn wrong_recovery_key_is_rejected() {
        let dir = TempDir::new();
        let (archive, _contents, _passphrase, _recovery) = make_backup(&dir);

        let out = dir.path("restored.db");
        assert!(
            restore_encrypted_backup(&archive, &out, BackupSecret::RecoveryKey("AAAAA-BBBBB"))
                .is_err()
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let dir = TempDir::new();
        let (archive, _contents, passphrase, _recovery) = make_backup(&dir);

        let mut bytes = std::fs::read(&archive).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&archive, &bytes).unwrap();

        let out = dir.path("restored.db");
        assert!(
            restore_encrypted_backup(&archive, &out, BackupSecret::Passphrase(&passphrase))
                .is_err()
        );
        assert!(!out.exists());
    }

    /// Truncation inside a chunk is caught by the chunk's authentication tag.
    /// Dropping whole trailing chunks is still undetectable, which is finding
    /// 1.4 and is tracked separately in #29.
    #[test]
    fn truncated_archive_is_rejected() {
        let dir = TempDir::new();
        let (archive, _contents, passphrase, _recovery) = make_backup(&dir);

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&archive)
            .unwrap();
        let len = file.seek(std::io::SeekFrom::End(0)).unwrap();
        file.set_len(len - 32).unwrap();
        drop(file);

        let out = dir.path("restored.db");
        assert!(
            restore_encrypted_backup(&archive, &out, BackupSecret::Passphrase(&passphrase))
                .is_err()
        );
    }

    #[test]
    fn a_plain_database_file_is_not_mistaken_for_an_archive() {
        let dir = TempDir::new();
        let plain = dir.path("library.db");
        std::fs::write(&plain, fake_db_bytes()).unwrap();

        assert!(!is_backup_envelope(&plain).unwrap());
        assert!(read_header(&plain).is_err());
    }
}
