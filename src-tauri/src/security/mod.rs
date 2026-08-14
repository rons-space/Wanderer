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
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// The v1 stream: a header nothing authenticated, followed by chunks whose only
/// associated data was their own index. Still read, never written. Every library
/// encrypted before this change is full of it, in the cache and in Telegram.
const MAGIC_V1: &[u8; 6] = b"WBENC1";
const FILE_VERSION_V1: u8 = 1;

/// The v2 stream. See `encrypt_stream` for the layout and what each field buys.
const MAGIC_V2: &[u8; 6] = b"WBENC2";
const FILE_VERSION_V2: u8 = 2;

const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024; // 1MB

/// AES-GCM tag length. A chunk carrying no plaintext is exactly this long, which is
/// how the terminator is told apart from data without trusting anything unauthenticated.
const TAG_LEN: usize = 16;

/// Truncated SHA-256 of the key, enough to answer "was this file written with the key
/// I hold" before decrypting a megabyte to find out.
const KEY_ID_LEN: usize = 8;

/// magic 6 || version 1 || chunk_size 4 || base_nonce 12 || key_id 8.
const V2_HEADER_LEN: usize = 6 + 1 + 4 + 12 + KEY_ID_LEN;

/// Domain separators in the associated data, so a data chunk can never be replayed
/// as the terminator and the terminator can never be replayed as chunk zero.
const CHUNK_KIND_DATA: u8 = 0;
const CHUNK_KIND_TERMINATOR: u8 = 1;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationStatus {
    pub running: bool,
    pub total: i64,
    pub processed: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub last_error: Option<String>,
}

/// The 32 bytes that decrypt the entire library.
///
/// A newtype rather than a bare `[u8; 32]`, for two reasons. It is not `Copy`, so
/// every duplicate of the key is a visible `.clone()` in the diff instead of an
/// implicit memcpy that leaves a copy behind wherever it landed. And it zeroizes on
/// drop, so a copy that does get made stops being readable in the process image, or in
/// a crash dump or swap file, the moment it goes out of scope.
///
/// `expose` is deliberately awkward to say. Every call is a place where the raw key
/// escapes into something that will not clear it.
#[derive(Clone, ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Constant-time, because comparing key material with a short-circuiting `==` is a
/// habit worth not having, even where today's only callers are tests.
impl PartialEq for MasterKey {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl Eq for MasterKey {}

/// Hand-written so that no log line, `dbg!` or `#[derive(Debug)]` on a struct that
/// happens to hold a key can ever print the bytes.
impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

#[derive(Debug, Default)]
pub struct RuntimeState {
    pub master_key: Option<MasterKey>,
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

    pub fn new_encrypted(passphrase: &str) -> Result<(Self, Zeroizing<String>, MasterKey)> {
        if passphrase.trim().len() < 8 {
            return Err(anyhow!("Passphrase must be at least 8 characters"));
        }

        let mut key_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key_bytes);
        let master_key = MasterKey::new(key_bytes);
        key_bytes.zeroize();

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

    pub fn unlock_with_passphrase(&self, passphrase: &str) -> Result<MasterKey> {
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
    pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<MasterKey> {
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
    ) -> Result<(Self, MasterKey)> {
        let master_key = self.unlock_with_recovery_key(recovery_key)?;
        let passphrase_wrap = wrap_master_key_with_secret(new_passphrase.as_bytes(), &master_key)?;

        let mut next = self.clone();
        next.passphrase_wrap = Some(passphrase_wrap);
        Ok((next, master_key))
    }

    pub fn regenerate_recovery_key(
        &self,
        passphrase: &str,
    ) -> Result<(Self, Zeroizing<String>, MasterKey)> {
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

/// `Zeroizing` because this is the key that decrypts the master key: leaving it in a
/// stack buffer is the same exposure as leaving the master key there.
fn derive_secret_key(secret: &[u8], salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>> {
    let mut out = Zeroizing::new([0u8; 32]);
    let argon2 = argon2id_params()?;
    argon2
        .hash_password_into(secret, salt, out.as_mut())
        .map_err(|e| anyhow!("Argon2 derivation failed: {}", e))?;
    Ok(out)
}

fn wrap_master_key_with_secret(secret: &[u8], master_key: &MasterKey) -> Result<WrappedMasterKey> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let derived_key = derive_secret_key(secret, &salt)?;

    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(derived_key.as_ref()));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), master_key.expose().as_slice())
        .map_err(|_| anyhow!("Failed to wrap master key"))?;

