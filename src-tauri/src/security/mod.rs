use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_MAGIC: &[u8; 6] = b"WBENC1";
const FILE_VERSION: u8 = 1;
const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024; // 1MB

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMode {
    Unencrypted,
    Encrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedMasterKey {
    pub salt_b64: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryData {
    pub verifier_phc: String,
    pub wrap: WrappedMasterKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityBundle {
    pub mode: EncryptionMode,
    pub key_id: String,
    pub created_at: i64,
    pub passphrase_wrap: Option<WrappedMasterKey>,
    pub recovery: Option<RecoveryData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramApiCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationStatus {
    pub running: bool,
    pub total: i64,
    pub processed: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub last_error: Option<String>,
}

impl Default for MigrationStatus {
    fn default() -> Self {
        Self {
            running: false,
            total: 0,
            processed: 0,
            succeeded: 0,
            failed: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct RuntimeState {
    pub master_key: Option<[u8; 32]>,
    pub migration: MigrationStatus,
    pub migration_worker_active: bool,
}

impl SecurityBundle {
    pub fn unencrypted() -> Self {
        Self {
            mode: EncryptionMode::Unencrypted,
            key_id: String::new(),
            created_at: unix_ts(),
            passphrase_wrap: None,
            recovery: None,
        }
    }

    pub fn new_encrypted(passphrase: &str) -> Result<(Self, String, [u8; 32])> {
        if passphrase.trim().len() < 8 {
            return Err(anyhow!("Passphrase must be at least 8 characters"));
        }

        let mut master_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut master_key);

        let passphrase_wrap = wrap_master_key_with_secret(passphrase.as_bytes(), &master_key)?;
        let recovery_key = generate_recovery_key();
        let recovery_wrap = wrap_master_key_with_secret(recovery_key.as_bytes(), &master_key)?;
        let verifier_phc = hash_recovery_key(&recovery_key)?;

        let mut key_id_bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut key_id_bytes);
        let key_id = B64.encode(key_id_bytes);

        Ok((
            Self {
                mode: EncryptionMode::Encrypted,
                key_id,
                created_at: unix_ts(),
                passphrase_wrap: Some(passphrase_wrap),
                recovery: Some(RecoveryData {
                    verifier_phc,
                    wrap: recovery_wrap,
                }),
            },
            recovery_key,
            master_key,
        ))
    }

    pub fn unlock_with_passphrase(&self, passphrase: &str) -> Result<[u8; 32]> {
        if self.mode != EncryptionMode::Encrypted {
            return Err(anyhow!("Encryption mode is not enabled"));
        }
        let wrapped = self
            .passphrase_wrap
            .as_ref()
            .ok_or_else(|| anyhow!("Missing passphrase key wrap"))?;
        unwrap_master_key_with_secret(passphrase.as_bytes(), wrapped)
    }

    /// Unwrap the master key with the recovery key, without rewrapping anything.
    ///
    /// Restoring a backup needs the key but has nowhere to persist a new wrap
    /// (the config table it would be written to is inside the artifact being
    /// restored), so this is deliberately separate from `recover_and_rewrap`.
    pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<[u8; 32]> {
        if self.mode != EncryptionMode::Encrypted {
            return Err(anyhow!("Encryption mode is not enabled"));
        }
        let recovery = self
            .recovery
            .as_ref()
            .ok_or_else(|| anyhow!("Missing recovery data"))?;

        if !verify_recovery_key(recovery_key, &recovery.verifier_phc)? {
            return Err(anyhow!("Invalid recovery key"));
        }

        unwrap_master_key_with_secret(recovery_key.as_bytes(), &recovery.wrap)
    }

    pub fn recover_and_rewrap(
        &self,
        recovery_key: &str,
        new_passphrase: &str,
    ) -> Result<(Self, [u8; 32])> {
        let master_key = self.unlock_with_recovery_key(recovery_key)?;
        let passphrase_wrap = wrap_master_key_with_secret(new_passphrase.as_bytes(), &master_key)?;

        let mut next = self.clone();
        next.passphrase_wrap = Some(passphrase_wrap);
        Ok((next, master_key))
    }

    pub fn regenerate_recovery_key(&self, passphrase: &str) -> Result<(Self, String, [u8; 32])> {
        let master_key = self.unlock_with_passphrase(passphrase)?;
        let new_recovery_key = generate_recovery_key();
        let wrap = wrap_master_key_with_secret(new_recovery_key.as_bytes(), &master_key)?;
        let verifier_phc = hash_recovery_key(&new_recovery_key)?;
        let mut next = self.clone();
        next.recovery = Some(RecoveryData { verifier_phc, wrap });
        Ok((next, new_recovery_key, master_key))
    }
}

fn unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn argon2id_params() -> Result<Argon2<'static>> {
    let params = Params::new(65_536, 3, 1, Some(32))
        .map_err(|e| anyhow!("Failed to build Argon2 params: {}", e))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn derive_secret_key(secret: &[u8], salt: &[u8; 16]) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    let argon2 = argon2id_params()?;
    argon2
        .hash_password_into(secret, salt, &mut out)
        .map_err(|e| anyhow!("Argon2 derivation failed: {}", e))?;
    Ok(out)
}

fn wrap_master_key_with_secret(secret: &[u8], master_key: &[u8; 32]) -> Result<WrappedMasterKey> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let derived_key = derive_secret_key(secret, &salt)?;

    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived_key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), master_key.as_slice())
        .map_err(|_| anyhow!("Failed to wrap master key"))?;

    Ok(WrappedMasterKey {
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce),
        ciphertext_b64: B64.encode(ciphertext),
    })
}

