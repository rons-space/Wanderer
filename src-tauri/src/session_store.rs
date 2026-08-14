//! An encrypted Telegram session.
//!
//! `SqliteSession` writes the MTProto authorization key into `session.db` as a
//! plaintext SQLite blob. That key is full access to the account: with a copy of the
//! file, someone can read every conversation and send as the user, without the
//! password and without triggering a login notification. It sat unprotected while the
//! `api_id`/`api_hash` pair, which is public in every open source Telegram client,
//! was DPAPI-wrapped.
//!
//! This keeps the session in memory and persists the part that matters through
//! [`SecretStore`], so what lands on disk is a sealed blob.
//!
//! **Only the datacenter options and the home datacenter are persisted.** Those carry
//! the authorization keys, which are what a restart cannot rebuild without a fresh
//! login, and logging in again is expensive in flood-wait terms. The peer cache and
//! the update state are deliberately dropped on exit: they are conveniences rather
//! than secrets, this application does not act on Telegram updates (its update
//! handler logs and nothing more) and does its own scanning through the sync worker,
//! and persisting them would mean re-sealing the blob on a hot path, on every cached
//! peer, to protect data that is not sensitive. The cost is a cold peer cache after a
//! restart.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use grammers_session::storages::MemorySession;
use grammers_session::types::{DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::{Session, SessionData};
use serde::{Deserialize, Serialize};

use crate::secret_store::SecretStore;

/// The file the sealed session lives in, next to the library.
pub const SESSION_FILE: &str = "session.enc";

/// The plaintext session this replaces. Read once, to migrate, then deleted.
pub const LEGACY_SESSION_FILE: &str = "session.db";

/// Authorization keys are exactly this long, and a blob claiming otherwise is
/// corrupt rather than something to hand to the MTProto layer.
const AUTH_KEY_LEN: usize = 256;

#[derive(Serialize, Deserialize)]
struct PersistedDcOption {
    id: i32,
    ipv4: String,
    ipv6: String,
    /// `None` before a key has been generated for this datacenter.
    auth_key: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    /// Version of this blob's own layout, so a future change can be read rather than
    /// guessed at. The protection around it is versioned separately by whatever
    /// `SecretStore` wrote it.
    version: u8,
    home_dc: i32,
    dc_options: Vec<PersistedDcOption>,
}

const PERSISTED_VERSION: u8 = 1;

/// A [`Session`] that keeps everything in memory and seals the authorization keys to
/// disk whenever they change.
pub struct EncryptedSession {
    inner: MemorySession,
    store: SecretStore,
    path: PathBuf,
    /// Serialises writers, so two datacenter updates cannot interleave into a
    /// half-written blob. The `Session` trait is synchronous, so this is a plain
    /// `Mutex` and every critical section is a small serialise-seal-write.
    write_lock: Mutex<()>,
}

impl EncryptedSession {
    /// Open the sealed session at `app_data_dir`, migrating a legacy plaintext
    /// `session.db` if one is there and no sealed session is.
    ///
    /// A missing session is not an error: that is a first run, or the state right
    /// after a logout.
    pub fn open(app_data_dir: &Path, store: SecretStore) -> Result<Self> {
        let path = app_data_dir.join(SESSION_FILE);
        let legacy = app_data_dir.join(LEGACY_SESSION_FILE);

        let data = if path.exists() {
            let blob = std::fs::read(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let plaintext = store.unprotect(&blob).context(
                "Could not open the stored Telegram session. If the library was \
                 re-encrypted or moved to another machine, sign in to Telegram again.",
            )?;
            let persisted: PersistedSession = serde_json::from_slice(&plaintext)
                .context("The stored Telegram session is not readable")?;
            Some(session_data_from(persisted)?)
        } else if legacy.exists() {
            log::info!(
                "Migrating the plaintext Telegram session at {} into a sealed session",
                legacy.display()
            );
            Some(read_legacy_session(&legacy)?)
        } else {
            None
        };

        let session = Self {
            inner: data.map(MemorySession::from).unwrap_or_default(),
            store,
            path,
            write_lock: Mutex::new(()),
        };

        // Only once the sealed copy is safely written is the plaintext removed. The
        // order matters: the other way round, a failure here logs the user out.
        if legacy.exists() {
            session.persist()?;
            std::fs::remove_file(&legacy).with_context(|| {
                format!(
                    "Sealed the Telegram session but could not remove the plaintext \
                     file at {}. Delete it by hand: it still contains the account key.",
                    legacy.display()
                )
            })?;
            log::info!("Removed the plaintext Telegram session file");
        }

        Ok(session)
    }

    /// Delete both the sealed session and any legacy plaintext one.
    pub fn remove(app_data_dir: &Path) -> Result<()> {
        for name in [SESSION_FILE, LEGACY_SESSION_FILE] {
            let path = app_data_dir.join(name);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("Failed to delete {}", path.display()))?;
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> PersistedSession {
        let data = SessionData::from(&self.inner as &dyn Session);
        let mut dc_options: Vec<PersistedDcOption> = data
            .dc_options
            .values()
            .map(|dc| PersistedDcOption {
                id: dc.id,
                ipv4: dc.ipv4.to_string(),
                ipv6: dc.ipv6.to_string(),
                auth_key: dc.auth_key.map(|k| k.to_vec()),
            })
            .collect();
        // Sorted so the same state produces the same plaintext, which keeps a
        // rewrite that changes nothing from looking like a change on disk.
        dc_options.sort_by_key(|dc| dc.id);

        PersistedSession {
            version: PERSISTED_VERSION,
            home_dc: data.home_dc,
            dc_options,
        }
    }

    fn persist(&self) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let plaintext = serde_json::to_vec(&self.snapshot())?;
        let sealed = self.store.protect(&plaintext)?;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Written beside the target and renamed over it. A crash mid-write would
        // otherwise leave a truncated blob that fails to authenticate, and the user
        // would be silently signed out.
        let tmp = self.path.with_extension("enc.tmp");
        std::fs::write(&tmp, &sealed)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("Failed to replace {}", self.path.display()))?;
        Ok(())
    }

    /// Persist, logging rather than propagating.
    ///
    /// The `Session` trait is infallible by design, because the client has nowhere to
    /// put an error from a storage callback. A failed write means the session is
    /// still correct in memory and this run keeps working; the user pays with a login
    /// next time, which is better than a panic inside the network loop.
    fn persist_or_log(&self) {
        if let Err(e) = self.persist() {
            log::error!("Failed to persist the Telegram session: {:#}", e);
        }
    }
}

fn session_data_from(persisted: PersistedSession) -> Result<SessionData> {
    if persisted.version != PERSISTED_VERSION {
        return Err(anyhow!(
            "Unsupported stored session version: {}",
            persisted.version
        ));
    }

    // Starts from the default, so the statically-known datacenters are present even
    // if the blob only carries the one that was in use.
    let mut data = SessionData::default();
    data.home_dc = persisted.home_dc;

    for dc in persisted.dc_options {
        let auth_key = match dc.auth_key {
            Some(bytes) => {
                let len = bytes.len();
                Some(<[u8; AUTH_KEY_LEN]>::try_from(bytes).map_err(|_| {
                    anyhow!(
                        "Stored authorization key for datacenter {} is {} bytes, expected {}",
                        dc.id,
                        len,
                        AUTH_KEY_LEN
                    )
                })?)
            }
            None => None,
        };
        data.dc_options.insert(
            dc.id,
            DcOption {
                id: dc.id,
                ipv4: dc
                    .ipv4
                    .parse()
                    .with_context(|| format!("Invalid stored IPv4 for datacenter {}", dc.id))?,
                ipv6: dc
                    .ipv6
                    .parse()
                    .with_context(|| format!("Invalid stored IPv6 for datacenter {}", dc.id))?,
                auth_key,
            },
        );
    }

    Ok(data)
}

/// Read the two tables that carry the account key out of a plaintext `session.db`.
///
/// Only `dc_home` and `dc_option` are read. The peer cache and update state in that
/// file are not carried over for the same reason they are not persisted going
/// forward.
fn read_legacy_session(path: &Path) -> Result<SessionData> {
    let conn = rusqlite::Connection::open(path)
        .with_context(|| format!("Failed to open the legacy session at {}", path.display()))?;

    let mut data = SessionData::default();

    if let Ok(home) = conn.query_row("SELECT dc_id FROM dc_home LIMIT 1", [], |row| {
        row.get::<_, i32>(0)
    }) {
        data.home_dc = home;
    }

    let mut stmt = conn.prepare("SELECT dc_id, ipv4, ipv6, auth_key FROM dc_option")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<Vec<u8>>>(3)?,
        ))
    })?;

    for row in rows {
        let (id, ipv4, ipv6, auth_key) = row?;
        // A malformed row in a file the user already had is not worth refusing to
        // start over: skip it and let that datacenter fall back to its default.
        let (Ok(ipv4), Ok(ipv6)) = (ipv4.parse(), ipv6.parse()) else {
            log::warn!(
                "Skipping legacy datacenter {} with an unparseable address",
                id
            );
            continue;
        };
        let auth_key = match auth_key {
            Some(bytes) => match <[u8; AUTH_KEY_LEN]>::try_from(bytes) {
                Ok(key) => Some(key),
                Err(_) => {
                    log::warn!("Skipping legacy datacenter {} with a malformed key", id);
                    continue;
                }
            },
            None => None,
        };
        data.dc_options.insert(
            id,
            DcOption {
                id,
                ipv4,
                ipv6,
                auth_key,
            },
        );
    }

    Ok(data)
}