    Ok(WrappedMasterKey {
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce),
        ciphertext_b64: B64.encode(ciphertext),
    })
}

fn unwrap_master_key_with_secret(secret: &[u8], wrapped: &WrappedMasterKey) -> Result<MasterKey> {
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
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(derived_key.as_ref()));
    // The AEAD hands back an ordinary Vec, so the unwrapped key exists in an allocation
    // nothing clears. Wrap it before anything else can fail and leave it behind.
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&nonce_vec), ciphertext.as_ref())
            .map_err(|_| anyhow!("Failed to unwrap key. Secret may be invalid"))?,
    );

    if plaintext.len() != 32 {
        return Err(anyhow!("Invalid unwrapped master key length"));
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(&plaintext);
    let key = MasterKey::new(out);
    out.zeroize();
    Ok(key)
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

fn generate_recovery_key() -> Zeroizing<String> {
    let mut raw = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let hex = hex::encode(raw).to_uppercase();
    let mut groups = Vec::new();
    for chunk in hex.as_bytes().chunks(5) {
        groups.push(String::from_utf8_lossy(chunk).to_string());
    }
    Zeroizing::new(groups.join("-"))
}

/// A stable identifier for a key, without being the key.
///
/// Domain separated and truncated: the point is to fail a wrong-key decrypt with a
/// clear error instead of a tag failure on every chunk, not to prove anything about
/// the key. The master key is 256 bits of `OsRng`, so a commitment to 64 bits of its
/// hash is not a search anyone can run.
fn key_id(key: &MasterKey) -> [u8; KEY_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(b"wanderer-wbenc2-key-id");
    hasher.update(key.expose());
    let digest = hasher.finalize();
    let mut out = [0u8; KEY_ID_LEN];
    out.copy_from_slice(&digest[..KEY_ID_LEN]);
    out
}

/// The associated data for one chunk: the entire header, the chunk index and the
/// kind.
///
/// Binding the header is the point of v2. In v1 the header was plaintext that nothing
/// covered, so `chunk_size` and `base_nonce` could be edited in place; only the chunk
/// index was authenticated, which made a chunk from one file indistinguishable from
/// the chunk at the same index of another file encrypted with the same key.
fn chunk_aad(header: &[u8], chunk_idx: u32, kind: u8) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + 5);
    aad.extend_from_slice(header);
    aad.extend_from_slice(&chunk_idx.to_le_bytes());
    aad.push(kind);
    aad
}

fn derive_chunk_nonce(base_nonce: &[u8; 12], chunk_idx: u32) -> [u8; 12] {
    let mut nonce = *base_nonce;
    nonce[8..12].copy_from_slice(&chunk_idx.to_le_bytes());
    nonce
}

/// Whether a file starts with either Wanderer magic.
///
/// A guess, and named as one at every call site that still uses it: six bytes of an
/// unauthenticated prefix say what a file claims to be, not what it is. Prefer
/// `decrypt_file_if_needed` with `Expect::Encrypted` wherever the caller knows.
pub fn is_encrypted_file(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 6];
    let read = file.read(&mut magic)?;
    if read != 6 {
        return Ok(false);
    }
    Ok(&magic == MAGIC_V2 || &magic == MAGIC_V1)
}

