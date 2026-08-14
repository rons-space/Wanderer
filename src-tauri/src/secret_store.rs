//! Where a secret goes when it has to survive a restart.
//!
//! The MTProto authorization key is full access to the user's Telegram account: with
//! it, someone can read every conversation and send as them. It was sitting in
//! `session.db` as plaintext SQLite while the far less sensitive `api_id`/`api_hash`
//! pair got DPAPI, which is the wrong way round.
//!
//! There is no single answer that works everywhere, so this is explicit about which
//! protection is in use rather than pretending otherwise:
//!
//! - **Windows** has DPAPI, which derives a key from the user's logon credentials.
//!   Nothing has to be unlocked first, so the app can reconnect at startup exactly as
//!   it does today. This is the platform the application ships on.
//! - **Everywhere else** the secret is wrapped with the library master key. That is
//!   real protection, and it costs one thing: the vault must be unlocked before
//!   Telegram can connect, because until then there is no key to unwrap with.
//! - **An unencrypted library on a non-Windows platform has nothing to protect with.**
//!   No passphrase exists, and inventing a key means storing that key next to what it
//!   protects, which is obfuscation sold as encryption. This refuses instead, and the
//!   caller reports why.
//!
//! An OS keychain (macOS Keychain Services, freedesktop Secret Service) would remove
//! the unlock requirement on those platforms, and this enum is the seam where it
//! would go. It is deliberately not here yet: Secret Service is a desktop-session
//! daemon rather than part of Linux, so it is absent on minimal installs, containers
//! and headless machines, and the master key path has to exist anyway for those.

use anyhow::{anyhow, Result};

use crate::security::MasterKey;

/// Secondary entropy for DPAPI, mixed in alongside the user's logon secret.
///
/// Without it, any process running as the same user can call `CryptUnprotectData` on
/// a stolen blob and get the plaintext back. It is a constant rather than a secret,
/// so it does not stop an attacker who also reads this source; what it stops is the
/// blob being usable by generic credential-stealing tooling that knows nothing about
/// this application.
#[cfg(target_os = "windows")]
const SESSION_ENTROPY: &[u8] = b"wanderer/telegram-session/v1";

/// How a secret is protected at rest.
///
/// `Debug` is safe to derive: `MasterKey` redacts itself, so the variant name is all
/// that ever reaches a log line.
#[derive(Debug)]
pub enum SecretStore {
    /// Windows DPAPI, scoped to the current user, with secondary entropy.
    #[cfg(target_os = "windows")]
    Dpapi,
    /// AES-256-GCM under the library master key.
    MasterKey(MasterKey),
}

impl SecretStore {
    /// Pick the protection for this platform and library state.
    ///
    /// `master_key` is whatever the vault currently holds: `None` when the library is
    /// unencrypted or still locked.
    pub fn for_session(master_key: Option<MasterKey>) -> Result<Self> {
        #[cfg(target_os = "windows")]
        {
            // DPAPI regardless of the vault: it does not depend on anything the user
            // has to unlock first, so Telegram can reconnect at startup.
            let _ = master_key;
            Ok(SecretStore::Dpapi)
        }

        #[cfg(not(target_os = "windows"))]
        {
            match master_key {
                Some(key) => Ok(SecretStore::MasterKey(key)),
                None => Err(anyhow!(
                    "Cannot protect the Telegram session on this platform without an \
                     unlocked encrypted library. Enable encryption and unlock it, or \
                     sign in again after unlocking."
                )),
            }
        }
    }

    /// A short name for logs and error messages, so support questions can be answered
    /// without guessing which branch a machine took.
    pub fn describe(&self) -> &'static str {
        match self {
            #[cfg(target_os = "windows")]
            SecretStore::Dpapi => "Windows DPAPI (user-scoped, with entropy)",
            SecretStore::MasterKey(_) => "library master key",
        }
    }

    pub fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        match self {
            #[cfg(target_os = "windows")]
            SecretStore::Dpapi => crate::security::dpapi_protect_with_entropy(
                plaintext,
                "Wanderer Telegram session",
                SESSION_ENTROPY,
            ),
            SecretStore::MasterKey(key) => {
                // The same stream format the media files use, so there is one
                // authenticated container in this codebase rather than two.
                let mut sealed = Vec::new();
                crate::security::encrypt_stream(&mut &plaintext[..], &mut sealed, key)?;
                Ok(sealed)
            }
        }
    }

    pub fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>> {
        match self {
            #[cfg(target_os = "windows")]
            SecretStore::Dpapi => {
                crate::security::dpapi_unprotect_with_entropy(blob, SESSION_ENTROPY)
            }
            SecretStore::MasterKey(key) => {
                let mut plaintext = Vec::new();
                crate::security::decrypt_stream(&mut &blob[..], &mut plaintext, key)?;
                Ok(plaintext)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> MasterKey {
        MasterKey::new([9u8; 32])
    }

    #[test]
    fn a_master_key_store_round_trips() {
        let store = SecretStore::MasterKey(test_key());
        let sealed = store.protect(b"auth key bytes").expect("protect");
        assert_ne!(sealed, b"auth key bytes", "the secret was stored verbatim");
        assert_eq!(
            store.unprotect(&sealed).expect("unprotect"),
            b"auth key bytes"
        );
    }

    #[test]
    fn another_key_cannot_open_it() {
        let store = SecretStore::MasterKey(test_key());
        let sealed = store.protect(b"auth key bytes").expect("protect");

        let other = SecretStore::MasterKey(MasterKey::new([1u8; 32]));
        assert!(other.unprotect(&sealed).is_err());
    }

    /// Tampering has to fail rather than yield something: this blob decides whether
    /// the app talks to Telegram as the right account.
    #[test]
    fn a_modified_blob_is_rejected() {
        let store = SecretStore::MasterKey(test_key());
        let mut sealed = store.protect(b"auth key bytes").expect("protect");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(store.unprotect(&sealed).is_err());
    }

    /// The honest failure: no OS store and no master key means no protection, and
    /// saying so is better than writing plaintext and calling it protected.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn an_unencrypted_library_off_windows_refuses_rather_than_pretending() {
        let err = SecretStore::for_session(None).expect_err("must refuse");
        assert!(err.to_string().contains("unlocked encrypted library"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn an_unlocked_library_off_windows_uses_the_master_key() {
        let store = SecretStore::for_session(Some(test_key())).expect("store");
        assert_eq!(store.describe(), "library master key");
    }
}