fn unwrap_master_key_with_secret(secret: &[u8], wrapped: &WrappedMasterKey) -> Result<[u8; 32]> {
    let salt_vec = B64
        .decode(&wrapped.salt_b64)
        .context("Invalid wrapped key salt encoding")?;
    if salt_vec.len() != 16 {
        return Err(anyhow!("Invalid wrapped key salt length"));
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&salt_vec);

    let nonce_vec = B64
        .decode(&wrapped.nonce_b64)
        .context("Invalid wrapped key nonce encoding")?;
    if nonce_vec.len() != 12 {
        return Err(anyhow!("Invalid wrapped key nonce length"));
    }

    let ciphertext = B64
        .decode(&wrapped.ciphertext_b64)
        .context("Invalid wrapped key ciphertext encoding")?;

    let derived_key = derive_secret_key(secret, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived_key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce_vec), ciphertext.as_ref())
        .map_err(|_| anyhow!("Failed to unwrap key. Secret may be invalid"))?;

    if plaintext.len() != 32 {
        return Err(anyhow!("Invalid unwrapped master key length"));
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(&plaintext);
    Ok(out)
}

fn hash_recovery_key(recovery_key: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = argon2id_params()?;
    argon2
        .hash_password(recovery_key.trim().as_bytes(), &salt)
        .map(|phc| phc.to_string())
        .map_err(|e| anyhow!("Failed to hash recovery key: {}", e))
}

fn verify_recovery_key(recovery_key: &str, verifier_phc: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(verifier_phc).map_err(|e| anyhow!("Invalid verifier hash: {}", e))?;
    let argon2 = argon2id_params()?;
    Ok(argon2
        .verify_password(recovery_key.trim().as_bytes(), &parsed)
        .is_ok())
}

fn generate_recovery_key() -> String {
    let mut raw = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let hex = hex::encode(raw).to_uppercase();
    let mut groups = Vec::new();
    for chunk in hex.as_bytes().chunks(5) {
        groups.push(String::from_utf8_lossy(chunk).to_string());
    }
    groups.join("-")
}

fn derive_chunk_nonce(base_nonce: &[u8; 12], chunk_idx: u32) -> [u8; 12] {
    let mut nonce = *base_nonce;
    nonce[8..12].copy_from_slice(&chunk_idx.to_le_bytes());
    nonce
}