pub fn encrypt_file(input_path: &Path, output_path: &Path, key: &MasterKey) -> Result<()> {
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

/// Write a `WBENC2` stream for everything `reader` yields.
///
/// Layout:
///
/// ```text
/// "WBENC2" | version | chunk_size | base_nonce | key_id | chunk* | terminator
///        6 |       1 |          4 |         12 |      8 |        |
/// ```
///
/// where each chunk and the terminator are `len: u32 || ciphertext`, and the whole
/// 31 byte header is the associated data of every one of them. Three things v1 got
/// wrong, in order of how much they mattered:
///
/// 1. **Nothing authenticated the header.** `chunk_size` and `base_nonce` sat in
///    plaintext that no tag covered. Binding them means an edited header now fails
///    on the first chunk instead of steering the decryptor.
/// 2. **Truncation was invisible.** The stream ended when the file ended, so dropping
///    whole chunks off the end produced a shorter file that decrypted perfectly. The
///    terminator carries the chunk count in its associated data, so a stream that
///    stops early has no valid terminator and a stream missing chunks has the wrong
///    count. Bytes appended after the terminator are rejected too.
/// 3. **Nothing said which key.** The key id turns "every chunk fails its tag" into
///    "this file was written with a different key".
///
/// The count lives in the terminator rather than the header because the header is
/// written before the input has been read, and this encrypts a `Read` of unknown
/// length: a header field would mean buffering the whole file or seeking backwards
/// in a writer that a backup envelope has already written its own header to.
///
/// Split out of `encrypt_file` so that envelope can prepend its plaintext header and
/// then hand the same writer here, keeping the artifact a single self-contained file.
pub fn encrypt_stream<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &MasterKey,
) -> Result<()> {
    let mut base_nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut base_nonce);

    let mut header = Vec::with_capacity(V2_HEADER_LEN);
    header.extend_from_slice(MAGIC_V2);
    header.push(FILE_VERSION_V2);
    header.extend_from_slice(&DEFAULT_CHUNK_SIZE.to_le_bytes());
    header.extend_from_slice(&base_nonce);
    header.extend_from_slice(&key_id(key));
    debug_assert_eq!(header.len(), V2_HEADER_LEN);
    writer.write_all(&header)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.expose()));
    let mut chunk_buf = vec![0u8; DEFAULT_CHUNK_SIZE as usize];
    let mut chunk_idx: u32 = 0;

    loop {
        let n = reader.read(&mut chunk_buf)?;
        if n == 0 {
            break;
        }

        let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
        let aad = chunk_aad(&header, chunk_idx, CHUNK_KIND_DATA);
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

    // The terminator: no plaintext, so it is exactly a tag, and its index is one past
    // the last data chunk, so it never reuses a nonce. Its associated data carries the
    // number of data chunks that came before it.
    let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
    let aad = chunk_aad(&header, chunk_idx, CHUNK_KIND_TERMINATOR);
    let terminator = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &[],
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("Failed to write the stream terminator"))?;
    writer.write_all(&(terminator.len() as u32).to_le_bytes())?;
    writer.write_all(&terminator)?;

    writer.flush()?;
    Ok(())
}

pub fn decrypt_file(input_path: &Path, output_path: &Path, key: &MasterKey) -> Result<()> {
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

/// Read a Wanderer encrypted stream from `reader`, writing the plaintext to `writer`.
///
/// Dispatches on the magic, because v1 files are not going away: they are in every
/// library encrypted before v2 and in every blob already uploaded to Telegram. The
/// counterpart to `encrypt_stream`: a backup envelope consumes its own header first
/// and then passes the still-positioned reader here.
pub fn decrypt_stream<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &MasterKey,
) -> Result<()> {
    let mut magic = [0u8; 6];
    reader.read_exact(&mut magic)?;
    if &magic == MAGIC_V2 {
        decrypt_stream_v2(reader, writer, key)
    } else if &magic == MAGIC_V1 {
        decrypt_stream_v1(reader, writer, key)
    } else {
        Err(anyhow!("Input is not a Wander(er) encrypted file"))
    }
}