impl Session for EncryptedSession {
    fn home_dc_id(&self) -> i32 {
        self.inner.home_dc_id()
    }

    fn set_home_dc_id(&self, dc_id: i32) {
        self.inner.set_home_dc_id(dc_id);
        self.persist_or_log();
    }

    fn dc_option(&self, dc_id: i32) -> Option<DcOption> {
        self.inner.dc_option(dc_id)
    }

    fn set_dc_option(&self, dc_option: &DcOption) {
        self.inner.set_dc_option(dc_option);
        // The authorization key changes here and nowhere else, so this is the write
        // that matters and it happens rarely: at login, and on a datacenter migration.
        self.persist_or_log();
    }

    fn peer(&self, peer: PeerId) -> Option<PeerInfo> {
        self.inner.peer(peer)
    }

    fn cache_peer(&self, peer: &PeerInfo) {
        // In memory only. See the module comment: this is a cache, not a secret, and
        // it is written often enough that sealing it would be a hot path.
        self.inner.cache_peer(peer);
    }

    fn updates_state(&self) -> UpdatesState {
        self.inner.updates_state()
    }

    fn set_update_state(&self, update: UpdateState) {
        self.inner.set_update_state(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::MasterKey;
    use std::net::{SocketAddrV4, SocketAddrV6};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wanderer-session-{}-{}",
                name,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> SecretStore {
        SecretStore::MasterKey(MasterKey::new([3u8; 32]))
    }

    fn dc_with_key(id: i32, byte: u8) -> DcOption {
        DcOption {
            id,
            ipv4: "149.154.167.51:443".parse::<SocketAddrV4>().unwrap(),
            ipv6: "[2001:67c:4e8:f002::a]:443"
                .parse::<SocketAddrV6>()
                .unwrap(),
            auth_key: Some([byte; AUTH_KEY_LEN]),
        }
    }

    #[test]
    fn an_authorization_key_survives_a_restart_and_is_never_on_disk_in_the_clear() {
        let dir = TempDir::new("roundtrip");
        {
            let session = EncryptedSession::open(&dir.0, store()).expect("open");
            session.set_home_dc_id(4);
            session.set_dc_option(&dc_with_key(4, 0xAB));
        }

        let blob = std::fs::read(dir.0.join(SESSION_FILE)).expect("sealed file");
        assert!(
            !blob.windows(32).any(|w| w == [0xABu8; 32]),
            "the authorization key is readable in the file on disk"
        );

        let reopened = EncryptedSession::open(&dir.0, store()).expect("reopen");
        assert_eq!(reopened.home_dc_id(), 4);
        assert_eq!(
            reopened.dc_option(4).and_then(|dc| dc.auth_key),
            Some([0xAB; AUTH_KEY_LEN])
        );
    }

    #[test]
    fn a_session_sealed_with_another_key_does_not_open() {
        let dir = TempDir::new("wrongkey");
        {
            let session = EncryptedSession::open(&dir.0, store()).expect("open");
            session.set_dc_option(&dc_with_key(2, 0x11));
        }

        let other = SecretStore::MasterKey(MasterKey::new([7u8; 32]));
        let err = EncryptedSession::open(&dir.0, other).expect_err("must not open");
        assert!(
            err.to_string().contains("sign in to Telegram again"),
            "unexpected error: {:#}",
            err
        );
    }

    /// The upgrade path. A user with an existing login must not be signed out, and
    /// the plaintext file must not survive the migration.
    #[test]
    fn a_legacy_plaintext_session_is_migrated_and_deleted() {
        let dir = TempDir::new("legacy");
        let legacy = dir.0.join(LEGACY_SESSION_FILE);
        {
            let conn = rusqlite::Connection::open(&legacy).unwrap();
            conn.execute_batch(
                "CREATE TABLE dc_home (dc_id INTEGER NOT NULL, PRIMARY KEY(dc_id));
                 CREATE TABLE dc_option (
                     dc_id INTEGER NOT NULL,
                     ipv4 TEXT NOT NULL,
                     ipv6 TEXT NOT NULL,
                     auth_key BLOB,
                     PRIMARY KEY (dc_id));
                 INSERT INTO dc_home VALUES (2);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO dc_option VALUES (2, '149.154.167.51:443', '[2001:67c:4e8:f002::a]:443', ?1)",
                [vec![0xCDu8; AUTH_KEY_LEN]],
            )
            .unwrap();
        }

        let session = EncryptedSession::open(&dir.0, store()).expect("migrate");
        assert_eq!(session.home_dc_id(), 2);
        assert_eq!(
            session.dc_option(2).and_then(|dc| dc.auth_key),
            Some([0xCD; AUTH_KEY_LEN])
        );
        assert!(!legacy.exists(), "the plaintext session file survived");
        assert!(dir.0.join(SESSION_FILE).exists(), "nothing was sealed");
    }

    #[test]
    fn a_first_run_with_no_session_is_not_an_error() {
        let dir = TempDir::new("firstrun");
        let session = EncryptedSession::open(&dir.0, store()).expect("open");
        // The statically-known datacenters are still there to connect with.
        assert!(session.dc_option(2).is_some());
        assert!(!dir.0.join(SESSION_FILE).exists());
    }

    #[test]
    fn removing_a_session_clears_both_files() {
        let dir = TempDir::new("remove");
        std::fs::write(dir.0.join(SESSION_FILE), b"sealed").unwrap();
        std::fs::write(dir.0.join(LEGACY_SESSION_FILE), b"plaintext").unwrap();

        EncryptedSession::remove(&dir.0).expect("remove");

        assert!(!dir.0.join(SESSION_FILE).exists());
        assert!(!dir.0.join(LEGACY_SESSION_FILE).exists());
        // Removing again is what a second logout does, and must not fail.
        EncryptedSession::remove(&dir.0).expect("idempotent");
    }
}