pub fn is_encrypted_file(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 6];
    let read = file.read(&mut magic)?;
    if read != 6 {
        return Ok(false);
    }
    Ok(&magic == FILE_MAGIC)
}

pub fn encrypt_file(input_path: &Path, output_path: &Path, key: &[u8; 32]) -> Result<()> {
    let input = File::open(input_path).with_context(|| {
        format!(
            "Failed to open input file for encryption: {}",
            input_path.display()
        )
    })?;
    let mut reader = BufReader::new(input);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = File::create(output_path).with_context(|| {
        format!(
            "Failed to create encrypted output file: {}",
            output_path.display()
        )
    })?;
    let mut writer = BufWriter::new(output);

    encrypt_stream(&mut reader, &mut writer, key)
}

/// Write a `WBENC1` stream for everything `reader` yields.
///
/// Split out of `encrypt_file` so a backup envelope can prepend its plaintext
/// header and then hand the same writer to the encryptor, keeping the artifact a
/// single self-contained file.
pub fn encrypt_stream<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &[u8; 32],
) -> Result<()> {
    let mut base_nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut base_nonce);

    writer.write_all(FILE_MAGIC)?;
    writer.write_all(&[FILE_VERSION])?;
    writer.write_all(&DEFAULT_CHUNK_SIZE.to_le_bytes())?;
    writer.write_all(&base_nonce)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut chunk_buf = vec![0u8; DEFAULT_CHUNK_SIZE as usize];
    let mut chunk_idx: u32 = 0;

    loop {
        let n = reader.read(&mut chunk_buf)?;
        if n == 0 {
            break;
        }

        let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
        let aad = chunk_idx.to_le_bytes();
        let payload = Payload {
            msg: &chunk_buf[..n],
            aad: &aad,
        };
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), payload)
            .map_err(|_| anyhow!("Chunk encryption failed at chunk {}", chunk_idx))?;

        let len = ciphertext.len() as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&ciphertext)?;
        chunk_idx = chunk_idx
            .checked_add(1)
            .ok_or_else(|| anyhow!("Chunk counter overflow"))?;
    }

    writer.flush()?;
    Ok(())
}

pub fn decrypt_file(input_path: &Path, output_path: &Path, key: &[u8; 32]) -> Result<()> {
    let input = File::open(input_path).with_context(|| {
        format!(
            "Failed to open encrypted input file: {}",
            input_path.display()
        )
    })?;
    let mut reader = BufReader::new(input);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = File::create(output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
    let mut writer = BufWriter::new(output);

    decrypt_stream(&mut reader, &mut writer, key)
}

/// Read a `WBENC1` stream from `reader`, writing the plaintext to `writer`.
///
/// The counterpart to `encrypt_stream`: a backup envelope consumes its header
/// first and then passes the still-positioned reader here.
pub fn decrypt_stream<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &[u8; 32],
) -> Result<()> {
    let mut magic = [0u8; 6];
    reader.read_exact(&mut magic)?;
    if &magic != FILE_MAGIC {
        return Err(anyhow!("Input is not a Wander(er) encrypted file"));
    }

    let mut version = [0u8; 1];
    reader.read_exact(&mut version)?;
    if version[0] != FILE_VERSION {
        return Err(anyhow!(
            "Unsupported encrypted file version: {}",
            version[0]
        ));
    }

    let mut chunk_size_bytes = [0u8; 4];
    reader.read_exact(&mut chunk_size_bytes)?;
    let chunk_size = u32::from_le_bytes(chunk_size_bytes);
    if chunk_size == 0 || chunk_size > 8 * 1024 * 1024 {
        return Err(anyhow!("Invalid encrypted chunk size"));
    }

    let mut base_nonce = [0u8; 12];
    reader.read_exact(&mut base_nonce)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut chunk_idx: u32 = 0;
    let mut len_buf = [0u8; 4];

    loop {
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        // A chunk is at most `chunk_size` of plaintext plus the 16 byte GCM tag,
        // and the header's `chunk_size` was bounded above. Without the upper
        // bound this length is attacker-controlled and unauthenticated: a
        // declared 0xFFFFFFFF allocates ~4 GiB per chunk before a single tag is
        // verified, so a corrupt or hostile file is an out-of-memory abort.
        let ct_len = u32::from_le_bytes(len_buf) as usize;
        let max_ct_len = chunk_size as usize + 16;
        if !(16..=max_ct_len).contains(&ct_len) {
            return Err(anyhow!(
                "Invalid encrypted chunk length: {} (expected 16..={})",
                ct_len,
                max_ct_len
            ));
        }

        let mut ciphertext = vec![0u8; ct_len];
        reader.read_exact(&mut ciphertext)?;

        let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
        let aad = chunk_idx.to_le_bytes();
        let payload = Payload {
            msg: ciphertext.as_ref(),
            aad: &aad,
        };
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), payload)
            .map_err(|_| anyhow!("Chunk decryption failed at chunk {}", chunk_idx))?;

        if plaintext.len() > chunk_size as usize {
            return Err(anyhow!("Invalid plaintext chunk length"));
        }

        writer.write_all(&plaintext)?;
        chunk_idx = chunk_idx
            .checked_add(1)
            .ok_or_else(|| anyhow!("Chunk counter overflow"))?;
    }

    writer.flush()?;
    Ok(())
}