/// Read the header fields that follow the magic and are common to both versions.
///
/// Returns the chunk size, bounded, and the base nonce.
fn read_common_header<R: Read>(reader: &mut R, expected_version: u8) -> Result<(u32, [u8; 12])> {
    let mut version = [0u8; 1];
    reader.read_exact(&mut version)?;
    if version[0] != expected_version {
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
    Ok((chunk_size, base_nonce))
}

/// Validate a declared chunk length against the header's chunk size.
///
/// A chunk is at most `chunk_size` of plaintext plus the tag, and `chunk_size` was
/// bounded when the header was read. Without this the length is attacker controlled
/// and unauthenticated: a declared `0xFFFFFFFF` allocates ~4 GiB before a single tag
/// is verified, so a corrupt or hostile file becomes an out-of-memory abort.
fn checked_chunk_len(len_buf: [u8; 4], chunk_size: u32) -> Result<usize> {
    let ct_len = u32::from_le_bytes(len_buf) as usize;
    let max_ct_len = chunk_size as usize + TAG_LEN;
    if !(TAG_LEN..=max_ct_len).contains(&ct_len) {
        return Err(anyhow!(
            "Invalid encrypted chunk length: {} (expected {}..={})",
            ct_len,
            TAG_LEN,
            max_ct_len
        ));
    }
    Ok(ct_len)
}

fn decrypt_stream_v2<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &MasterKey,
) -> Result<()> {
    let (chunk_size, base_nonce) = read_common_header(reader, FILE_VERSION_V2)?;

    let mut file_key_id = [0u8; KEY_ID_LEN];
    reader.read_exact(&mut file_key_id)?;
    let expected_key_id = key_id(key);
    if file_key_id != expected_key_id {
        return Err(anyhow!(
            "This file was encrypted with a different key than the one currently unlocked"
        ));
    }

    // Reconstructed rather than kept: every byte of it was just validated, so a
    // rebuilt header that differs from the one on disk cannot verify a single chunk.
    let mut header = Vec::with_capacity(V2_HEADER_LEN);
    header.extend_from_slice(MAGIC_V2);
    header.push(FILE_VERSION_V2);
    header.extend_from_slice(&chunk_size.to_le_bytes());
    header.extend_from_slice(&base_nonce);
    header.extend_from_slice(&file_key_id);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.expose()));
    let mut chunk_idx: u32 = 0;
    let mut len_buf = [0u8; 4];

    loop {
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            // v1 ended here and called it success. In v2 the stream is only complete
            // when the terminator says so, so running out of input is truncation.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(anyhow!(
                    "Encrypted stream ended after {} chunks without a terminator; \
                     the file is truncated",
                    chunk_idx
                ));
            }
            Err(e) => return Err(e.into()),
        }

        let ct_len = checked_chunk_len(len_buf, chunk_size)?;
        let mut ciphertext = vec![0u8; ct_len];
        reader.read_exact(&mut ciphertext)?;

        let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);

        // A tag with no plaintext is the terminator, and a data chunk is never
        // empty, so the lengths cannot collide. Guessing wrong is not exploitable
        // either way: the kind is in the associated data, so a data chunk cut down
        // to a bare tag fails to verify as a terminator.
        if ct_len == TAG_LEN {
            let aad = chunk_aad(&header, chunk_idx, CHUNK_KIND_TERMINATOR);
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: ciphertext.as_ref(),
                        aad: &aad,
                    },
                )
                .map_err(|_| {
                    anyhow!(
                        "Stream terminator failed to verify at chunk {}; the file is \
                         truncated or has been modified",
                        chunk_idx
                    )
                })?;

            let mut trailing = [0u8; 1];
            match reader.read(&mut trailing) {
                Ok(0) => {}
                Ok(_) => {
                    return Err(anyhow!(
                        "Encrypted stream has data after its terminator; the file has \
                         been modified"
                    ))
                }
                Err(e) => return Err(e.into()),
            }

            writer.flush()?;
            return Ok(());
        }

        let aad = chunk_aad(&header, chunk_idx, CHUNK_KIND_DATA);
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext.as_ref(),
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("Chunk decryption failed at chunk {}", chunk_idx))?;

        if plaintext.len() > chunk_size as usize {
            return Err(anyhow!("Invalid plaintext chunk length"));
        }

        writer.write_all(&plaintext)?;
        chunk_idx = chunk_idx
            .checked_add(1)
            .ok_or_else(|| anyhow!("Chunk counter overflow"))?;
    }
}