pub fn decrypt_file_if_needed(
    input_path: &Path,
    output_path: &Path,
    key: Option<&[u8; 32]>,
) -> Result<bool> {
    if !is_encrypted_file(input_path)? {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(input_path, output_path)?;
        return Ok(false);
    }

    let key = key.ok_or_else(|| anyhow!("Encrypted file requires unlocked encryption key"))?;
    decrypt_file(input_path, output_path, key)?;
    Ok(true)
}

#[cfg(target_os = "windows")]
pub fn dpapi_protect(data: &[u8], description: &str) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let description_utf16: Vec<u16> = description.encode_utf16().chain(Some(0)).collect();

    let ok = unsafe {
        CryptProtectData(
            &mut in_blob,
            description_utf16.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };

    if ok == 0 {
        return Err(anyhow!("CryptProtectData failed"));
    }

    let bytes =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(out_blob.pbData as _);
    }
    Ok(bytes)
}

#[cfg(target_os = "windows")]
pub fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let mut desc_out: windows_sys::core::PWSTR = std::ptr::null_mut();

    let ok = unsafe {
        CryptUnprotectData(
            &mut in_blob,
            &mut desc_out,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
            &mut out_blob,
        )
    };

    if ok == 0 {
        return Err(anyhow!("CryptUnprotectData failed"));
    }

    let bytes =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();

    unsafe {
        if !out_blob.pbData.is_null() {
            let _ = LocalFree(out_blob.pbData as _);
        }
        if !desc_out.is_null() {
            let _ = LocalFree(desc_out as _);
        }
    }
    Ok(bytes)
}

#[cfg(not(target_os = "windows"))]
pub fn dpapi_protect(_data: &[u8], _description: &str) -> Result<Vec<u8>> {
    Err(anyhow!(
        "DPAPI secure storage is only supported on Windows in this build"
    ))
}

#[cfg(not(target_os = "windows"))]
pub fn dpapi_unprotect(_data: &[u8]) -> Result<Vec<u8>> {
    Err(anyhow!(
        "DPAPI secure storage is only supported on Windows in this build"
    ))
}

pub fn serialize_and_protect<T: Serialize>(value: &T, description: &str) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let protected = dpapi_protect(&bytes, description)?;
    Ok(B64.encode(protected))
}