/// The v1 reader, unchanged in behaviour and kept only to open old files.
///
/// It still cannot detect a stream that lost whole chunks off its end, because
/// nothing in the format says how many there should have been. That is the reason v2
/// exists, and it is why nothing writes v1 any more.
fn decrypt_stream_v1<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    key: &MasterKey,
) -> Result<()> {
    let (chunk_size, base_nonce) = read_common_header(reader, FILE_VERSION_V1)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.expose()));
    let mut chunk_idx: u32 = 0;
    let mut len_buf = [0u8; 4];

    loop {
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        let ct_len = checked_chunk_len(len_buf, chunk_size)?;
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

/// What the caller knows about whether the input is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The caller knows this blob is encrypted, because it wrote it or because the
    /// database says so. A missing magic is then a real failure and not a plaintext
    /// file: silently copying it through is how plaintext ends up where the caller
    /// believed ciphertext was.
    Encrypted,
    /// The caller genuinely cannot know. An encrypted library still contains blobs
    /// uploaded before encryption was turned on, until the migration reaches them,
    /// so this cannot simply be `Encrypted` everywhere.
    Unknown,
}

/// Decrypt `input_path` into `output_path`, or copy it through when it is plaintext
/// and the caller allows for that. Returns whether it was decrypted.
pub fn decrypt_file_if_needed(
    input_path: &Path,
    output_path: &Path,
    key: Option<&MasterKey>,
    expect: Expect,
) -> Result<bool> {
    if !is_encrypted_file(input_path)? {
        if expect == Expect::Encrypted {
            return Err(anyhow!(
                "{} was expected to be encrypted but carries no Wanderer header",
                input_path.display()
            ));
        }
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
        assert_eq!(key.expose().len(), 32);
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

    /// Build a `WBENC2` header followed by a single chunk length, with no chunk
    /// body: enough to exercise the length validation without a real key. The key id
    /// has to match, because it is checked before any chunk is read.
    fn header_with_chunk_len(key: &MasterKey, chunk_size: u32, ct_len: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_V2);
        buf.push(FILE_VERSION_V2);
        buf.extend_from_slice(&chunk_size.to_le_bytes());
        buf.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(&key_id(key));
        buf.extend_from_slice(&ct_len.to_le_bytes());
        buf
    }

    /// A v1 writer, kept only so the tests can produce the files this app used to
    /// write and prove they still open. Nothing in the application writes v1.
    fn encrypt_stream_v1<R: Read, W: Write>(
        reader: &mut R,
        writer: &mut W,
        key: &MasterKey,
    ) -> Result<()> {
        let mut base_nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut base_nonce);

        writer.write_all(MAGIC_V1)?;
        writer.write_all(&[FILE_VERSION_V1])?;
        writer.write_all(&DEFAULT_CHUNK_SIZE.to_le_bytes())?;
        writer.write_all(&base_nonce)?;

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.expose()));
        let mut chunk_buf = vec![0u8; DEFAULT_CHUNK_SIZE as usize];
        let mut chunk_idx: u32 = 0;
        loop {
            let n = reader.read(&mut chunk_buf)?;
            if n == 0 {
                break;
            }
            let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
            let aad = chunk_idx.to_le_bytes();
            let ciphertext = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &chunk_buf[..n],
                        aad: &aad,
                    },
                )
                .map_err(|_| anyhow!("v1 chunk encryption failed"))?;
            writer.write_all(&(ciphertext.len() as u32).to_le_bytes())?;
            writer.write_all(&ciphertext)?;
            chunk_idx += 1;
        }
        writer.flush()?;
        Ok(())
    }

    /// Plaintext spanning several chunks, so tests can drop a whole chunk rather
    /// than only cut one in half.
    fn multi_chunk_plaintext() -> Vec<u8> {
        (0..(DEFAULT_CHUNK_SIZE as usize * 2 + 77))
            .map(|i| (i % 251) as u8)
            .collect()
    }

    fn seal(contents: &[u8], key: &MasterKey) -> Vec<u8> {
        let mut sealed = Vec::new();
        encrypt_stream(&mut &contents[..], &mut sealed, key).expect("encrypt");
        sealed
    }

    fn open(sealed: &[u8], key: &MasterKey) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        decrypt_stream(&mut &sealed[..], &mut out, key)?;
        Ok(out)
    }

    /// The format v2 exists for: a stream that lost whole chunks off its end used to
    /// decrypt perfectly into a shorter file, so a truncated photo was indistinguishable
    /// from a complete one.
    #[test]
    fn a_v2_stream_missing_its_last_chunk_is_rejected() {
        let key = random_test_key();
        let contents = multi_chunk_plaintext();
        let sealed = seal(&contents, &key);

        assert_eq!(open(&sealed, &key).expect("roundtrip"), contents);

        // Cut on an exact chunk boundary, after two whole chunks. Nothing fails on a
        // short read here, which is the point: this is the clean drop of trailing
        // chunks that v1 accepted as a complete, shorter file.
        let whole_chunk = 4 + DEFAULT_CHUNK_SIZE as usize + TAG_LEN;
        let cut = V2_HEADER_LEN + 2 * whole_chunk;
        assert!(cut < sealed.len(), "fixture is not multi-chunk");
        let err = open(&sealed[..cut], &key).expect_err("truncated stream must fail");
        assert!(
            err.to_string().contains("without a terminator"),
            "unexpected error: {}",
            err
        );

        // And the ragged case, cut in the middle of a chunk, still fails too.
        assert!(open(&sealed[..cut + 9], &key).is_err());
    }

    /// The same file with the terminator removed. Under v1 this was simply "the file
    /// ended", which is exactly the ambiguity the terminator removes.
    #[test]
    fn a_v2_stream_with_its_terminator_removed_is_rejected() {
        let key = random_test_key();
        let sealed = seal(b"a few bytes", &key);

        // The terminator is the trailing 4 byte length plus a bare tag.
        let without = &sealed[..sealed.len() - (4 + TAG_LEN)];
        let err = open(without, &key).expect_err("missing terminator must fail");
        assert!(
            err.to_string().contains("without a terminator"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn data_appended_after_the_terminator_is_rejected() {
        let key = random_test_key();
        let mut sealed = seal(b"a few bytes", &key);
        sealed.extend_from_slice(&[0u8; 8]);

        let err = open(&sealed, &key).expect_err("trailing data must fail");
        assert!(
            err.to_string().contains("after its terminator"),
            "unexpected error: {}",
            err
        );
    }

    /// The v1 header was plaintext that no tag covered, so these fields could be
    /// edited in place. In v2 the whole header is associated data.
    #[test]
    fn editing_the_v2_header_is_rejected() {
        let key = random_test_key();
        let contents = multi_chunk_plaintext();
        let sealed = seal(&contents, &key);

        // chunk_size, at offset 7, still inside the bound the reader enforces.
        let mut edited = sealed.clone();
        edited[7..11].copy_from_slice(&(DEFAULT_CHUNK_SIZE / 2).to_le_bytes());
        assert!(
            open(&edited, &key).is_err(),
            "an edited chunk_size was accepted"
        );

        // base_nonce, at offset 11.
        let mut edited = sealed.clone();
        edited[11] ^= 0xFF;
        assert!(
            open(&edited, &key).is_err(),
            "an edited base_nonce was accepted"
        );

        // key_id, at offset 23, is checked before a single chunk is read.
        let mut edited = sealed.clone();
        edited[23] ^= 0xFF;
        let err = open(&edited, &key).expect_err("an edited key_id was accepted");
        assert!(
            err.to_string().contains("different key"),
            "unexpected error: {}",
            err
        );
    }

    /// Chunks are bound to their position and their kind, so neither can be moved.
    #[test]
    fn chunks_cannot_be_reordered_or_replayed_as_the_terminator() {
        let key = random_test_key();
        let contents = multi_chunk_plaintext();
        let sealed = seal(&contents, &key);

        // Chunk 0 starts after the header; swap its body with chunk 1's. Both are
        // full size, so the framing stays valid and only the tags disagree.
        let first = V2_HEADER_LEN + 4;
        let body = DEFAULT_CHUNK_SIZE as usize + TAG_LEN;
        let second = first + body + 4;
        let mut swapped = sealed.clone();
        for i in 0..body {
            swapped.swap(first + i, second + i);
        }
        assert!(open(&swapped, &key).is_err(), "reordered chunks decrypted");

        // Truncating the file to end at a bare tag makes that tag look like a
        // terminator by length. The kind is in the associated data, so it is not one.
        let mut forged = sealed[..first].to_vec();
        forged.extend_from_slice(&(TAG_LEN as u32).to_le_bytes());
        forged.extend_from_slice(&sealed[first + body - TAG_LEN..first + body]);
        assert!(
            open(&forged, &key).is_err(),
            "a data tag was accepted as a terminator"
        );
    }

    /// Wrong key now says so, instead of failing every chunk's tag in turn.
    #[test]
    fn a_v2_stream_names_the_wrong_key() {
        let key = random_test_key();
        let sealed = seal(b"secret", &key);
        let err = open(&sealed, &random_test_key()).expect_err("wrong key must fail");
        assert!(
            err.to_string().contains("different key"),
            "unexpected error: {}",
            err
        );
    }

    /// Every library encrypted before this change is full of v1, in the local cache
    /// and in Telegram. Being unable to read it would be data loss.
    #[test]
    fn v1_streams_still_decrypt() {
        let key = random_test_key();
        let contents = multi_chunk_plaintext();

        let mut sealed_v1 = Vec::new();
        encrypt_stream_v1(&mut &contents[..], &mut sealed_v1, &key).expect("v1 encrypt");
        assert_eq!(&sealed_v1[..6], MAGIC_V1);

        assert_eq!(open(&sealed_v1, &key).expect("v1 decrypt"), contents);
        assert!(
            open(&sealed_v1, &random_test_key()).is_err(),
            "a v1 stream opened with the wrong key"
        );
    }

    /// The writer only ever emits v2, and both magics are recognised on disk.
    #[test]
    fn encryption_writes_v2_and_both_magics_are_detected() {
        let dir = std::env::temp_dir().join(format!("wanderer-sec-v2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let key = random_test_key();

        let plain = dir.join("plain.bin");
        std::fs::write(&plain, b"contents").expect("write");
        let sealed = dir.join("sealed.wbenc");
        encrypt_file(&plain, &sealed, &key).expect("encrypt");
        assert_eq!(&std::fs::read(&sealed).expect("read")[..6], MAGIC_V2);
        assert!(is_encrypted_file(&sealed).expect("detect v2"));

        let legacy = dir.join("legacy.wbenc");
        let mut out = Vec::new();
        encrypt_stream_v1(&mut &b"contents"[..], &mut out, &key).expect("v1");
        std::fs::write(&legacy, &out).expect("write legacy");
        assert!(is_encrypted_file(&legacy).expect("detect v1"));

        assert!(!is_encrypted_file(&plain).expect("detect plaintext"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A caller that knows the blob is encrypted must not silently receive a copy of
    /// whatever plaintext was there instead.
    #[test]
    fn expecting_encryption_refuses_to_copy_plaintext_through() {
        let dir = std::env::temp_dir().join(format!("wanderer-sec-exp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let key = random_test_key();

        let plain = dir.join("plain.bin");
        std::fs::write(&plain, b"not encrypted").expect("write");
        let out = dir.join("out.bin");

        assert!(
            decrypt_file_if_needed(&plain, &out, Some(&key), Expect::Encrypted).is_err(),
            "plaintext was accepted where ciphertext was expected"
        );
        assert!(!out.exists(), "a plaintext copy was written anyway");

        // The pre-migration case still works.
        assert!(!decrypt_file_if_needed(&plain, &out, Some(&key), Expect::Unknown).expect("copy"));
        assert_eq!(std::fs::read(&out).expect("read"), b"not encrypted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_chunk_length_is_rejected_before_allocating() {
        let key = random_test_key();

        // ~4 GiB declared. This must fail on the bound, not on the allocation.
        let hostile = header_with_chunk_len(&key, DEFAULT_CHUNK_SIZE, u32::MAX);
        let err = decrypt_stream(&mut hostile.as_slice(), &mut Vec::new(), &key)
            .expect_err("oversized chunk must be rejected");
        assert!(err.to_string().contains("Invalid encrypted chunk length"));

        // One byte past the legitimate maximum is still rejected.
        let over = header_with_chunk_len(&key, DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_SIZE + 17);
        assert!(decrypt_stream(&mut over.as_slice(), &mut Vec::new(), &key).is_err());

        // Under the tag length was already rejected and still is.
        let under = header_with_chunk_len(&key, DEFAULT_CHUNK_SIZE, 15);
        assert!(decrypt_stream(&mut under.as_slice(), &mut Vec::new(), &key).is_err());

        // A length at the maximum passes validation and fails later, on the
        // truncated body, which proves the bound itself is not too tight.
        let at_max = header_with_chunk_len(&key, DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_SIZE + 16);
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

        let key = random_test_key();

        encrypt_file(&plain, &sealed, &key).expect("encrypt");
        assert!(is_encrypted_file(&sealed).expect("magic"));
        decrypt_file(&sealed, &out, &key).expect("decrypt");
        assert_eq!(std::fs::read(&out).expect("read"), contents);

        let wrong_key = random_test_key();
        assert!(decrypt_file(&sealed, &dir.join("bad.bin"), &wrong_key).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `RuntimeState` derives `Debug` and holds the key, so anything that logs the
    /// runtime state would print it if this newtype ever went back to a derive.
    #[test]
    fn debug_output_never_shows_key_bytes() {
        let key = MasterKey::new([0xAB; 32]);
        let rendered = format!("{:?} {:?}", key, Some(key.clone()));
        assert!(!rendered.contains("171"), "key bytes leaked: {}", rendered);
        assert!(!rendered.contains("ab"), "key bytes leaked: {}", rendered);
        assert_eq!(
            rendered,
            "MasterKey(<redacted>) Some(MasterKey(<redacted>))"
        );
    }

    /// Cloning is explicit precisely so that copies are visible, but a clone still
    /// has to be the same key, and comparison has to be by value.
    #[test]
    fn a_cloned_key_equals_its_original_and_differs_from_another() {
        let key = random_test_key();
        assert_eq!(key.clone(), key);
        assert_ne!(random_test_key(), key);
    }

    fn random_test_key() -> MasterKey {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        MasterKey::new(bytes)
    }

    /// Two fixtures for the tamper tests: a multi-chunk `.wbenc` and its plaintext.
    fn sealed_fixture(name: &str) -> (std::path::PathBuf, MasterKey, Vec<u8>) {
        let dir =
            std::env::temp_dir().join(format!("wanderer-sec-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let plain = dir.join("plain.bin");
        let sealed = dir.join("sealed.wbenc");

        // Larger than one chunk, so truncation and tampering can be aimed at a chunk
        // boundary rather than at the header.
        let contents: Vec<u8> = (0..(DEFAULT_CHUNK_SIZE as usize * 2 + 77))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&plain, &contents).expect("write");

        let key = random_test_key();
        encrypt_file(&plain, &sealed, &key).expect("encrypt");

        (sealed, key, contents)
    }

    /// A `.wbenc` cut short must fail rather than yielding the prefix it can decrypt.
    /// Returning partial plaintext would be worse than failing: the caller cannot tell
    /// a complete file from a truncated one, and a truncated media file looks like a
    /// corrupt photo rather than an integrity failure.
    #[test]
    fn truncated_ciphertext_is_rejected() {
        let (sealed, key, contents) = sealed_fixture("truncated");
        let dir = sealed.parent().unwrap().to_path_buf();

        let full = std::fs::read(&sealed).expect("read sealed");
        // Drop the final chunk, keeping the header and the first chunk intact.
        let cut = full.len() - (contents.len() / 2);
        std::fs::write(&sealed, &full[..cut]).expect("truncate");

        let out = dir.join("out.bin");
        assert!(
            decrypt_file(&sealed, &out, &key).is_err(),
            "a truncated archive decrypted successfully"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Flipping a bit inside a chunk must fail the AEAD tag, not surface as altered
    /// plaintext.
    #[test]
    fn tampered_chunk_is_rejected() {
        let (sealed, key, _) = sealed_fixture("tampered");
        let dir = sealed.parent().unwrap().to_path_buf();

        let mut bytes = std::fs::read(&sealed).expect("read sealed");
        // magic (6) + version (1) + chunk size (4) + base nonce (12), then the
        // 4-byte length of the first chunk: anything past that is ciphertext.
        const HEADER_LEN: usize = 6 + 1 + 4 + 12 + 4;
        let target = HEADER_LEN + 64;
        bytes[target] ^= 0b0000_0001;
        std::fs::write(&sealed, &bytes).expect("write tampered");

        let out = dir.join("out.bin");
        assert!(
            decrypt_file(&sealed, &out, &key).is_err(),
            "a tampered chunk decrypted successfully"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