pub fn unprotect_and_deserialize<T: for<'de> Deserialize<'de>>(blob_b64: &str) -> Result<T> {
    let protected = B64.decode(blob_b64)?;
    let bytes = dpapi_unprotect(&protected)?;
    Ok(serde_json::from_slice::<T>(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_key_verification_roundtrip() {
        let key = generate_recovery_key();
        let hash = hash_recovery_key(&key).expect("hash");
        assert!(verify_recovery_key(&key, &hash).expect("verify"));
        assert!(!verify_recovery_key("WRONG-KEY", &hash).expect("verify2"));
    }

    #[test]
    fn security_bundle_encrypt_unlock_roundtrip() {
        let (bundle, _, _) =
            SecurityBundle::new_encrypted("correct horse battery staple").expect("bundle");
        let key = bundle
            .unlock_with_passphrase("correct horse battery staple")
            .expect("unlock");
        assert_eq!(key.len(), 32);
        assert!(bundle.unlock_with_passphrase("bad passphrase").is_err());
    }

    #[test]
    fn recovery_key_unlocks_without_rewrapping() {
        let (bundle, recovery_key, key) =
            SecurityBundle::new_encrypted("correct horse battery staple").expect("bundle");
        assert_eq!(
            bundle
                .unlock_with_recovery_key(&recovery_key)
                .expect("unlock"),
            key
        );
        assert!(bundle.unlock_with_recovery_key("WRONG-KEY").is_err());
    }

    /// Build a `WBENC1` header followed by a single chunk length, with no chunk
    /// body: enough to exercise the length validation without a real key.
    fn header_with_chunk_len(chunk_size: u32, ct_len: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(FILE_MAGIC);
        buf.push(FILE_VERSION);
        buf.extend_from_slice(&chunk_size.to_le_bytes());
        buf.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(&ct_len.to_le_bytes());
        buf
    }

    #[test]
    fn oversized_chunk_length_is_rejected_before_allocating() {
        let key = [7u8; 32];

        // ~4 GiB declared. This must fail on the bound, not on the allocation.
        let hostile = header_with_chunk_len(DEFAULT_CHUNK_SIZE, u32::MAX);
        let err = decrypt_stream(&mut hostile.as_slice(), &mut Vec::new(), &key)
            .expect_err("oversized chunk must be rejected");
        assert!(err.to_string().contains("Invalid encrypted chunk length"));

        // One byte past the legitimate maximum is still rejected.
        let over = header_with_chunk_len(DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_SIZE + 17);
        assert!(decrypt_stream(&mut over.as_slice(), &mut Vec::new(), &key).is_err());

        // Under the tag length was already rejected and still is.
        let under = header_with_chunk_len(DEFAULT_CHUNK_SIZE, 15);
        assert!(decrypt_stream(&mut under.as_slice(), &mut Vec::new(), &key).is_err());

        // A length at the maximum passes validation and fails later, on the
        // truncated body, which proves the bound itself is not too tight.
        let at_max = header_with_chunk_len(DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_SIZE + 16);
        let err = decrypt_stream(&mut at_max.as_slice(), &mut Vec::new(), &key)
            .expect_err("truncated body must fail");
        assert!(!err.to_string().contains("Invalid encrypted chunk length"));
    }

    /// Guards the split of `encrypt_file` and `decrypt_file` into stream
    /// functions: the file-level wrappers must still round-trip byte-for-byte,
    /// across more than one chunk.
    #[test]
    fn encrypt_file_decrypt_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wanderer-sec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let plain = dir.join("plain.bin");
        let sealed = dir.join("sealed.wbenc");
        let out = dir.join("out.bin");

        let contents: Vec<u8> = (0..(DEFAULT_CHUNK_SIZE as usize + 4096))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&plain, &contents).expect("write");

        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);

        encrypt_file(&plain, &sealed, &key).expect("encrypt");
        assert!(is_encrypted_file(&sealed).expect("magic"));
        decrypt_file(&sealed, &out, &key).expect("decrypt");
        assert_eq!(std::fs::read(&out).expect("read"), contents);

        let mut wrong_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut wrong_key);
        assert!(decrypt_file(&sealed, &dir.join("bad.bin"), &wrong_key).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
