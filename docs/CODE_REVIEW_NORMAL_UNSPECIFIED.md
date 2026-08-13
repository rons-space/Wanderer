# Code Review: Wander(er) (`Wanderer`)

**Reviewed commit:** `a9d7439` (tip of `main` at time of review)
**Date:** 2026-08-13
**Reviewer:** [code]smith
**Scope:** full repository. Tauri 2 desktop app. 21 Rust files / 10,752 LOC in `src-tauri/src`, 62 TypeScript files / 10,915 LOC in `src`, 74 IPC commands, 19 schema migrations.

---

## Contents

| Section | Covers |
| --- | --- |
| [About this review](#about-this-review-and-who-wrote-it) | Who wrote this, method, what was and was not verified |
| [Executive summary](#executive-summary) | The blocking defects, measured state, severity definitions |
| [1. Encryption and key management](#1-encryption-and-key-management) | What the crypto gets right, and the four ways the layers around it fail |
| [2. Recoverability and data integrity](#2-recoverability-and-data-integrity) | The undecryptable backup, migrations, transactions, durability |
| [3. Attack surface](#3-attack-surface) | Shipped MCP bridge, CSP, asset scope, key material in the webview, SQL |
| [4. Rust code quality](#4-rust-code-quality) | Error handling, concurrency, duplication, performance |
| [5. README accuracy](#5-readme-accuracy) | Claim-by-claim documentation audit |
| [6. Frontend](#6-frontend) | Recovery-key UX, rendering performance, accessibility, dead code |
| [7. Operational readiness](#7-operational-readiness) | No CI, unsigned installer, lockfiles, committed debris |
| [8. What is already good](#8-what-is-already-good) | Load-bearing strengths, with citations |
| [9. Remediation plan](#9-remediation-plan) | Staged, ordered by risk reduction per unit of effort |
| [Appendix A](#appendix-a-reproducing-every-measurement) | The exact command behind every number in this document |
| [Appendix B](#appendix-b-finding-index-by-severity) | All findings indexed by severity |

---

## About this review and who wrote it

This review was performed by **[code]smith**, the cloud coding agent from [Blacksmith](https://blacksmith.sh). I am an autonomous agent that picks up delegated engineering work, does it end to end, and delivers a result. I was asked to do a deep dive and complete review of this repository, and this document is that result.

So that reviewers can calibrate how much to trust each claim, here is exactly how it was conducted.

### Method

1. Provisioned a sandbox with the repository cloned at `a9d7439` and read the tree, both manifests, the Tauri configuration, the README, and git history.
2. Ran four parallel review passes: cryptography and secret handling; the database layer and IPC command surface; the React frontend; and build configuration, supply chain and documentation accuracy. Each pass read actual file contents in full rather than inferring from filenames. `database.rs` (3,264 lines), `lib.rs` (2,545), `Settings.tsx` (1,302) and `MediaGrid.tsx` (914) were each read end to end.
3. Independently re-read and confirmed every finding rated Critical, plus the implicated code for each High, before including it here.
4. Ran the real toolchain to produce measured rather than estimated numbers: `npm ci`, `npx tsc --noEmit`, `npx vite build`, `npm audit`, `cargo fmt --check`.
5. Re-ran every quantitative claim in this document as a direct shell measurement. The commands are in [Appendix A](#appendix-a-reproducing-every-measurement) so you can reproduce any number yourself.

Because this repository ships a detailed user-facing `README.md` that makes specific security promises, a distinct part of the review was auditing **the accuracy of those promises against the code**. That audit is [Section 5](#5-readme-accuracy), and it is the most positive section of the report.

### What I did and did not verify

**Verified by reading the code, with file and line cited.** Every Critical and High finding.

**Verified by execution.** `npx tsc --noEmit` passes with 0 errors under `strict`. `npx vite build` succeeds. `npm audit` reports 7 vulnerabilities, 5 High. `cargo fmt --check` reports formatting drift in 7 files. Every count in this document.

**Not verified by execution.** I did not run `cargo build`, `cargo test`, or the application. The Rust toolchain would need to compile ONNX Runtime, `rusqlite` bundled SQLite and the full image stack from scratch, which is not a good use of the review budget when CI should be doing it (Finding 7.1). So **the Rust code is reviewed by reading only, and I did not confirm that it compiles or that its 8 unit tests pass.** No exploit was executed against a running instance; the attack paths described are traced from source.

**A specific limitation.** This is a Windows-first application and I reviewed it on Linux. Claims about DPAPI behaviour, WebView2 specifics, and the `%TEMP%`/`%LOCALAPPDATA%` layout are traced from code and from documented Windows semantics, not observed. Where a finding depends on Windows runtime behaviour I say so.

**Not covered.** The ONNX model files themselves, the correctness of the CLIP and ArcFace inference math, RAW decoding correctness across camera formats, Telegram protocol conformance inside the `grammers` dependency, visual and design review, and load testing.

### How to read my confidence

I have tried to be direct rather than diplomatic, because a hedged security review is not useful. Where I state something is exploitable or unrecoverable, I traced the path and believe it. Where I am inferring, I say so. Every claim carries a file and line so you can check it in under a minute, and I would rather be corrected than believed.

Two things worth stating plainly. First, I have no stake in this codebase and no relationship with whoever wrote it; nothing here is a judgement of the authors. Second, and unusually for a review of a young project: **the hard part of this codebase is done well.** I went looking specifically for nonce reuse, a weak KDF, and unauthenticated encryption, which is where hand-rolled crypto normally dies, and found none of them. The findings below are almost entirely in the plumbing *around* correct cryptography. See Section 8, which is not filler.

---

## Executive summary

**The cryptography is sound. The system built on top of it can lose all of your data, and in one case is guaranteed to.**

Wander(er) is a local-first photo manager that encrypts media before backing it up to a user's Telegram account. The feature set is broad and coherent: import and watch folders, timeline and album browsing, cloud-only storage with on-demand restore, map view, duplicate detection, face grouping, semantic search, ratings, tags, trash and restore. The crypto core is AES-256-GCM with Argon2id at 64 MiB, per-file random nonces from `OsRng`, a 160-bit recovery key, and a dual-wrapped master key. `tsc --noEmit` passes under `strict` with zero errors, zero `@ts-ignore`, and two `as any` in 10,915 lines.

Four defects make it unsafe to rely on as a backup tool:

1. **The encrypted backup cannot be decrypted.** `backup_database` copies `library.db`, encrypts the copy with the master key, and optionally uploads it to Telegram. But the wrapped master key exists **only inside `library.db`** (config row `security_bundle_v1`). The key material required to open the artifact is sealed inside the artifact. Lose the local disk, and neither the passphrase nor the recovery key can recover the backup, or any media in the Telegram archive. The entire disaster-recovery feature is a no-op precisely in the disaster it exists for.
2. **Encryption enforcement fails open to plaintext upload.** Every worker decides whether to encrypt by reading a *second*, duplicated config row `security_mode` with `.ok().flatten().unwrap_or("unset")`, so a read error or a missing row silently yields `should_encrypt == false` and the original file is uploaded to Telegram in the clear. The two rows are written by two non-transactional statements, and the UI derives "encrypted" from the *other* one, so the app will report the library as encrypted while shipping plaintext.
3. **A remote-control plugin ships in release builds.** `tauri-plugin-mcp-bridge` is registered unconditionally at `src-tauri/src/lib.rs:863`, with no `#[cfg(debug_assertions)]` gate, in a process that holds the decrypted master key in memory.
4. **The wrapped master key is handed to the webview**, by a `get_all_config` command with no key filter, under a CSP that permits `'unsafe-inline'` and `'unsafe-eval'` and an asset-protocol scope of `**`. The matching *write* path is correctly guarded against `security_*` keys, which makes this look like an oversight rather than a design choice.

Beyond those, "encrypted at rest" has a large asterisk: viewing anything in encrypted mode writes a fully decrypted copy into the OS temp directory, and **nothing ever deletes it**, including `lock_encryption`.

There is no CI, no `.github/` directory at all, so nothing type-checks, formats, lints, tests or builds on push. The installer the README links to is unsigned and there is no updater, meaning there is no mechanism to ship the fixes above to users who already installed it.

If this has been distributed to real users who enabled encryption, the backup defect (1) should be treated as an active incident: those users believe they have an off-site backup and do not.

### Measured state of the codebase

| Metric | Value |
| --- | --- |
| `npx tsc --noEmit` | passes (0 errors, `strict: true`) |
| `npx vite build` | succeeds, **876.57 kB** JS in a single chunk (255.52 kB gzip) |
| `cargo fmt --check` | **7 files** with formatting drift |
| `cargo build` / `cargo test` | **not run** (see limitations) |
| `npm audit` | **7 vulnerabilities: 5 High, 1 Moderate, 1 Low** (all build tooling) |
| CI workflows (`.github/`) | **0** |
| ESLint / Prettier / rustfmt.toml / clippy.toml configs | **0 / 0 / 0 / 0** |
| `lint` or `test` script in `package.json` | **none** |
| Rust unit tests / test modules | 8 / 7 |
| Frontend tests | **0** |
| Tauri IPC commands | 74 |
| `unwrap()` in Rust (non-test) | 7 |
| `panic!` / `todo!` / `unimplemented!` | 0 |
| `let _ =` discarded results | **80** |
| `println!` in Rust | 50 |
| `console.*` in frontend | 77 |
| `as any` / `@ts-ignore` / `@ts-expect-error` | **2 / 0 / 0** |
| `aria-label` in application code | **0** (2 total, both in generated shadcn) |
| Icon-only buttons (`size="icon"`) in app code | 26 |
| `onKeyDown` / `tabIndex` / `role=` in app code | **2 total** |
| `React.memo` usages | **0** |
| Declared but never imported deps (`react-window` and friends) | **4** |
| SQL statements built with string interpolation of free text | **1** |
| Schema migrations / committed `.sql` schema files | 19 / **0** |
| `CREATE INDEX` statements (2 of which are dead) | 7 |
| Committed but unused ONNX model | **1,244 KB** |
| Lockfiles committed | **2** (`package-lock.json` and `pnpm-lock.yaml`) |
| `LICENSE` / `SECURITY.md` | **absent / absent** |
| Version in `package.json` / `tauri.conf.json` / `Cargo.toml` | `0.0.0` / `0.0.0` / `0.0.0` |
| Code signing / updater configured | **no / no** |

### Severity definitions

| Severity | Meaning |
| --- | --- |
| **Critical** | Causes permanent data loss, silently defeats the app's central security promise, or exposes a remote-control or key-extraction path. Fix before further distribution. |
| **High** | Data loss or security degradation under realistic conditions, or a defect that makes the system unrecoverable or unmaintainable. |
| **Medium** | Meaningful risk, correctness defect, or reliability degradation in normal operation. |
| **Low** | Hygiene, maintainability, and defence in depth. |

---

## 1. Encryption and key management

I want to lead this section with the conclusion, because it is unusual: **the primitives are correct.** I specifically hunted for the four defects that kill hand-rolled file encryption (weak KDF, nonce reuse, unauthenticated ciphertext, non-CSPRNG randomness) and found none of them. Details and citations are in [Section 8](#8-what-is-already-good). Everything below is a failure in the layer that *decides when and whether* to use that crypto.

### 1.1 Encryption enforcement reads a duplicated flag and fails open to plaintext upload (Critical)

The authoritative crypto state is the `SecurityBundle` stored in config key `security_bundle_v1`. But every worker that decides "encrypt or not" reads a **second, separate** config row, `security_mode`:

```rust
// src-tauri/src/upload_worker.rs:115-121
let security_mode = db
    .get_config("security_mode")
    .ok()
    .flatten()
    .unwrap_or_else(|| "unset".to_string());
let should_encrypt = security_mode == "encrypted";
let mut upload_path = item.file_path.clone();
```

`.ok().flatten().unwrap_or("unset")` swallows **both** a database error and a missing row, and the result is `should_encrypt == false`, at which point `upload_path` remains the original file and it is uploaded to Telegram as-is (`upload_worker.rs:163-165`). The same fail-open pattern appears in `sync_worker.rs:60-66`, `watcher.rs:223-228`, `download_for_view` (`lib.rs:2110-2117`) and `backup_database` (`lib.rs:1907-1914`).

**This is reachable without an attacker.** The two rows are written by two independent, non-transactional `INSERT OR REPLACE` statements:

```rust
// src-tauri/src/lib.rs:105-115
fn save_security_bundle(db: &Database, bundle: &SecurityBundle) -> Result<(), String> {
    let json = serde_json::to_string(bundle).map_err(|e| e.to_string())?;
    db.set_config(SECURITY_BUNDLE_KEY, &json)
        .map_err(|e| e.to_string())?;
    let mode = match bundle.mode { /* ... */ };
    db.set_config(SECURITY_MODE_KEY, mode)
```

A crash, a power loss, or a transient DB error between those two calls leaves `security_bundle_v1 = encrypted` and `security_mode` absent.

**The failure is invisible.** `get_security_status` derives the user-facing state from the *bundle*, not from `security_mode` (`lib.rs:257-266`), so the UI reports "encrypted, unlocked" while the upload worker streams plaintext originals into cloud storage. Nothing verifies after the fact that the uploaded blob starts with the `WBENC1` magic.

Because `library.db` is an ordinary unencrypted SQLite file, any local process running as the user can also simply set `security_mode = 'unencrypted'` and silently disable encryption of all future uploads, with the UI still showing encryption as on.

**Fix:** delete `security_mode` as a decision input. Derive `should_encrypt` from `load_security_bundle()?.mode`, treat a read failure as **fail-closed** (defer the upload rather than send plaintext), and assert `FILE_MAGIC` on the artifact immediately before handing it to `upload_file_with_progress`.

### 1.2 Decrypted plaintext accumulates in the OS temp directory and is never deleted (High)

In encrypted mode, viewing anything writes a fully decrypted copy to the system temp directory. Two sinks, both permanent:

```rust
// src-tauri/src/lib.rs:166-190  (thumbnails)
let cache_dir = std::env::temp_dir().join("wanderer-thumb-cache");
let output = cache_dir.join(format!("{}.jpg", cache_key));
if needs_refresh && security::decrypt_file(&src, &output, &key).is_err() {
    return None;
}
```

```rust
// src-tauri/src/lib.rs:2178-2187  (full-size originals)
let materialized_dir = std::env::temp_dir().join("wanderer-view-cache-materialized");
std::fs::create_dir_all(&materialized_dir).map_err(|e| e.to_string())?;
```

The only cleanup routine in the codebase, `view_cache::cleanup_cache`, is pointed exclusively at the *encrypted* blob directory under the app data dir, and runs **once**, ten seconds after startup (`lib.rs:1077-1088`). Nothing ever deletes `wanderer-thumb-cache/`, `wanderer-view-cache-materialized/`, `wanderer-encrypted-uploads/`, `wanderer-download-staging/`, `wanderer-view-cache-staging/` or `wanderer-local-restore-staging/`. Locking the vault does not touch them either:

```rust
// src-tauri/src/lib.rs:360-364
async fn lock_encryption(state: State<'_, AppState>) -> Result<(), String> {
    state.security_runtime.lock().await.master_key = None;
    Ok(())
}
```

So the entire *viewed* portion of an "encrypted" library accumulates as plaintext in `%TEMP%` indefinitely, readable with no passphrase, after the user has locked the vault and closed the app. On Windows `%TEMP%` is per-user, which limits this to same-user access and is why this is High rather than Critical. On Linux and macOS, where the README says support is planned, `/tmp` is shared and files land with default permissions.

This does not make the README's claims false: `cache\thumbnails\` and `view_cache\` genuinely are encrypted (Section 5). But a reader would reasonably conclude their viewed media is not recoverable from disk, and it is.

**Fix:** track materialized paths, delete them in `lock_encryption`, on window close and on startup, and prefer serving decrypted bytes from memory through a custom protocol handler over writing plaintext files at all.

### 1.3 The Telegram session file is a full account credential stored with no protection (High)

```rust
// src-tauri/src/telegram.rs:105-112
let session_path = app_data_dir.join("session.db");
info!("Connecting to Telegram using session at: {:?}", session_path);
let session = SqliteSession::open(session_path)?;
```

No DPAPI wrapping, no master-key encryption, no ACL hardening. This is a strict inversion of value: the **low**-sensitivity API ID and hash get DPAPI (`lib.rs:429`), while the **high**-sensitivity MTProto authorization key does not.

*Inferred, not verified:* I did not read the `grammers-session` source (it is a git dependency), but `sqlite-storage` conventionally persists the DC address, user id and the MTProto `auth_key` in plaintext columns. If so, copying that one file grants full, silent, ongoing access to the victim's entire Telegram account, which includes every backed-up media file regardless of whether it was encrypted. Worth confirming against the dependency before deciding priority.

Credit where due: logout deletes the file carefully, with retries (`telegram.rs:488-514`). It is `remove_file` rather than a secure wipe, but the intent is right.

### 1.4 The file format authenticates chunks but not the file (High)

The header is `magic | version | chunk_size | base_nonce`, and the only AAD is the chunk index (`security/mod.rs:328-348`). Decryption loops until EOF and treats EOF as normal termination:

```rust
// src-tauri/src/security/mod.rs:410-415
loop {
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
        Err(e) => return Err(e.into()),
    }
```

Three consequences. **Truncation is undetectable:** deleting trailing chunks yields a shorter file that decrypts with zero errors, because there is no total-length field, no final-chunk flag, and no length in the AAD. **Substitution is undetectable:** nothing binds a blob to a media item, so with a single master key across all files, an attacker with write access to the Telegram account can swap photo A's blob for photo B's and both decrypt cleanly. Notably a `key_id` **is** generated and persisted (`security/mod.rs:112-114`) but is never written into a file header nor checked. **The header is unauthenticated:** `version` and `chunk_size` are read and trusted (`security/mod.rs:380-394`) without being covered by any AAD.

Compounding it, whether a blob is encrypted at all is decided by sniffing six magic bytes, with no reference to the DB's `is_encrypted` column, and plaintext is silently accepted and re-encrypted as though authentic:

```rust
// src-tauri/src/lib.rs:2150-2168
let downloaded_is_encrypted = security::is_encrypted_file(&raw_download_path)...;
let write_result = if downloaded_is_encrypted {
    std::fs::rename(&raw_download_path, &cache_blob_path)
} else {
    security::encrypt_file(&raw_download_path, &cache_blob_path, &key)  // accepted as genuine
```

So in encrypted mode with the vault unlocked, an attacker who substitutes a blob can inject arbitrary content that the app treats as authentic media. The AEAD provides confidentiality but, at the application level, effectively no authenticity for cloud-sourced data, because the application accepts "not encrypted" as a valid answer.

**Fix:** a v2 format that puts `magic || version || chunk_size || base_nonce || key_id || total_chunks` into the AAD, plus an explicit terminator chunk. When the bundle says encrypted and the row says `is_encrypted`, require the magic and hard-fail otherwise, then compare the decrypted content against the stored blake3 hash.

### 1.5 No zeroization; the master key is a `Copy` type fanned out across the process (High)

`zeroize` is not a direct dependency, and no `Zeroize`, `Zeroizing` or `ZeroizeOnDrop` appears anywhere in `src-tauri/src`. The master key is a plain `Copy` array in long-lived state:

```rust
// src-tauri/src/security/mod.rs:81-86
#[derive(Debug, Default)]
pub struct RuntimeState {
    pub master_key: Option<[u8; 32]>,
    pub migration: MigrationStatus,
    pub migration_worker_active: bool,
}
```

Because `[u8; 32]` is `Copy`, every read leaves an uncleaned copy behind: `lib.rs:204-206`, `upload_worker.rs:125`, `watcher.rs:231`, `sync_worker.rs:68` and `:303`. Worse, `lib.rs:496-531` copies the key and **moves it into a `tokio::spawn`'d migration task**, so a running migration holds a live plaintext key copy that `lock_encryption()` cannot reach. Also unzeroized: the derived KDF output, the unwrapped plaintext `Vec<u8>` from GCM, the passphrase arriving over IPC (`lib.rs:318`, `344`, `369`, `387`), and the recovery key string returned to the frontend.

The `#[derive(Debug)]` on `RuntimeState` is also one careless `{:?}` away from printing the raw master key to a log. I checked, and no such statement exists today.

**Fix:** add `zeroize`, wrap the key in a non-`Copy` `ZeroizeOnDrop` newtype so every copy becomes explicit and reviewable, take passphrases as `Zeroizing<String>`, and zeroize on lock.

### 1.6 Nonce carries 64 bits of entropy, not 96 (Medium)

To be explicit, because this is the finding people misread: **this is not nonce reuse.** The base nonce is fresh from `OsRng` per file, and within a file each chunk gets a distinct index with checked overflow. But:

```rust
// src-tauri/src/security/mod.rs:289-293
fn derive_chunk_nonce(base_nonce: &[u8; 12], chunk_idx: u32) -> [u8; 12] {
    let mut nonce = *base_nonce;
    nonce[8..12].copy_from_slice(&chunk_idx.to_le_bytes());
    nonce
}
```

The counter **overwrites** the last four random bytes, so the per-file random prefix is only bytes `0..8`, i.e. 64 bits. Two files collide if their 64-bit prefixes collide. At 2^20 encrypted files under one key the collision probability is around 3 x 10^-8, which is acceptable, but the margin is 2^32 times worse than a full 96-bit random nonce for no benefit, and **the master key is never rotated** (neither `recover_and_rewrap` nor `regenerate_recovery_key` re-keys).

**Fix:** store an 8-byte random `file_id` in the header, derive a per-file subkey from it, and use a full-width counter nonce. This composes neatly with the `key_id` work in 1.4.

### 1.7 A used recovery key is never invalidated, and there is no change-passphrase path (Medium)

Recovery re-wraps only the passphrase; the recovery wrap and its verifier are carried over untouched:

```rust
// src-tauri/src/security/mod.rs:160-166
let master_key = unwrap_master_key_with_secret(recovery_key.as_bytes(), &recovery.wrap)?;
let passphrase_wrap = wrap_master_key_with_secret(new_passphrase.as_bytes(), &master_key)?;
let mut next = self.clone();
next.passphrase_wrap = Some(passphrase_wrap);
```

A recovery key that was typed into a possibly-compromised machine, or left in `wanderer-recovery-key.txt` in the Downloads folder (`Onboarding.tsx:89-99`), remains a permanently valid master credential. And there is **no change-passphrase command** among the 74: the only way to change a passphrase is `recover_encryption`, which *requires* the recovery key. So a user who suspects their passphrase was shoulder-surfed must expose their recovery key in order to rotate it.

**Fix:** rotate the recovery wrap and verifier inside `recover_and_rewrap` and return a fresh recovery key; add `change_passphrase(old, new)`.

### 1.8 Passphrase policy and unlock throttling (Medium)

The only check is length, and it is inconsistent with what actually gets wrapped:

```rust
// src-tauri/src/security/mod.rs:100-102
if passphrase.trim().len() < 8 {
    return Err(anyhow!("Passphrase must be at least 8 characters"));
}
```

The check uses `trim().len()` while the wrap uses the untrimmed `passphrase.as_bytes()` (`security/mod.rs:107`), so `"1234567 "` is accepted and the trailing space becomes part of the secret, which is an easy way for a user to lock themselves out from a different keyboard. There is no strength estimation, no breach list, no character-class requirement, and `unlock_encryption` (`lib.rs:343-358`) has no attempt counter, backoff or lockout. Argon2id at 64 MiB is itself a strong rate limiter at roughly 0.1 to 0.3 seconds per guess, and the realistic threat is offline cracking of the on-disk wrap (Section 3.3) rather than online guessing, which is why this is Medium.

Note also that `recover_and_rewrap` accepts `new_passphrase` and wraps it with **no length check at all** (`security/mod.rs:143-166`), so the 8-character floor is enforced on the initialize path but not on the reset path. Hoist the check into one shared validator used by both.

### 1.9 DPAPI is used correctly but without secondary entropy (Medium)

The README's DPAPI claim is **true**, and the call is well formed: `CryptProtectData` with `CRYPTPROTECT_UI_FORBIDDEN`, user scope rather than `CRYPTPROTECT_LOCAL_MACHINE`, and `LocalFree` on both the output blob and the description string (`security/mod.rs:481-501`, `536-543`). The gap is that `pOptionalEntropy` is `null`, so **any** process running as the same user can call `CryptUnprotectData` on the blob lifted out of `library.db`. DPAPI here protects against offline theft of the file, not against same-user malware. Impact is bounded, since an api_id and api_hash are not account credentials, but the README wording may lead users to over-trust it, and 1.3 is the credential that actually matters.

### 1.10 Metadata is never encrypted (Medium)

`encrypt_file` covers media blobs, thumbnails and the backup artifact, but the live `library.db` is opened as a plain `rusqlite` database with no SQLCipher (`Cargo.toml:28` has no cipher feature). So filenames, full local paths, blake3 and perceptual hashes, extracted EXIF **including GPS coordinates**, face and person data, and album structure are all plaintext at rest, sitting alongside the wrapped-key material and the DPAPI blob. The README is admirably explicit that `backup/` is plaintext (Section 5) but does not mention that the metadata index is too, and for a photo library the GPS trail is arguably more sensitive than any single image.

---

## 2. Recoverability and data integrity

### 2.1 The encrypted backup is mathematically undecryptable (Critical)

This is the most serious finding in the report, and it is a design defect rather than a bug: the code does exactly what it says, and what it says is circular.

The master key is **random**, not derived from the passphrase:

```rust
// src-tauri/src/security/mod.rs:99-109
pub fn new_encrypted(passphrase: &str) -> Result<(Self, String, [u8; 32])> {
    let mut master_key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut master_key);
    let passphrase_wrap = wrap_master_key_with_secret(passphrase.as_bytes(), &master_key)?;
    let recovery_wrap = wrap_master_key_with_secret(recovery_key.as_bytes(), &master_key)?;
```

The only copy of that wrapped key, with its Argon2 salts, is the `SecurityBundle`, persisted into the `config` table **inside `library.db`** (`lib.rs:105-108`). Now the backup:

```rust
// src-tauri/src/lib.rs:1901-1922
std::fs::copy(&db_path, &backup_path).map_err(|e| e.to_string())?;
// ...
if security_mode == "encrypted" {
    let key = get_active_master_key(&state).await
        .ok_or_else(|| "Encryption vault is locked. Unlock to create encrypted backup.".to_string())?;
    let encrypted_path = backup_path.with_extension("db.wbenc");
    security::encrypt_file(&backup_path, &encrypted_path, &key).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&backup_path);
    final_backup_path = encrypted_path;
}
```

The backup of `library.db` is encrypted with the master key, and the wrapped master key lives only inside that same `library.db`. **The key material required to open the artifact is sealed inside the artifact.**

Walk the disaster scenario the feature exists for. The user's drive fails. They reinstall Wander(er) on a new machine. They have their passphrase and their printed recovery key, exactly as the README told them to. They have `library_backup_1234.db.wbenc`, and they have every photo in Telegram. And there is no sequence of actions that recovers any of it, because unwrapping the master key requires the salts and wrapped ciphertext that are inside the encrypted blob they are trying to open. The recovery key advertised at `lib.rs:83-91` recovers nothing in this scenario.

The same reasoning applies to the entire cloud archive: every uploaded media file is encrypted with that same master key (`upload_worker.rs:147-155`). **Lose `library.db` and the Telegram backup is cryptographically destroyed.** Note that `backup_database` will also happily upload the undecryptable artifact to Telegram when `upload_to_telegram` is set (`lib.rs:1927-1930`), so the off-site copy is off-site and useless.

**Fix, and it is small:** the `SecurityBundle` is *already* protected by Argon2id and the passphrase; that is its entire purpose. So export it **unencrypted** alongside the encrypted backup, or write it as a plaintext header on the `.wbenc` artifact. Then the passphrase and recovery key work as documented. This should be shipped before anything else in this report, and existing users should be told to keep a copy of their current `library.db`.

### 2.2 Filesystem deletions happen inside a transaction that can roll back (High)

`empty_trash` unlinks irrecoverable user files *before* the transaction commits:

```rust
// src-tauri/src/database.rs:2281-2295
let tx = conn.transaction()?;

for (id, file_path, thumbnail_path, telegram_media_id) in items {
    // Delete local file
    if std::path::Path::new(&file_path).exists() {
        let _ = std::fs::remove_file(&file_path);
    }
    // Delete thumbnail
    if let Some(ref thumb_path) = thumbnail_path {
        if std::path::Path::new(thumb_path).exists() {
            let _ = std::fs::remove_file(thumb_path);
        }
    }
```

If any later `tx.execute` fails on item N, the `?` propagates, the transaction **rolls back**, and the database still lists all N items as present while their bytes are gone. The user is left with a populated trash in which every entry is a dangling path. The two `let _ = std::fs::remove_file` calls also discard the failure case entirely.

`permanent_delete` has the mirror-image bug with **no transaction at all** (`database.rs:2234-2255`): file unlinked, then row deleted, so a failure between them leaves a row pointing at nothing.

**Fix:** collect paths, commit the transaction, *then* unlink. Irreversible side effects must never live inside a rollback scope.

### 2.3 SQLite runs without WAL or a busy timeout, and the backup is a raw copy of a live database (High)

There is no `journal_mode`, `busy_timeout` or `synchronous` pragma anywhere in the codebase. The only pragma at open time is:

```rust
// src-tauri/src/database.rs:151-157
let conn = Connection::open(path)?;
// Enable foreign keys
conn.execute("PRAGMA foreign_keys = ON;", [])?;
```

So the database runs in rollback-journal mode at default durability, with no configured wait on contention, while the upload worker, sync worker, AI worker and filesystem watcher all write through a shared `Arc<Database>`. Then `backup_database` takes a raw `std::fs::copy` of that live file (`lib.rs:1901`). If a write is in flight, the copy can capture a torn page set whose hot journal is not copied, producing a backup that will not open. Combined with 2.1, the user's disaster-recovery story is a possibly-corrupt file that they also cannot decrypt.

**Fix:** set `journal_mode = WAL` and a `busy_timeout` at open, and use `rusqlite`'s online backup API or `VACUUM INTO` instead of `fs::copy`.

### 2.4 Migration defects: a stale version variable, two destructive steps, and no committed schema (High)

There **is** a real versioned migration system keyed on `PRAGMA user_version`, with 19 numbered steps each in its own `BEGIN; ... COMMIT;` batch, and three of them correctly implement the full SQLite table-rebuild dance to repair foreign keys. That is better than most projects this age and is credited in Section 8. Four defects sit on top of it.

**(a) The `version` local is never updated after eight of the migrations.** Migration 5's update is commented out, literally:

```rust
// src-tauri/src/database.rs:317-321
                 PRAGMA user_version = 5;
                 COMMIT;",
            )?;
            // version = 5;
        }
```

The same omission occurs for migrations 7, 8, 9, 10, 11, 12 and 13. Because each gate is `if version < N` and the stale value *under*-estimates, today's effect is only "run more migrations than necessary", which the `IF NOT EXISTS` guards absorb. But `ALTER TABLE ... ADD COLUMN` is **not** idempotent, and several steps use it. This survives purely because those steps happen to be gated by a `version` still below their threshold. Anyone inserting a new migration between an assigning and a non-assigning step gets a duplicate-column error and a hard startup failure with no obvious cause. Only migration 12 does this properly, probing with `pragma_table_info` (`database.rs:416-443`).

**(b) Migration 7 drops the config table without carrying rows across:**

```rust
// src-tauri/src/database.rs:338-345
            conn.execute_batch(
                "BEGIN;
                 DROP TABLE IF EXISTS config;
                 CREATE TABLE config (
```

Today that only wipes app preferences, which is survivable. But it establishes a drop-and-recreate pattern in **the table that now holds the encryption bundle from 2.1**. If that pattern is ever repeated, it is unrecoverable key destruction for every user.

**(c) Migration 15 can delete every named person:**

```rust
// src-tauri/src/database.rs:512-517
                  DELETE FROM persons WHERE id NOT IN
                    (SELECT DISTINCT person_id FROM faces WHERE person_id IS NOT NULL);
```

If face embeddings were never computed, which is the **default state** because AI is opt-in and off, then `faces.person_id` is uniformly `NULL`, the subquery returns the empty set, `NOT IN` is true for every row, and all persons with their user-assigned names are deleted. There is no backup and no undo.

**(d) The schema is not committed anywhere.** There are zero `.sql` files in the repository. The only definition of the schema is roughly 480 lines of string literals inside `migrate()`, replayable only from version 0. There is no snapshot to diff against, no test that runs the chain, and no downgrade path. Commit a generated `schema.sql` and add a test that migrates 0 to 19 and asserts `pragma_table_info` for every table; that single test would have caught (a) and (c).

Also worth noting: the v5/v12 person rename left an orphaned `people` table that is never dropped, and migration 11's `CREATE TABLE IF NOT EXISTS tags` was a silent no-op on upgraded databases whose legacy `tags` table had a different shape, meaning **all tag writes failed** until migration 16 repaired it (`database.rs:525-592`). Migration 16 is genuinely careful work, and it is also evidence of how expensive (a) already was.

### 2.5 The full-text index is insert-only (High)

`media_fts` is written in exactly one place, and the result is discarded:

```rust
// src-tauri/src/database.rs:1069
let _ = conn.execute("INSERT INTO media_fts (file_path) VALUES (?1)", [file_path]);
```

Three consequences. The `let _ =` means a failed insert leaves that photo **permanently unsearchable, silently**. There is no `DELETE FROM media_fts` in `permanent_delete` or `empty_trash`, and no triggers exist anywhere, so the index accumulates rows for deleted media forever. And `add_media_synced` (`database.rs:1074-1105`), which is the sync-worker ingest path, never inserts into `media_fts` at all, so **every photo restored from Telegram is invisible to search**.

Because search joins on the text column rather than a rowid (`database.rs:1615-1616`), stale rows also resurrect as phantom joins if a path is ever reused, and neither side of that join is indexed (2.6).

**Fix:** an external-content FTS5 table with `INSERT`, `UPDATE` and `DELETE` triggers, which removes the manual call site entirely.

### 2.6 Missing indexes on the hottest columns (Medium)

There are 7 `CREATE INDEX` statements, and 2 of them died with the legacy `tags` table in migration 16. The effective indexes on `media` are the implicit unique index on `file_hash` and `idx_media_phash`. Unindexed and hot:

- **`media.file_path`**, the single most-queried key in the app. `upload_worker.rs:205-224` performs three lookups on it per completed upload, each a full table scan.
- **`media.is_deleted` and `is_archived`**, in the `WHERE` of essentially every gallery query.
- **`media.created_at` and `date_taken`**, the `ORDER BY` of every timeline query, so every page load sorts the whole table.
- **`media.telegram_media_id`**, hit once per Telegram message per sync cycle.
- **`media.scan_status`, `clip_status`, `tags_status`, `face_status`**, polled continuously by the AI worker.
- **`album_media.media_id`**: the primary key is `(album_id, media_id)`, so `WHERE media_id = ?` cannot use it.
- **`faces.media_id`**: `idx_faces_person` exists but not this, despite three separate query paths filtering on it.

Separately, there are **41** `conn.prepare(` calls and **zero** `prepare_cached`, so every query recompiles its plan on each invocation.

### 2.7 Non-atomic read-modify-write and lost idempotency (Medium)

`toggle_favorite` (`database.rs:2034-2047`) issues an `UPDATE ... SET is_favorite = NOT ...` and then a separate `SELECT` to report the new value, so a double-click can return a value contradicting the stored state and the UI renders the wrong icon. A single `RETURNING` clause fixes it.

`add_to_queue` (`database.rs:1688-1707`) is a check-then-act across two statements, and there is **no `UNIQUE` constraint on `upload_queue(file_path)`** in any of the 19 migrations, so nothing enforces deduplication at the schema level. The watcher asserts the opposite in a comment:

```rust
// src-tauri/src/watcher.rs:171
            // This is safe because database::add_to_queue now handles its own deduplication
            db.add_to_queue(&path_str)?;
```

That comment is wrong. The result is duplicate uploads of the same photo. `upload_worker.rs:80-98` adds a third defensive hash check at upload time, which reads like scar tissue from exactly this bug.

Relatedly, `upload_worker.rs` discards **seven** `update_queue_status` results. A dropped reset to `"pending"` leaves an item stuck in `"uploading"` forever, invisible both to `get_next_pending_item` and to the retry path, which only resets rows with `status = 'failed'`. There is no reaper for stale `uploading` rows.

---

## 3. Attack surface

### 3.1 A remote-control plugin is registered unconditionally in release builds (Critical)

```rust
// src-tauri/src/lib.rs:859-863
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_mcp_bridge::init())
```

```toml
# src-tauri/Cargo.toml:43
tauri-plugin-mcp-bridge = "0.7.0"
```

There is no `#[cfg(debug_assertions)]` gate and no feature flag. An MCP bridge exists to let an external agent process drive the application, and such plugins conventionally open a local listener. This ships, always on, in a process that holds the decrypted master key in memory and has commands that read arbitrary files and upload them to Telegram (3.4).

*Partially inferred:* the plugin is a crates.io dependency and I did not read its source, so I have not confirmed what it binds or whether it authenticates. What is **verified** is that it is registered in every build with no gate, and that it is conspicuously absent from `capabilities/default.json`, which suggests it was added during development and never removed. Capabilities gate frontend-to-Rust IPC; they do not gate a plugin's own listener.

**Fix:** gate it behind `#[cfg(debug_assertions)]` or delete it, then cut a new release. Until you have read the plugin's source and confirmed its bind address and auth model, treat this as the highest-priority item alongside 2.1.

### 3.2 The wrapped master key is handed to the JavaScript context (Critical)

The write path is guarded. The read path is not.

```rust
// src-tauri/src/lib.rs:1686-1690
async fn set_config(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    if key.starts_with("security_") {
        return Err("Security settings are managed by dedicated security commands".to_string());
    }
```

```rust
// src-tauri/src/lib.rs:1677-1684
async fn get_all_config(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_all_config().map_err(|e| e.to_string())
}
```

`get_all_config` returns every row of the `config` table with no filter (`database.rs:2856-2858`). That table holds `security_bundle_v1`, which is the Argon2 salts plus the AES-GCM-wrapped master key for **both** the passphrase and the recovery wraps, and `security_telegram_credentials`, the DPAPI blob. This is not a corner case: it is called from `src/lib/api.ts:211`, which is called from `Settings.tsx:164` and `MediaGrid.tsx:661`, so **every mount of the photo grid pulls the key material into JavaScript**.

That the `security_` prefix guard exists on the write path, and that all five security keys are correctly prefixed, is strong evidence the author understood this boundary. It was simply applied in one direction.

### 3.3 CSP allows inline script and eval, and the asset scope is the whole filesystem (High)

```json
// src-tauri/tauri.conf.json (app.security)
"csp": "default-src 'self' ipc: http://ipc.localhost; img-src 'self' asset: http://asset.localhost blob: data:; media-src 'self' asset: http://asset.localhost blob: data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-eval' 'unsafe-inline';",
"assetProtocol": { "enable": true, "scope": ["**", "C:\\**", "C:/**", "$APPDATA/**", "$LOCALAPPDATA/**"] }
```

`script-src 'unsafe-eval' 'unsafe-inline'` removes the main structural defence against script injection in a webview. `assetProtocol.scope` of `"**"` is unrestricted: the asset protocol will serve **any file the process can open** to the webview, on any platform. `capabilities/default.json:13-40` separately grants `fs:allow-read` and `fs:allow-exists` over `C:\**`, the entire system drive.

Individually each of these is a configuration smell. Together with 3.2 they compose into a real chain: any script injection, whether from a crafted EXIF field rendered unsafely or a compromised npm dependency, becomes arbitrary local file read **plus** exfiltration of the wrapped key material for offline Argon2 cracking, **plus** theft of the Telegram `api_hash`.

Mitigating, and worth stating: no `fs:allow-write*` and no `fs:allow-remove` are granted, and there is no `shell:` permission at all, so the plugin surface cannot write or delete. The write primitives live in custom commands instead (3.4).

**Fix:** drop `'unsafe-eval'` and `'unsafe-inline'`, narrow `assetProtocol.scope` and the `fs` scopes to `$LOCALAPPDATA/com.wanderer.desktop/**` plus the configured backup directory, and filter `security_*` out of `get_all_config`.

### 3.4 `import_files` is an arbitrary file read that auto-uploads to Telegram (High)

Of the 74 commands, five accept a caller-supplied filesystem path. The one that matters:

```rust
// src-tauri/src/lib.rs:1216-1244
async fn import_files(files: Vec<String>, app: tauri::AppHandle) -> Result<usize, String> {
    for file_path in files {
        let path = std::path::Path::new(&file_path);
        if let Some(file_name) = path.file_name() {
            let dest_path = backup_dir.join(file_name);
            // ...
            if let Err(e) = std::fs::copy(&path, &dest_path) {
```

No validation, no extension filter, no allowlist, no confinement to paths the user actually picked in a dialog. Any file the process can read is copied into the watched `backup` directory, where `watcher.rs:271-282` immediately ingests it and queues it for **upload to Telegram**. So one `invoke('import_files', { files: ['C:\\Users\\x\\.ssh\\id_rsa'] })` from injected script both reads the file and ships it off the machine. `dest_path` uses only `file_name()`, so there is no traversal on the write side; the read side is the problem.

Also unvalidated: `export_media` will `create_dir_all` and write into any destination (`lib.rs:1385-1387`), `backup_database` writes a DB copy anywhere, and `import_sync_manifest` reads and JSON-parses any path with parse errors returned to the frontend.

Credit where due, and I looked for this specifically: **there is no command that deletes a frontend-supplied path.** Both delete paths operate on values read out of the database, never on IPC input. That is the right design.

**Fix:** canonicalize and confine `import_files` sources to paths returned by the dialog plugin in the same session, and confine `export_media` and `backup_database` destinations likewise.

### 3.5 One SQL statement interpolates free text (High)

Of roughly 90 SQL statements, 87 bind every value. This is the exception:

```rust
// src-tauri/src/database.rs:1530-1537
        if let Some(camera) = &filters.camera_make {
            if !camera.is_empty() {
                conditions.push(format!(
                    "camera_make LIKE '%{}%'",
                    camera.replace('\'', "''")
                ));
            }
        }
```

The clause is joined and interpolated into both query bodies (`database.rs:1551-1559` and `1612-1621`), and the input is a free-text box: `Search.tsx:347-352` to `Search.tsx:118` to `api.ts:87` to `lib.rs:721-735`.

**Honest exploitability assessment.** Quote-doubling *is* the correct escape for a SQLite string literal, and SQLite honours no backslash escapes inside literals, so I could not construct an arbitrary-statement injection. What is live today is a **LIKE-pattern injection**: `%` and `_` are not escaped, so a user typing `%` matches every row, and a pattern like `%_%_%_%_%` forces pathological backtracking on a full table scan of an unindexed column.

The reason to fix it anyway is structural. This is a hand-rolled escaper on a concatenated clause: one future filter that forgets `.replace('\'', "''")` turns it into a real injection, and because the SQL text varies per filter combination it also defeats statement caching. Bind it, as the codebase already does correctly for its dynamic `IN (...)` lists.

There are no dynamic table names, no dynamic column names, and no interpolated `ORDER BY`, `LIMIT` or `OFFSET` anywhere in 3,264 lines. Album names, tag names, person names, config keys and file paths are all bound. And there is **no SQL outside `database.rs`** at all, which is a genuinely valuable containment property.

### 3.6 Unbounded allocation from an attacker-controlled length field (Medium)

```rust
// src-tauri/src/security/mod.rs:417-423
let ct_len = u32::from_le_bytes(len_buf) as usize;
if ct_len < 16 {
    return Err(anyhow!("Invalid encrypted chunk length"));
}

let mut ciphertext = vec![0u8; ct_len];
reader.read_exact(&mut ciphertext)?;
```

Only a lower bound is checked. The header's `chunk_size` **is** validated to at most 8 MiB (`security/mod.rs:392-394`) but that value is never used to bound `ct_len`. A malicious or corrupted blob fetched from Telegram can declare `0xFFFFFFFF` and force a roughly 4 GiB allocation per chunk, before any authentication tag is verified. One line fixes it: `if ct_len < 16 || ct_len > chunk_size as usize + 16`.

### 3.7 Unclamped pagination and an unchecked negative cast (Medium)

Nine query methods clamp defensively with `limit.max(0).min(1000)`. Two do not: `get_media_by_person` (`database.rs:2758-2777`) and `get_media_by_tag` (`database.rs:3074-3092`) pass frontend values straight through, so `limit: 10_000_000` materializes an unbounded result set into a `Vec` and serializes it over IPC.

Worse, in `semantic_search`:

```rust
// src-tauri/src/lib.rs:2469-2473
    let top_ids: Vec<i64> = scores
        .iter()
        .take(limit as usize)
```

`limit` is an `i32` from the frontend, and `-1i32 as usize` is `usize::MAX`, so a negative limit takes everything. Those IDs then flow into `get_media_by_ids`, which builds one `?` placeholder per ID, blowing past `SQLITE_MAX_VARIABLE_NUMBER`. `bulk_delete` and `bulk_set_favorite` have the same unbounded-placeholder exposure with an arbitrary-length `Vec<i64>`.

### 3.8 Migration leaves plaintext copies in Telegram and reports success anyway (Medium)

During migration to encrypted mode, the old plaintext message is deleted with the result thrown away:

```rust
// src-tauri/src/lib.rs:609-615
if let Ok(old_id) = previous_tg_id.parse::<i32>() {
    if old_id != new_msg_id {
        let _ = telegram.delete_messages(&[old_id]).await;
    }
}
```

`let _ =` swallows rate limits, network errors and partial deletes, and `delete_messages` itself only reports `pts_count`, which can be lower than requested. The migration is then marked `succeeded` regardless (`lib.rs:622-623`). So plaintext originals can remain in Telegram cloud storage forever, with no record of which ones, while the app reports the library as fully migrated to encrypted. For a user who enabled encryption specifically to remove plaintext from Telegram, this silently fails to deliver the thing they asked for.

**Fix:** verify the deletion, retry on `FLOOD_WAIT`, and record un-deleted message IDs in a durable pending-purge list surfaced in the UI.

---

## 4. Rust code quality

### 4.1 A single panic bricks the database for the process lifetime (High)

The doc comment and the log message both claim recovery. The code does not recover:

```rust
// src-tauri/src/database.rs:138-148
    /// Get a connection, recovering from poisoned mutex if needed.
    pub fn get_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| {
            // Recover from poisoned mutex - the previous holder panicked
            log::warn!("Recovering from poisoned database mutex");
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("Mutex poisoned: {}", e)),
            )
        })
    }
```

This maps poison to `Err` and returns it. `std::sync::Mutex` poisoning is **permanent**, so after one panic anywhere in a DB call, all 89 database methods fail forever with "Mutex poisoned" and all 74 commands degrade to error strings until the app is restarted. The honest fix is one call: `.unwrap_or_else(|e| e.into_inner())`, which is what "recovering" actually means. At absolute minimum, correct the comment so the next reader is not misled.

The most likely trigger is debug code left in a production write path:

```rust
// src-tauri/src/database.rs:696-708
            // DEBUG: Check FK definition
            let mut stmt = conn.prepare("PRAGMA foreign_key_list('faces')")?;
            let fks = stmt.query_map([], |row| { /* ... */ })?;
            for fk in fks {
                println!("DEBUG FK: faces -> {}", fk.unwrap());
            }
```

That `fk.unwrap()` runs **while the connection guard is held**, so any row-decode error panics with the lock held and poisons it permanently. The whole block also executes on every single face embedding stored, running an extra `PRAGMA` query and printing to stdout per face.

### 4.2 The global lock is held across blocking work and CPU-bound inference (High)

There are two lock layers: `AppState.db: Mutex<Option<Arc<Database>>>` (a **tokio** mutex) wrapping `Database.conn: Mutex<Connection>` (a **std** mutex).

The std guard never crosses an `await`. Every one of the 89 methods acquires and releases inside a synchronous body. **That is the single most important thing to get right with `Mutex<Connection>` in an async application, and it is right everywhere**, which is why this is High rather than Critical.

The tokio guard is the problem:

```rust
// src-tauri/src/lib.rs:2513-2542  (index_pending_clip)
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    let pending = db.get_pending_clip_items(limit).map_err(|e| e.to_string())?;
    for (id, path_str) in pending {
        // ...
        match clip::encode_image(path) {
```

`clip::encode_image` is synchronous ONNX inference. This holds the global DB lock for the entire batch **and** blocks an executor thread, so no other command can touch the database and no other task on that thread progresses. `detect_faces` gets this right using `spawn_blocking` (`lib.rs:811-814`); this does not.

The per-request equivalent is `materialize_media_items_for_response` (`lib.rs:193-202`), which loops over items and, per item, does a blocking `is_encrypted_file` read, a **fresh async lock acquisition**, `create_dir_all`, two `metadata` calls, and a full `decrypt_file`. For a 200-item gallery page that is 200 async lock round-trips and up to 200 synchronous AES-GCM file decryptions on a runtime thread, and it runs on the return path of about a dozen commands. `lib.rs` contains **39** `std::fs::` calls, nearly all inside `async fn` bodies with no `spawn_blocking`.

No deadlock is reachable today, but the lock *ordering* is inconsistent: `lib.rs:511` takes `security_runtime` and then the DB lock, while `sync_worker.rs:60-68` takes the DB lock and then `security_runtime`. That inversion is currently harmless only because the locks are held so briefly. Making any DB method hold its guard longer would turn it into a deadlock.

### 4.3 Duplicate detection is O(n squared) with two allocations per comparison, under the global lock (High)

```rust
// src-tauri/src/database.rs:2653-2660
        for i in 0..n {
            for j in (i + 1)..n {
                let distance = hamming_distance(&candidates[i].1, &candidates[j].1);
```

and `hamming_distance` re-parses **both** hashes from base64 into an `ImageHash` on every call (`database.rs:113-131`). At 10,000 photos that is roughly 50M iterations with about 100M heap allocations, all while holding the connection guard taken at `database.rs:2567` for the entire function, so the UI and every background worker stall. The function also loads every candidate row with all 24 columns into memory first.

**Fix:** parse each hash once into a `Vec<u64>` up front, and bucket by phash prefix instead of comparing all pairs.

Related performance findings: **four N+1 query patterns** (`export_sync_manifest` at `lib.rs:2291-2302` runs two correlated subqueries per photo over the whole library; `scan_duplicates` at `lib.rs:1562-1573` and `find_duplicates` at `lib.rs:1504-1514` both reacquire the global lock *inside* the loop with one implicit transaction and one fsync per photo; `reconcile_cloud_only_flags` at `database.rs:2436-2441` does a blocking `Path::exists()` plus an individual `UPDATE` per candidate **at startup**). And `semantic_search` loads every CLIP embedding in the library into memory on each search, which the author flagged honestly in a comment at `lib.rs:2452-2453`.

### 4.4 Eighty discarded results, several hiding real state corruption (Medium)

There are **80** `let _ =` and **32** `.ok()` sites. Most are legitimately fire-and-forget, such as `app.emit` and temp-file cleanup. These are not:

- `database.rs:1069`: the FTS insert, which silently loses searchability (2.5).
- `lib.rs:2349-2356`: the entire `import_sync_manifest` merge writes through discarded results, and `updated_count` increments regardless, so the command reports `"Synced N items"` after failing all N of them.
- `upload_worker.rs`: seven discarded `update_queue_status` calls, which can strand items in `"uploading"` forever (2.7).
- `sync_worker.rs:356`: a discarded `mark_media_encrypted_by_path`, after which the item is treated as unencrypted forever and the migration will re-upload it.
- `database.rs` has **eight** `filter_map(|r| r.ok())` sites that silently drop unreadable rows mid-iteration. At `2274` in `empty_trash` that means a trashed item is skipped and never deleted; at `2920` in `get_all_media_for_sync` it means an item silently vanishes from the sync manifest.

### 4.5 `errors.rs` is dead code and all 74 commands return `Result<T, String>` (Medium)

`errors.rs` defines a well-structured `AppError` with `#[derive(Error, Serialize)]`, a `#[serde(tag = "type", content = "message")]` representation, seven variants, and `From` impls for `rusqlite::Error` and `std::io::Error`. It is referenced **zero** times outside its own file. Every command instead does `.map_err(|e| e.to_string())`, which flattens typed errors into opaque strings the frontend cannot branch on and leaks raw SQL and IO error text, including absolute filesystem paths, into the UI.

This has a concrete downstream cost: `App.tsx:50` retries startup by **string-matching** `message.includes("Database not initialized")`, a contract that breaks the moment the Rust error text changes (6.3). Wiring up the type that already exists would fix both ends.

### 4.6 Structure and duplication in `database.rs` (Medium)

`database.rs` is 3,264 lines with **89 public methods** across **three separate `impl Database` blocks** (lines 137, 2820, 2872) with no module boundary between them, two of which are unlabelled continuations. It mixes schema migration, media CRUD, queue management, albums, tags, face clustering, CLIP vector storage, config storage and duplicate detection, and it embeds two clustering *algorithms* (union-find at `2626-2683`, greedy face matching at `2743-2822`) in the data-access layer.

The duplication is measurable and the fix is unusually cheap. The 24-field `MediaItem` row mapping is written out inline **17 times**, and the long `SELECT id, file_path, file_hash, ...` column list is duplicated **15 times**. A `map_media_row` helper **already exists** at `database.rs:1401-1434` and is used exactly **3 times**. Applying the existing helper at the other 17 sites would delete roughly 500 lines with no behaviour change, and it is the single highest-value, lowest-risk refactor available in this codebase. Because the column list is manual and positional, adding a column today means editing 15 strings and 17 mappings in lockstep; the defensive `row.get::<_, Option<i32>>(21)?` pattern on the newer columns reads like scar tissue from that exact failure.

`lib.rs` at 2,545 lines holds all 74 command handlers plus startup wiring in one file, and would split cleanly along the same domain lines.

### 4.7 Smaller Rust findings (Low)

- **`progress_stream.rs:66`**: `self.bytes_read.try_lock().unwrap()` inside `poll_read`, on the upload hot path. It should never contend, but the panic would land inside an in-flight Telegram upload. It is also unnecessary: the method takes `Pin<&mut Self>`, so a plain `u64` field removes both the `Arc<Mutex<_>>` and the panic.
- **`escape_like_pattern` does not work.** `media_utils.rs:252-256` escapes `%` and `_` with backslashes, but no query anywhere uses an `ESCAPE '\'` clause, and SQLite only honours a backslash escape when one is specified. So the function fails to neutralize wildcards *and* inserts literal backslashes that make a search for `my_photo` fail to match `my_photo`. Its unit tests assert the string transformation rather than the SQL behaviour, so they pass while the feature is broken. Mitigating: the only caller, `Database::search_media`, is dead code, since the `search_media` command routes to `search_fts` instead. `Database::get_persons` is likewise dead.
- **FTS5 query construction can produce syntax errors on ordinary input.** `database.rs:1606-1610` maps each token to `"\"{}\"*"`, so a token consisting only of `"` becomes `""*`, an FTS5 syntax error surfaced raw to the user. Correctly bound as a parameter, so not injection.
- **`sync_worker.rs:148`**: `.unwrap()` on `to_str()` of a path, inside a spawned worker, where a panic silently kills sync for the session. Every other path conversion in the codebase uses `to_string_lossy()`.
- **The remaining 6 non-test `unwrap()`s are safe by invariant**, and I want to be fair about that: `database.rs:778` and `920` are guarded by preceding length checks, `793` is reachable only when a preceding comparison assigned the value, and `ai/worker.rs:48` unwraps a runtime build inside a dedicated thread, which is conventional.
- **`unchecked_transaction` used once inconsistently** (`database.rs:3224`) where the other five transactions use the checked `conn.transaction()`, apparently only to avoid a `mut` binding.
- **50 `println!` calls** bypass the initialized `env_logger` entirely and go to a stdout that the `windows_subsystem = "windows"` release attribute discards. Several log user file paths and IDs.
- **`cargo fmt --check` reports drift in 7 files**: `ai/object_detection.rs`, `ai/worker.rs`, `clip.rs`, `database.rs`, `lib.rs`, `security/mod.rs`, `upload_worker.rs`.
- **A non-cryptographic RNG exists in the tree.** `sync_manifest.rs:212-231` seeds an unkeyed `DefaultHasher` from a timestamp. I traced every use: it feeds only `generate_device_id()` and touches **no** key, nonce, salt or recovery key. Harmless today, but it is a foot-gun sitting next to a crypto module.
- **Dead `.env` machinery.** `dotenvy::dotenv().ok()` is called at `lib.rs:840` and `.env.example` advertises `TG_ID` and `TG_HASH`, but neither is ever read; credentials come exclusively from the DPAPI-protected config. Meanwhile **`.env` is not gitignored** in either `.gitignore`, so a developer following `.env.example` would have real credentials staged by default, for no functional benefit.
- **`windows-sys` is not target-gated** (`Cargo.toml:63`), and the non-Windows DPAPI stubs hard-error, so onboarding cannot complete on Linux or macOS while `bundle.targets` is `"all"`. The stubs correctly **fail closed** with no plaintext fallback, which is the right behaviour; the packaging is what is wrong.

---

## 5. README accuracy

I audit documentation separately because a user-facing README that makes security promises is part of the security posture: a false promise there causes users to take real risks. I checked 51 concrete factual claims against the code.

**The result is unusually good.** 34 claims verify as fully true, including **every one of the six security claims**, and including the self-critical one. Two are false, both about distribution rather than behaviour.

### 5.1 The six security claims all verify (context, and this is the headline)

| Claim | Verdict | Evidence |
| --- | --- | --- |
| "Files are encrypted before Telegram cloud upload" | **True** | `upload_worker.rs:146-155` encrypts to a temp `.wbenc` before `upload_file_with_progress`. See 1.1 for the fail-open caveat. |
| "Thumbnails are encrypted at rest" | **True** | `watcher.rs:231-246`, and it deletes the plaintext thumbnail rather than leaving it when the vault is locked. |
| "View cache is encrypted at rest" | **True** | `lib.rs:2150-2168` writes only `.wbenc` blobs into `view_cache/`. |
| "Database backup artifact is encrypted" | **True** | `lib.rs:1914-1922`. True, and unfortunately also finding 2.1. |
| "API ID and hash are stored locally with Windows DPAPI" | **True** | Real `CryptProtectData` with `CRYPTPROTECT_UI_FORBIDDEN` and user scope, `security/mod.rs:481-501`. See 1.9 for the entropy caveat. |
| "Local files in `backup/` are still plaintext at rest" | **True** | Correct, and **voluntarily disclosed**. `encrypt_file` is never applied to the `backup/` tree. |

That last row deserves emphasis. A README that spends a section explaining what its own encryption does **not** protect, accurately, without being asked, is rare. It materially raises my confidence in the rest of the document.

All seven documented storage paths under `%LOCALAPPDATA%\com.wanderer.desktop\` are exactly right. "AI is opt-in, default OFF" is true and enforced in the schema (`database.rs:349-350`, `606-607` seed `ai_face_enabled` and `ai_tags_enabled` to `'false'`). The onboarding flow, the one-way encryption warning, the recovery-key verification step, the `tg://` share-link caveat, the partial RAW support, the unimplemented mobile companion and the incomplete metadata preservation are all accurate. Several are hedged more carefully than they needed to be.

### 5.2 The download links point at the wrong repository (High, documentation)

```
README.md:34-36
- Releases page (all versions): https://github.com/ronimuliawan/Wanderer/releases
- Direct download (Windows x64, v0.0.0): https://github.com/ronimuliawan/Wanderer/releases/download/0.0.0/Wanderer._0.0.0_x64-setup.exe
- Direct download (Windows x64, latest): https://github.com/ronimuliawan/Wanderer/releases/latest/download/Wanderer._0.0.0_x64-setup.exe
```

The repository's actual remote is `https://github.com/rons-space/Wanderer`. All three download links point at a **different owner namespace**. Whether that namespace resolves, redirects, or 404s cannot be determined from inside the repository, but the URLs do not match this project either way.

For most projects this would be a Medium typo. Here it is High, because the artifact being distributed is an **unsigned Windows installer** (7.2) for an application that will hold the user's entire photo library and a Telegram account credential. "Download this unsigned .exe from a GitHub namespace that is not the project's" is precisely the shape of a supply-chain phishing instruction, and a user has no way to tell the difference. If `ronimuliawan/Wanderer` is a legitimate second remote, say so explicitly in the README; if it is stale, fix it.

Related, and in the same spirit: the one populated in-app link points at a **third** project name.

```ts
// src/components/Settings.tsx:49-53
const ABOUT_LINKS = {
    github: "https://github.com/ronimuliawan/wanderbackup-rust",
```

So the repository, the README's download links, and the app's own About tab name three different GitHub locations. The empty `telegramChannel`, `supportGroup` and `donate` values are handled gracefully, rendering "Not configured yet" with disabled buttons, which matches the README's honest "(if configured)" hedge.

### 5.3 "Production build: `npm run build`" is false (Medium, documentation)

```
README.md, For Developers
Production build:
    npm run build
```

`"build": "tsc && vite build"` (`package.json:8`) type-checks and bundles **the frontend only**, emitting `dist/`. I ran it: it succeeds in 2.71 seconds and produces no application. The production build is `npm run tauri build`, which is never mentioned. This will waste every new contributor's first hour, and it is the reason the developer-facing section is the weakest part of an otherwise strong document.

### 5.4 Claims that are true but materially incomplete (Medium, documentation)

- **"Thumbnails / view cache are encrypted at rest"** is true of the documented directories, but omits that viewing anything writes an unencrypted copy into `%TEMP%` that is never cleaned up (1.2). A reader will conclude their viewed media is not recoverable from disk, and it is. This is the most important omission in the file.
- **"If you lose both passphrase and recovery key, encrypted data is unrecoverable"** is true, but the README does not warn about the case that actually bites: losing `library.db` while *retaining* both secrets is **also** unrecoverable (2.1). The document tells users to safeguard the two secrets and never tells them to safeguard the file that the secrets are useless without.
- **"Minimum 8 chars"** is enforced on the initialize path (`security/mod.rs:100-102`) but **not** on the recovery/reset path, which accepts any `new_passphrase` (1.8).
- The README does not mention that the metadata index, including GPS coordinates, is plaintext (1.10).

### 5.5 Audit summary

| Verdict | Count |
| --- | --- |
| True | 34 |
| True but materially incomplete | 5 |
| True with a backend enforcement gap | 1 |
| True but the cited link is wrong | 1 |
| **False** | **2** (production build command; release URLs) |
| Unverifiable from the repository | 4 |

Failures cluster in exactly two places: developer-facing instructions, and distribution metadata. Neither is about the product's behaviour, which is why this section reads so differently from Sections 1 through 4. The user-facing security documentation is accurate and, in places, more forthcoming than it had to be.

**Fix:** correct the two false claims, add the `%TEMP%` residue and the `library.db` dependency to the Security and Privacy section, and add a "back up your `library.db`" instruction that is at least as prominent as the recovery-key instruction. Once 2.1 is fixed, replace that with the new procedure.

---

## 6. Frontend

### 6.1 The Settings path to enable encryption can silently destroy recoverability (High)

There are **two** paths that enable encryption, and they diverge badly. Onboarding does it correctly (see Section 8). Settings does not:

```tsx
// src/components/Settings.tsx:601-609
{generatedRecoveryKey && (
    <div className="space-y-2 rounded-md border bg-muted p-3">
        <Label>Recovery Key (shown once)</Label>
        <p className="font-mono text-xs break-all">{generatedRecoveryKey}</p>
        <p className="text-xs text-muted-foreground">
            Save this key securely. It is required if passphrase is lost.
        </p>
    </div>
)}
```

Compared with the onboarding flow, this is missing **everything that makes the onboarding flow safe**. There is no verification step, so nothing forces the user to have actually read the key; onboarding requires retyping two segments before proceeding (`Onboarding.tsx:170-194`). There are no Download, Print or Copy buttons, so the user must manually select the text. And the state is never cleared, so despite the "(shown once)" label nothing enforces that. A user can enable encryption from Settings, navigate away without reading the key, and has now permanently lost the ability to recover if they forget the passphrase.

This is a direct consequence of the duplication in 6.6: the same security-critical operation implemented twice, once carefully.

**Fix:** extract one `<EnableEncryption>` component containing the verification gate and the save affordances, and use it in both places.

### 6.2 Recovery-key handling defects in the onboarding flow (High)

Three separate issues around the one-time display of an unrecoverable secret:

**The print window is never closed and fails silently.**

```tsx
// src/components/Onboarding.tsx:101-111
const printRecoveryKey = () => {
    if (!recoveryKey) return;
    const printWindow = window.open("", "_blank", "width=700,height=500");
    if (!printWindow) return;
    printWindow.document.write(
        `<pre style="...">Wander(er) Recovery Key\n\n${recoveryKey}\n\n...</pre>`,
    );
    printWindow.document.close();
    printWindow.focus();
    printWindow.print();
};
```

After `print()` returns, or if the user cancels, a window containing the plaintext master-recovery secret stays open on the desktop indefinitely. And `if (!printWindow) return` is a **silent** no-op: in a Tauri WebView2 window with `decorations: false`, `window.open` is likely blocked, so the user clicks Print, nothing happens, no toast, no error. For the only display of an unrecoverable key, a silent no-op is a data-loss path. Every other failure in this file produces a `toast.error`.

**The clipboard copy has no error handling.** `Onboarding.tsx:526-528` calls `navigator.clipboard.writeText(recoveryKey || "")` with the promise unhandled and no success toast, so a permission failure is indistinguishable from success. A safe helper already exists in this codebase: `Settings.tsx:240-251` wraps the same call in try/catch and toasts both outcomes.

**The download revokes the blob URL synchronously after `click()`** (`Onboarding.tsx:89-99`), and the anchor is never appended to the document. Both work in current WebView2, but this is a known-fragile idiom for what is again a one-shot secret.

### 6.3 Startup retries forever by string-matching a Rust error message (High)

```tsx
// src/App.tsx:43-63
  const refreshSecurityStatus = async () => {
    try {
      const status = await api.getSecurityStatus();
      // ...
    } catch (e: any) {
      const message = String(e);
      if (message.includes("Database not initialized")) {
        setTimeout(() => { refreshSecurityStatus(); }, 250);
        return;
      }
```

Four problems in twenty lines. The timeout handle is never captured or cleared, so the effect has no cleanup and under React 19 `StrictMode` (`main.tsx:11`) two independent 4 Hz polling chains start in development. There is **no retry cap and no timeout**, so if the Rust side never initializes the database the app sits on "Loading secure startup..." polling IPC forever with no user-visible error and no way out. The retry decision is made by string-matching an error message, a contract that breaks the moment the Rust text changes, which is the downstream cost of the dead `errors.rs` in 4.5. And `catch (e: any)`.

### 6.4 The photo grid remounts every visible cell on every parent render (High)

`Gallery.tsx:158` and `Trash.tsx:124` define `ItemWrapper` components **inside** render:

```tsx
// src/components/Gallery.tsx:158
    const SelectableItemWrapper = ({ item, children }: { item: MediaItem; children: React.ReactNode }) => {
        const isSelected = selectedIds.has(item.id);
```

Both are new function identities on every render, and both are passed as the `ItemWrapper` **component type** prop, which `MediaGrid.tsx:476` uses as a JSX element. React compares element types by reference, so a new type means unmount and remount of that subtree. In `Gallery`, `selectedIds` changes on every click, so **every selection toggle tears down and rebuilds every visible cell's DOM**, including the `<img>` elements, which re-enter the network and decode path.

This compounds with two more issues. `VirtualGrid.handleScroll` triggers **two** state updates per scroll event (`MediaGrid.tsx:260`, `855`), one of them unconditionally. And there are **zero** `React.memo` usages in the entire codebase, while `MediaGrid` passes ten `on*` handler props with none wrapped in `useCallback`. So every scroll frame re-renders the grid and every visible `Cell`, and each `Cell` re-runs `convertFileSrc` and rebuilds a full Radix context menu subtree with a six-item rating submenu.

Also: the grid cell key is an **absolute index** into the array (`MediaGrid.tsx:335`, `key={itemIndex}`) while `handleDelete` and `handleArchive` splice the array, so every item after a removal shifts index and React reuses the wrong DOM node and image for a different photo. `DuplicateReview.tsx:206` has the same hazard.

**Fix:** hoist the wrappers to module scope, `memo()` the `Cell`, `useCallback` the handlers, key by `item.id`, and throttle scroll state through `requestAnimationFrame`.

### 6.5 Zero accessibility affordances (High)

There are **0** `aria-label` attributes in application code (2 in the repository, both in generated shadcn files) against **26** `size="icon"` buttons and 9 raw `<button>` elements. A screen reader announces the favourite toggle, the eight repeated Copy and ExternalLink pairs in the About tab, and the mobile menu control as just "button".

Keyboard navigation is absent from the core surface. The clickable grid cell is a `<div onClick=...>` (`MediaGrid.tsx:412-415`) with no `tabIndex`, no `role` and no `onKeyDown`, and across the whole application there are exactly **2** occurrences of `onKeyDown`, `tabIndex` or `role=` combined. **The photo grid is entirely unreachable by keyboard.** `MediaViewer` has no arrow-key navigation, and structurally cannot: it receives a single `item` prop rather than a list and an index. Escape and focus trapping work only because Radix `Dialog` provides them.

The only accessible-by-accident controls are the window buttons, which use `title` attributes. Enabling `eslint-plugin-jsx-a11y` (7.3) would catch most of this mechanically.

### 6.6 Dead code, triplicated flows, and a hand-rolled event bus (Medium)

**Three separate implementations** of the Telegram phone-and-code login exist: in `Onboarding.tsx:218-252`, in `Settings.tsx:316-346`, and in `LoginView.tsx:20-49`. `LoginView.tsx` is **never imported or rendered anywhere**, and it is also the only file in the app that calls `invoke()` directly rather than going through the typed `api.ts` layer, plus the only source of "Check console" error strings and a `"Log out (Stub)"` button. Also fully dead: `Sidebar.tsx` (147 lines, superseded by `AppSidebar.tsx`, still containing a 2-second polling loop and the app's only two `alert()` calls) and `ThemeSwitcher.tsx` (57 lines, now inlined into Settings).

`Settings.tsx` at 1,302 lines holds **17 `useState` hooks** spanning five unrelated domains, does six independent fetches on mount, and returns one 940-line block of JSX. Its About tab alone is roughly 140 lines of four copy-pasted link rows differing only in icon, label and URL.

There is **no state management library and no server-state cache**. Cross-cutting auth state is propagated through a hand-rolled pub/sub over `window`:

```tsx
// src/components/Settings.tsx:144
window.dispatchEvent(new Event('auth-changed'));
// src/components/AppSidebar.tsx:134
window.addEventListener('auth-changed', checkUser);
```

`MediaGrid` refetches `getAllConfig()` **and** `getAlbums()` on every mount, and it is mounted by seven different parents, so switching views reissues both calls every time. `getAlbums()` is independently fetched in three places.

The `loadNextPage` pagination block is copy-pasted across **seven** components in **two mutually incompatible variants**. `Favorites.tsx` and `Archive.tsx` are byte-for-byte identical except for one API method name and two log strings. The variant used by `Favorites`, `Archive` and `Trash` appends without de-duplicating by id, so any overlapping page, which fixed-offset pagination trivially produces when an item is deleted mid-scroll, yields **duplicate React keys**. This is one `useMediaPagination(fetcher)` hook.

### 6.7 Error handling and other frontend findings (Medium / Low)

- **The error boundary does not cover the whole app.** `ErrorBoundary` is correctly mounted inside `App` (`App.tsx:81-108`), but `main.tsx:12` mounts `ThemeProvider` **outside** it. `ThemeProvider` reads `localStorage` in six lazy initializers and throws from `useTheme` (`ThemeContext.tsx:186`), so a failure there escapes the boundary and produces a blank white window. The fallback also has **no reset or reload button**, which in a desktop app leaves killing the process as the only recourse, and it renders raw error text including absolute paths. Nothing is persisted or reported.
- **Tauri event listener cleanup is broken in two places.** `Settings.tsx:125-133` and `Gallery.tsx:60-87` assign the unlisten function inside a `.then()`, so unmounting before the promise resolves leaves the listener registered forever, calling `setState` on an unmounted component. The correct pattern **already exists** in this codebase at `UploadQueue.tsx:80-86`, which unlistens through the stored promise.
- **`MediaGrid` mirrors props into state** (`MediaGrid.tsx:648-657`), which double-renders on every load and actively fights the four optimistic-update handlers in the same file: they mutate `localItems`, then trigger a parent refetch, which then overwrites the local edit.
- **A scroll timeout is never cleared** (`MediaGrid.tsx:861`), firing `setState` after unmount.
- **`Search.tsx:82-94`** has a `useEffect` whose dependency array lists only `[selectedTag]` while the closure reads `query`, `hasSearched` and four filter states, and whose body contains five lines of question-mark comments admitting the control flow is not understood.
- **Errors swallowed:** four bare `.catch(console.error)` sites where a failed `getAlbums()` renders as "No albums", and three places where an IPC failure is rendered identically to "logged out", including `Settings.tsx:310-313` which silently boots the user to the login form on any transient error.
- **The error banner is unreadable.** `Settings.tsx:426` styles it `bg-red-50 text-red-500`, raw Tailwind palette values rather than the app's semantic `destructive` token, while the app defaults to dark mode (`index.html:2`).
- **Three confirmation idioms** coexist: native `confirm()` (`Settings.tsx:137`, `BulkActionBar.tsx:116`), the Tauri dialog plugin's `ask()` (`AppSidebar.tsx:140-144`), and a Radix `AlertDialog` (`Trash.tsx:148-187`).
- **`BulkActionBar` fires N sequential IPC calls** for "Cloud Only" (`BulkActionBar.tsx:125-133`) while every other bulk action in the same file uses a real batch command. Selecting 2,000 photos means 2,000 serialized round-trips behind one spinner.
- **Non-virtualized grids elsewhere** load 100 full images at once with no `loading="lazy"` (`SmartAlbums.tsx`, `Tags.tsx`, `DuplicateReview.tsx`); only 2 `loading="lazy"` attributes exist in the app. `MapView.tsx:35` fetches 500 rows in one shot.
- **The map cannot render.** `MapView.tsx:91-92` loads OpenStreetMap tiles over `https:`, and `MapView.tsx:15-17` loads Leaflet marker icons from `unpkg.com`, but the CSP's `img-src` is `'self' asset: http://asset.localhost blob: data:` with no `https:`. So the map view shows markers and tiles blocked. Silver lining: this also means photo GPS coordinates are **not** currently leaking to a third-party tile server. Decide which behaviour you want, and if you want tiles, vendor the marker assets locally and document the privacy tradeoff.
- **Dead and duplicated UI**: a doubled `<ContextMenuSeparator />` (`MediaGrid.tsx:576-578`), a `theme` prop threaded through two components only to be explicitly ignored (`MediaGrid.tsx:158`), a permanently disabled "Log Out (Not Implemented)" button directly beneath a working "Disconnect Account" (`Settings.tsx:469-475`), three hardcoded fake tags with no `onClick` while a real tag system exists (`AppSidebar.tsx:371-388`), three separate controls navigating to the same timeline view, and a fabricated display email (`{(user || "guest") + "@wander.app"}`).
- **Cosmetics:** `index.html:7` still reads `<title>Tauri + React + Typescript</title>`, `package.json` still declares `"name": "tauri-app"`, and `vite.config.ts:5` has a **disarmed** suppression comment (`// ts-expect-error`, missing the `@`) so it does nothing. Two debug banners log on every launch (`main.tsx:7-8`).

---

## 7. Operational readiness

### 7.1 No CI of any kind (High)

There is **no `.github/` directory**. Nothing runs on push or pull request: not `cargo build`, not `cargo clippy`, not `cargo fmt --check`, not `cargo test`, not `tsc`, not a lint, not a bundle.

The consequences are visible throughout this report rather than hypothetical. A job that ran only the commands the README documents would have caught the false build claim (5.3) immediately. `cargo fmt --check` in CI would have kept 7 files formatted. `cargo clippy -D warnings` would very likely have flagged the held-lock-across-blocking-work in 4.2 and several of the 80 discarded results in 4.4. And a job that ran the 8 existing Rust tests would at least prove the crate compiles, which is something neither I nor, as far as the repository shows, any automated system has verified for this commit.

This is the highest-leverage single item in the report. It is roughly 40 lines of YAML and it converts most of the other findings from "will drift again" to "cannot drift again".

**Fix:** add `.github/workflows/ci.yml` running `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `npm ci && npm run build` on pull requests to `main`. Then add a migration test that runs the chain from version 0 to 19 (2.4d), which is the one test this codebase most needs and does not have.

### 7.2 The installer is unsigned and there is no updater (High)

`tauri.conf.json` has no `plugins.updater` block, no `pubkey`, and no code-signing configuration, while `bundle.targets` is `"all"`. So the artifact the README tells users to download is an **unsigned Windows executable** that triggers a SmartScreen warning, and there is **no mechanism to ship any of the fixes in this report to anyone who has already installed it**.

That interacts badly with 2.1 and 3.1. If the backup defect and the shipped MCP bridge are real in a distributed build, there is currently no push channel to remediate them; the only option is asking users to notice a new release and manually reinstall. For a security-sensitive application handling a Telegram account credential, an update channel is not a nice-to-have.

Also, `targets: "all"` emits an MSI alongside the NSIS installer, and the README documents only the `.exe`, so users may encounter an artifact the documentation does not mention.

**Fix:** add `tauri-plugin-updater` with a signing `pubkey`, set up Authenticode signing, pin `targets` to `["nsis"]`, and add a `release.yml` workflow so releases are reproducible rather than hand-built.

### 7.3 No linting or formatting configuration exists (High)

There are **zero** ESLint, Prettier, `rustfmt.toml` and `clippy.toml` files, and `package.json` has no `lint` or `test` script. Linting has never run in this project.

This is not a style complaint. It is the direct explanation for a specific cluster of findings in Section 6: `eslint-plugin-react-hooks` would have flagged the missing `useEffect` dependencies in 6.7 and `Search.tsx:82-94`, the exhaustive-deps rule plus a components-in-render rule would have flagged the remount bug in 6.4, `jsx-a11y` would have flagged most of 6.5 mechanically, and `no-unused-vars` at module scope would have surfaced the three dead files in 6.6. The frontend has strong *type* discipline (`strict`, zero `@ts-ignore`) and no *lint* discipline, and the defects that survived are precisely the ones types cannot catch.

**Fix:** add ESLint 9 flat config with `react-hooks`, `jsx-a11y` and `@typescript-eslint`, add a `lint` script, add `rustfmt.toml` and `clippy.toml`, and wire all of it into 7.1. Expect a large first-pass backlog.

### 7.4 Test coverage is thin and structurally misplaced (Medium)

There are **8** Rust unit tests across **7** `#[cfg(test)]` modules, and **0** frontend tests. The Rust tests that exist are reasonable, and the crypto ones are genuinely valuable: `security/mod.rs:577-594` covers recovery-key verification round-trip and asserts that a wrong passphrase fails.

But the coverage does not point at the risk. There is **no test that**: encrypts a file and then decrypts it byte-for-byte; detects a **truncated** `.wbenc` file (1.4, which is exactly why truncation is undetectable); rejects a tampered chunk; runs the 19-migration chain and asserts the resulting schema (2.4); or round-trips a database backup through the documented recovery procedure, which is the test that would have caught 2.1 the day it was written.

**Fix:** add `WBENC1` round-trip, tamper and boundary tests; add the migration chain test; and add one end-to-end test of the backup-and-restore procedure as documented in the README.

### 7.5 Committed debris, a dead 1.2 MB model, and two lockfiles (Medium)

Tracked in git and serving no purpose:

```
src-tauri/2                 140 bytes   (a file literally named "2")
src-tauri/build_log.txt     106 bytes
src-tauri/output.txt         84 bytes
```

More significantly, **both ONNX models are committed but only one is used**:

```
1244 KB  src-tauri/src/ai/version-RFB-320.onnx             <- never referenced
1088 KB  src-tauri/src/ai/version-RFB-320_simplified.onnx  <- include_bytes! at ai/mod.rs:12
```

So 1.24 MB of binary is in every clone forever, and it is the two largest tracked files in the repository. Committing the *used* model is defensible for a face detector that must work offline; committing the unused one is not.

**Two lockfiles are committed**: `package-lock.json` (204 KB) and `pnpm-lock.yaml` (132 KB). They will drift, and contributors will resolve dependencies differently depending on which package manager they reach for, which is a reproducibility problem for a project that ships signed-installer-shaped artifacts. Pick one, delete the other.

Also missing: **no `LICENSE`** and **no `SECURITY.md`**. For a security-sensitive application distributing binaries, the absence of a `SECURITY.md` means a researcher who finds something has no disclosure channel, and the absence of a license means nobody can legally fork or contribute.

**Fix:** `git rm` the three stray files and the unused model, delete one lockfile, add `.env` and `*.txt` to `.gitignore`, and add `LICENSE` and `SECURITY.md`.

### 7.6 Version and release metadata (Medium)

All three manifests declare version `0.0.0`, and `package.json` still declares `"name": "tauri-app"`. `Cargo.toml` has no `[profile.release]` section at all, so the release build gets no LTO, no `strip`, and default codegen units; for a Rust binary shipping ONNX Runtime, `lto = true` and `strip = true` are typically worth several MB and a measurable startup improvement.

A version of `0.0.0` also means the updater in 7.2, once added, has no meaningful version to compare against, and users cannot report "which build" they are on. Bump to `0.1.0` across all three manifests as part of the first fix release.

### 7.7 Dependency supply chain (Medium)

`npm audit` reports **7 vulnerabilities: 5 High, 1 Moderate, 1 Low**, in `vite`, `rollup`, `postcss`, `picomatch`, `nanoid`, `yaml` and `@babel/core`.

To be accurate about severity: **every one of these is build tooling, not shipped runtime code.** The Vite dev-server path-traversal and `server.fs.deny` bypass issues affect a developer running `npm run dev`, not an end user running the installer. They are still worth fixing, since a compromised dev machine is how supply-chain attacks on signed releases begin, but they are not user-facing and should not be reported as such.

On the Rust side I want to correct a concern that would be reasonable to raise and is not warranted here. The `grammers` Telegram client is a git dependency, which is often a supply-chain risk, but it is **correctly pinned to an immutable commit**:

```toml
# src-tauri/Cargo.toml:25-26
grammers-client = { git = "https://github.com/Lonami/grammers", rev = "b595a8c4fdfa5c3a8abcb5766c959ecfe30e9f6e", ... }
grammers-session = { git = "https://github.com/Lonami/grammers", rev = "b595a8c4fdfa5c3a8abcb5766c959ecfe30e9f6e", ... }
```

A `rev` pin, not a branch, and the same rev for both crates. That is the right way to do it. The crypto crates are all current and none are RUSTSEC-flagged (`aes-gcm 0.10.3`, `argon2 0.5.3`, `rand 0.8.5`, `blake3 1.8.3`). There are no path dependencies outside the repository, no wildcard versions, and no vendored code. `cargo audit` was not run, since that requires a full dependency resolution, and it belongs in CI.

### 7.8 No error tracking, and 876 kB in a single chunk (Medium / Low)

There is no Sentry or equivalent on either side. Observability is 50 `println!` calls in Rust that the Windows release subsystem discards, plus 77 `console.*` calls in the frontend that nobody can see in a packaged desktop app. `componentDidCatch` only calls `console.error` (6.7), so a crash in a shipped build is unreportable, which combined with 7.2 means you cannot learn about a problem *or* fix it for users.

`vite build` emits a single **876.57 kB** JavaScript chunk (255.52 kB gzipped) with no code splitting, and Vite says so explicitly. Four declared dependencies (`react-window`, `react-window-infinite-loader`, `react-virtualized-auto-sizer`, and their types) are **never imported**, yet `vite.config.ts:16-18` pre-bundles three of them in `optimizeDeps`, because `MediaGrid` hand-rolls its own virtualizer instead (Section 8). For a local desktop app the bundle size costs startup time rather than bandwidth, so this is Low, but deleting four unused deps and the stale `optimizeDeps` block is free.

---

## 8. What is already good

A review that lists only defects gives a false picture, and here it would give a badly false one. This project gets the hardest part right, and several findings above are fixed by copying a pattern that already exists a few files away. These are load-bearing strengths, not consolation.

**The cryptography is correct, and I checked specifically for the ways it usually is not.**

*Argon2id with strong, deliberate parameters.* 64 MiB memory cost, 3 iterations, 32-byte output, comfortably above the OWASP floor, with the algorithm and version pinned explicitly:

```rust
// src-tauri/src/security/mod.rs:189-193
fn argon2id_params() -> Result<Argon2<'static>> {
    let params = Params::new(65_536, 3, 1, Some(32))
        .map_err(|e| anyhow!("Failed to build Argon2 params: {}", e))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}
```

No PBKDF2, no raw SHA or blake3 used as a KDF, no fast hash on any passphrase path, and the same parameters used consistently for wrapping, verification and hashing.

*No nonce reuse anywhere.* This was the finding I expected to make and could not. Every nonce and salt comes fresh from `OsRng`, including on re-encryption paths and on the fixed-filename temp upload path. There is no constant nonce, no nonce derived from content or file ID, and no key-plus-nonce pair used twice on any code path I could find. Finding 1.6 is about entropy *width*, not reuse, and I flagged it as Medium precisely for that reason.

*Authenticated encryption everywhere, with no unauthenticated mode available.* AES-256-GCM is the only cipher in the codebase. No CBC, no CTR, no hand-rolled MAC. Tags are always verified, and failures map to opaque errors that leak no oracle detail (`security/mod.rs:431-433`).

*Chunk index bound into both the nonce and the AAD*, which defeats chunk reordering, duplication and mid-file deletion, the three attacks most often missed in hand-rolled chunked-AEAD formats:

```rust
// src-tauri/src/security/mod.rs:343-348
let nonce = derive_chunk_nonce(&base_nonce, chunk_idx);
let aad = chunk_idx.to_le_bytes();
let payload = Payload { msg: &chunk_buf[..n], aad: &aad };
```

Streaming through `BufReader`/`BufWriter` also means multi-GB videos are never fully loaded into memory, and counter overflow is explicitly checked on both paths.

*A 160-bit recovery key from a CSPRNG* (`security/mod.rs:278-287`), hex-encoded and grouped for transcription, making brute force infeasible regardless of the KDF. *`OsRng` exclusively* for all key material across all nine RNG call sites. *Per-wrap random 16-byte salts*, so the passphrase wrap and the recovery wrap of the same master key get different salts and nonces. *Strict length validation on every decoded field* before use. *Constant-time credential comparison* via `argon2::verify_password`.

**Encryption fails closed in the paths that check the key, and it does so thoughtfully.** Uploads are deferred rather than sent in the clear when the vault is locked (`upload_worker.rs:126-136`). And the watcher deliberately destroys a plaintext thumbnail rather than leaving it on disk:

```rust
// src-tauri/src/watcher.rs:248-252
} else {
    // Avoid leaving plaintext thumbnail when vault is locked.
    let _ = fs::remove_file(&thumb_path);
    thumbnail_path = None;
}
```

That is not the obvious thing to write. Someone thought about residue. Which is what makes 1.2 frustrating rather than damning: the instinct is present, it just was not applied to the temp directory.

**The vault starts locked and never auto-unlocks.** No passphrase caching, no key on disk, no "remember me" (`lib.rs:931-938`). **Encryption downgrade is blocked in the backend**, not just the UI (`lib.rs:303-309`), and `initialize_encryption` refuses to clobber an existing bundle (`lib.rs:324-328`), which prevents the catastrophic "overwrite the wrap and lose every key" scenario. **DPAPI is real, not aspirational**, and correctly called with user scope and proper `LocalFree` cleanup.

**The generic `set_config` command refuses to touch security keys.** This is the mitigation that keeps 3.2 from also being a write primitive:

```rust
// src-tauri/src/lib.rs:1686-1690
async fn set_config(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    if key.starts_with("security_") {
        return Err("Security settings are managed by dedicated security commands".to_string());
    }
```

All five security-relevant keys are correctly `security_`-prefixed, so the guard covers all of them.

**Parameter binding is the default, including the tricky case.** 87 of roughly 90 SQL statements bind everything, and the dynamic-arity `IN (...)` pattern is textbook-correct with `params_from_iter` (`database.rs:1262-1277`). No dynamic table or column names, no interpolated `ORDER BY` or `LIMIT`, and **no SQL outside `database.rs` at all**. Most query methods also clamp inputs defensively (`limit.max(0).min(1000)`), and `set_rating` clamps its domain.

**The std connection mutex never crosses an `await`.** All 89 methods acquire and release inside synchronous bodies, and several commands explicitly `drop(db_guard)` before awaiting with a comment saying why. This is the single most important thing to get right with `Mutex<Connection>` in an async app, and it is right everywhere.

**No arbitrary-file-delete command exists.** I looked for this specifically. Every `remove_file` operates on a path read from the database or generated by the app, never on IPC input, and no `fs:allow-write*` or `fs:allow-remove` capability is granted. **No shell injection either**: the three process spawns all pass arguments as arrays with no shell, and ffmpeg availability is probed first with graceful degradation.

**A real versioned migration system**, keyed on `PRAGMA user_version`, with each step in its own transaction. Migrations 13, 14 and 16 correctly implement the full SQLite table-rebuild dance (`foreign_keys = OFF`, create `_new`, `INSERT ... SELECT`, `DROP`, `RENAME`, re-enable) to repair foreign keys that cannot be altered in place, and migration 16 detects and repairs a legacy schema shape by probing `pragma_table_info`. That is careful work.

**Real transactions where they matter most**: `add_faces`, `add_tags`, `merge_persons` and `bulk_add_to_album` are all correctly atomic, and `add_tags` scopes its prepared statements in an inner block so they drop before commit.

**Graceful degradation and cooperative cancellation.** The face detector is `Option`al and the app runs without it. Workers take a `CancellationToken` and check it each iteration. The upload worker honours Telegram's own `FLOOD_WAIT` hint rather than blindly retrying. `resolve_app_data_dir` has a real fallback path. `clip.rs` tries multiple model candidates and gives an actionable error naming the Settings screen when all fail.

**The frontend's type discipline is genuinely strong for a project this young.** `strict`, `noUnusedLocals`, `noUnusedParameters` and `noFallthroughCasesInSwitch` are all on, and, unusually, **the codebase honours them**: zero `@ts-ignore`, zero `@ts-expect-error`, zero `@ts-nocheck`, and two `as any` in 10,915 lines, one of which is the canonical documented Leaflet workaround. `npm run build` is `tsc && vite build`, so this is enforced at build time rather than aspirational.

**`lib/api.ts` is a properly typed IPC boundary, and it is accurate.** Every wrapper declares a concrete return type and no `invoke` in it returns bare `any`. I mechanically diffed the frontend command names against the Rust `#[tauri::command]` attributes: **every single frontend invoke resolves to a real backend command**, with no typos and no drift, and the only unwired backend command is `debug_reset_faces`. The camelCase and snake_case split is also deliberate rather than accidental, with each side verified against its `serde` derive.

**The onboarding recovery-key flow gets the hard parts right.** It forces verification before proceeding, and, the detail most implementations miss, it actively purges the secret from state once verified:

```tsx
// src/components/Onboarding.tsx:185-194
const handleConfirmRecoveryStep = () => {
    if (!recoveryVerified) {
        toast.error("Verify the recovery key first.");
        return;
    }
    // Show once only in onboarding session.
    setRecoveryKey(null);
    setRecoverySegments([]);
    setStep("byok");
};
```

It also states the tradeoffs of each mode honestly to the user, requires an explicit risk acknowledgement for unencrypted mode, and includes a genuinely helpful inline tutorial for obtaining Telegram API credentials. And I verified three separate ways that **no secret is written to `localStorage`, `sessionStorage`, a log, or an error message anywhere in the application**. The 17 storage writes are all theme preferences and search history.

**`withBusy` and `toErrorMessage` are the right small abstractions** (`Onboarding.tsx:70-87`): the `finally` in `withBusy` makes a stuck spinner structurally impossible, and `toErrorMessage` types its input as `unknown` rather than `any`. They belong in `lib/utils.ts`, because every `catch (e: any)` elsewhere is a place they were needed and missing.

**`MediaGrid`'s hand-rolled virtualizer is competent**, with a `ResizeObserver` hook, a variable-height row model, a binary search for the row at a given scroll offset, and a configured overscan window. Finding 6.4 is about memoization around it, not about the virtualization itself.

**Honest inline documentation of known limits.** `lib.rs:2452-2453` notes that semantic search needs a real index for large datasets. `database.rs:809-810` flags a suspected transaction interaction. The README does the same at the product level. This is more useful than silence, and it is a good habit to keep.

---

## 9. Remediation plan

Ordered by risk reduction per unit of effort, not by section number. Two dependencies worth noting: **2.1 must ship before you tell anyone the backup works**, and **7.1 (CI) is what stops the rest of this list from silently regressing**, so it should land early despite not being a bug.

### Stage 0: stop the bleeding (hours, ship immediately)

| # | Change | Finding |
|---|---|---|
| 1 | Gate `tauri_plugin_mcp_bridge::init()` behind `#[cfg(debug_assertions)]`, or delete it. Read the plugin's source and confirm what it binds | 3.1 |
| 2 | Export the `SecurityBundle` unencrypted alongside the encrypted backup (or as a plaintext header), so the passphrase and recovery key actually work | 2.1 |
| 3 | Tell existing encrypted-mode users to keep a copy of `library.db`, and treat prior "encrypted backup" guidance as withdrawn | 2.1 |
| 4 | Derive `should_encrypt` from `SecurityBundle.mode`, fail closed on read error, and assert `WBENC1` on the artifact before upload | 1.1 |
| 5 | Filter `security_*` keys out of `get_all_config` | 3.2 |
| 6 | Drop `'unsafe-eval'` and `'unsafe-inline'` from `script-src`; narrow `assetProtocol.scope` and the `fs` scopes off `**` and `C:\**` | 3.3 |
| 7 | Guard `import_files` sources against arbitrary paths; confine `export_media` and `backup_database` destinations | 3.4 |
| 8 | Bound `ct_len` by the header's `chunk_size` | 3.6 |
| 9 | Fix the README download URLs and the in-app About link; bump all three versions to `0.1.0` | 5.2, 7.6 |

Items 1 and 2 are the two that change the risk profile of the product. Everything else in this stage is additive and low-risk.

### Stage 1: make regression impossible (days)

| # | Change | Finding |
|---|---|---|
| 10 | Add `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `npm ci && npm run build` | 7.1 |
| 11 | Add ESLint 9 flat config with `react-hooks` and `jsx-a11y`, plus `rustfmt.toml` and `clippy.toml`; add a `lint` script | 7.3 |
| 12 | Add `WBENC1` round-trip, truncation and tamper tests; add a 0-to-19 migration chain test; add a backup-and-restore test | 7.4 |
| 13 | Correct the two false README claims; document the `%TEMP%` residue and the `library.db` dependency | 5.3, 5.4 |
| 14 | Add code signing and `tauri-plugin-updater` with a `pubkey`; pin `bundle.targets` to `["nsis"]`; add `release.yml` | 7.2 |
| 15 | Fix the `version` assignments in migrations 5, 7 through 13; guard migration 15 against wiping all persons | 2.4 |
| 16 | Delete one lockfile; `git rm` the 3 stray files and the unused 1.2 MB model; add `LICENSE` and `SECURITY.md`; gitignore `.env` | 7.5 |

Item 15 belongs here rather than later because it is currently a latent hard-startup-failure waiting for the next migration anyone writes.

### Stage 2: correctness and durability (1 to 2 weeks)

| # | Change | Finding |
|---|---|---|
| 17 | Purge `%TEMP%\wanderer-*` on `lock_encryption`, on exit, and on startup; prefer serving decrypted bytes from memory | 1.2 |
| 18 | v2 file format: authenticate the header plus `key_id`, add a terminator chunk, require the magic when the bundle says encrypted | 1.4 |
| 19 | Protect `session.db` with DPAPI-plus-entropy or the master key | 1.3 |
| 20 | Add `zeroize`; make the master key a non-`Copy` `ZeroizeOnDrop` newtype | 1.5 |
| 21 | Move filesystem deletions out of the transaction in `empty_trash`; wrap `permanent_delete` | 2.2 |
| 22 | Set `journal_mode = WAL` and `busy_timeout`; use the online backup API instead of `fs::copy` | 2.3 |
| 23 | Convert `media_fts` to external-content FTS5 with triggers | 2.5 |
| 24 | Add the 7 missing indexes; switch to `prepare_cached` | 2.6 |
| 25 | Add `UNIQUE` on `upload_queue(file_path)`; make `toggle_favorite` a single `RETURNING`; add a stale-`uploading` reaper | 2.7 |
| 26 | Fix the poisoned-mutex handling with `into_inner()`; delete the per-face DEBUG `PRAGMA` block | 4.1 |
| 27 | Move ONNX inference and the 39 `std::fs` calls off the async runtime; drop the DB guard before per-item work | 4.2 |
| 28 | Wire up `AppError` and remove the string-matching retry in `App.tsx` | 4.5, 6.3 |
| 29 | Verify and retry Telegram plaintext deletion during migration; record un-purged IDs | 3.8 |
| 30 | Bind `camera_make`; clamp the 2 unclamped paginations; fix the negative `limit as usize` cast | 3.5, 3.7 |
| 31 | Commit a generated `schema.sql` | 2.4 |

### Stage 3: quality and maintainability (ongoing)

| # | Change | Finding |
|---|---|---|
| 32 | Extract one `<EnableEncryption>` component with the verification gate, used by both Onboarding and Settings | 6.1 |
| 33 | Close the print window, handle the clipboard rejection, defer the blob revoke | 6.2 |
| 34 | Hoist the inline `ItemWrapper`s; `memo()` the `Cell`; `useCallback` the handlers; key by `item.id` | 6.4 |
| 35 | Add `aria-label` to 26 icon buttons; make the grid keyboard-navigable; give `MediaViewer` a list and arrow keys | 6.5 |
| 36 | Delete `LoginView.tsx`, `Sidebar.tsx`, `ThemeSwitcher.tsx`; extract one `<TelegramLogin>` and one `useMediaPagination` | 6.6 |
| 37 | Apply the existing `map_media_row` at the other 17 sites (about 500 lines deleted, no behaviour change) | 4.6 |
| 38 | Move `ErrorBoundary` outermost in `main.tsx`; add a reload button; report crashes | 6.7 |
| 39 | Fix the two broken Tauri listener cleanups; remove the prop-mirroring state; clear the scroll timeout | 6.7 |
| 40 | Parse phashes once and bucket by prefix in `find_duplicates`; fix the 4 N+1 patterns | 4.3 |
| 41 | Decide the map tile question: vendor Leaflet assets and allow `https:` in `img-src`, or remove the map | 6.7 |
| 42 | Rotate the recovery key on use; add `change_passphrase`; hoist the 8-char check into a shared validator | 1.7, 1.8 |
| 43 | Widen the nonce to full width via a per-file `file_id` and derived subkey | 1.6 |
| 44 | Delete the 4 unused deps and the stale `optimizeDeps`; add code splitting | 7.8 |
| 45 | Add error reporting on both sides; replace `println!` with `log::`; audit for PII | 7.8, 4.7 |
| 46 | Fix `escape_like_pattern` (add `ESCAPE '\'`) or delete it with its dead caller; delete `Database::get_persons` | 4.7 |
| 47 | Add `[profile.release]` with `lto` and `strip`; target-gate `windows-sys` | 7.6, 4.7 |
| 48 | Split `database.rs` and `lib.rs` along domain lines; split `Settings.tsx` into per-tab components | 4.6, 6.6 |

---

## Appendix A: reproducing every measurement

Every number in this review came from a command run against commit `a9d7439` in a clean checkout. They are listed so you can re-run them, and so a future reader can tell whether a count has moved.

**Toolchain results (executed)**

```bash
npm ci
npx tsc --noEmit          # 0 errors, exit 0, strict: true
npx vite build            # succeeds; dist/assets/index-*.js = 876.57 kB (gzip 255.52 kB)
npm audit                 # 7 vulnerabilities: 5 high, 1 moderate, 1 low
cd src-tauri && cargo fmt --check   # drift in 7 files
# NOT run: cargo build, cargo test, cargo clippy, cargo audit (see limitations)
```

**Size and shape**

```bash
# rust: 21 files, 10,752 LOC; database.rs 3,264, lib.rs 2,545
find src-tauri/src -name '*.rs' | wc -l
find src-tauri/src -name '*.rs' | xargs wc -l | sort -rn

# frontend: 62 files, 10,915 LOC; Settings.tsx 1,302, MediaGrid.tsx 914
find src -name '*.ts*' | wc -l
find src -name '*.tsx' -o -name '*.ts' | xargs wc -l | sort -rn

# 74 Tauri commands (both counts agree, and `comm` of the two lists is empty both ways)
rg -c '^#\[tauri::command\]' src-tauri/src/lib.rs

# 89 public methods in database.rs, across 3 impl blocks
rg -c '^\s{4}pub fn ' src-tauri/src/database.rs
```

**Rust counts**

```bash
# 7 non-test unwrap(), 2 non-test expect(), 0 panic!/todo!/unimplemented!
rg -n '\.unwrap\(\)' src-tauri/src/
rg -n 'panic!|unreachable!\(|todo!\(|unimplemented!\(' src-tauri/src/

# 80 discarded results, 32 .ok(), 50 println!
rg -o 'let _ = ' src-tauri/src/ --no-filename | wc -l
rg -o '\.ok\(\)' src-tauri/src/ --no-filename | wc -l
rg -o 'println!' src-tauri/src/ --no-filename | wc -l

# 8 tests in 7 test modules; 0 frontend tests
grep -rn '#\[test\]' src-tauri/src | wc -l
grep -rln '#\[cfg(test)\]' src-tauri/src

# 19 migrations, 0 committed .sql files, 7 CREATE INDEX, 6 transactions
rg -n 'PRAGMA user_version =' src-tauri/src/database.rs | wc -l
find . -name '*.sql' -not -path './node_modules/*' | wc -l
rg -c 'CREATE INDEX' src-tauri/src/database.rs

# 41 conn.prepare(, 0 prepare_cached
rg -o 'conn\.prepare\(' src-tauri/src/database.rs | wc -l
rg -o 'prepare_cached' src-tauri/src/ | wc -l

# duplication: 17 inline row mappings, 15 duplicated column lists, helper used 3 times
rg -c 'file_hash: row.get\(2\)\?' src-tauri/src/database.rs
rg -c 'SELECT id, file_path, file_hash' src-tauri/src/database.rs

# 39 std::fs:: calls in lib.rs, nearly all inside async fn with no spawn_blocking
rg -c 'std::fs::' src-tauri/src/lib.rs

# no journal_mode / busy_timeout / synchronous pragma anywhere
rg -n 'journal_mode|busy_timeout|synchronous|WAL' src-tauri/src/   # no matches

# no SQL outside database.rs
rg -n 'execute\(|query_row|prepare\(' src-tauri/src/ --glob '!database.rs'   # no matches
```

**Frontend counts**

```bash
# 2 as any, 0 @ts-ignore, 0 @ts-expect-error, 0 @ts-nocheck
grep -rn "as any" src --include=*.ts --include=*.tsx | wc -l
grep -rn "@ts-ignore\|@ts-expect-error\|@ts-nocheck" src | wc -l

# 77 console.* (6 log, 66 error, 3 warn)
grep -rn "console\." src --include=*.ts --include=*.tsx | wc -l

# 0 aria-label in app code (2 total, both generated), vs 26 size="icon"
grep -rn "aria-label" src --include=*.tsx | grep -v "^src/components/ui/" | wc -l
grep -rn 'size="icon"' src --include=*.tsx | grep -v "^src/components/ui/" | wc -l

# 2 total occurrences of onKeyDown / tabIndex / role= in app code
grep -rn "onKeyDown\|tabIndex\|role=" src --include=*.tsx | grep -v "^src/components/ui/" | wc -l

# 0 React.memo, 0 imports of the 4 declared virtualization deps
grep -rn "memo(" src --include=*.tsx | grep -v "^src/components/ui/" | wc -l
grep -rn "react-window\|react-virtualized" src --include=*.ts --include=*.tsx | wc -l

# LoginView / Sidebar / ThemeSwitcher are never imported (1 hit each: their own definition)
grep -rn "LoginView\|from \"./Sidebar\"\|ThemeSwitcher" src | grep -v "^src/components/\(LoginView\|Sidebar\|ThemeSwitcher\).tsx"

# every frontend invoke resolves to a real backend command
grep -oP 'invoke(<[^>]*>)?\("\K[^"]+' src/lib/api.ts | sort -u > /tmp/fe.txt
grep -rA3 "#\[tauri::command\]" src-tauri/src --include=*.rs | grep -oP "fn \K\w+" | sort -u > /tmp/be.txt
comm -23 /tmp/fe.txt /tmp/be.txt    # empty: no frontend call without a backend command
```

**Configuration and hygiene**

```bash
# no CI, no lint/format config
ls -a .github                       # No such file or directory
ls -a | grep -iE "eslint|prettier"  # nothing
ls src-tauri | grep -iE "rustfmt|clippy"  # nothing

# CSP allows unsafe-inline and unsafe-eval; asset scope is **
python3 -c "import json;d=json.load(open('src-tauri/tauri.conf.json'));print(d['app']['security'])"

# grammers is pinned to an immutable rev, not a branch
grep -n 'git =' src-tauri/Cargo.toml

# no [profile.release]; version 0.0.0 in all three manifests
grep -n 'profile' src-tauri/Cargo.toml            # no matches
grep -n '"version"' package.json src-tauri/tauri.conf.json

# largest tracked files: the unused model is 1,244 KB
git ls-files -z | xargs -0 du -k | sort -rn | head -8
grep -rn "version-RFB-320" src-tauri/src --include=*.rs   # only _simplified is used

# both lockfiles tracked; no LICENSE, no SECURITY.md
ls -la package-lock.json pnpm-lock.yaml
ls LICENSE* SECURITY*                # No such file or directory
```

**Note on `grep -c` versus occurrence counts.** Several counts above are occurrence counts (`rg -o ... | wc -l`), not matching-line counts, so a line with two `let _ =` contributes 2. This is deliberate where the unit of work is the occurrence. Where the unit is a file, `-l` is used instead. The frontend `console.*` and `aria-label` figures are line counts.

---

## Appendix B: finding index by severity

**Critical (4).** Permanent data loss, or a silent defeat of the product's central promise.

| ID | Finding |
|---|---|
| 2.1 | The encrypted database backup and the entire Telegram archive are undecryptable if `library.db` is lost, because the wrapped master key lives only inside `library.db` |
| 1.1 | Encryption enforcement reads a duplicated `security_mode` row and fails open to plaintext upload, while the UI reports "encrypted" from a different source |
| 3.1 | `tauri-plugin-mcp-bridge` is registered unconditionally in release builds, in a process holding the decrypted master key |
| 3.2 | `get_all_config` hands the wrapped master key and the DPAPI credential blob to the webview, under a CSP permitting `unsafe-inline` and `unsafe-eval` |

**High (23).** Data loss or security degradation under realistic conditions, or unrecoverable/unmaintainable state.

| ID | Finding |
|---|---|
| 1.2 | Decrypted plaintext accumulates in `%TEMP%` forever and survives `lock_encryption` |
| 1.3 | `session.db`, a full Telegram account credential, is stored with no protection |
| 1.4 | The file format authenticates chunks but not the file: truncation and substitution are undetectable, and plaintext is silently accepted |
| 1.5 | No zeroization; the master key is `Copy` and is moved into a spawned task that outlives lock |
| 2.2 | Filesystem deletions happen inside a transaction that can roll back |
| 2.3 | No WAL or busy timeout; the backup is a raw `fs::copy` of a live database |
| 2.4 | Migration `version` variable not updated in 8 steps (latent hard failure); migration 15 can delete every named person; no committed schema |
| 2.5 | The full-text index is insert-only, never deleted from, and never populated by the sync path |
| 3.3 | CSP allows inline script and eval; asset and `fs` scopes cover the whole filesystem |
| 3.4 | `import_files` is an arbitrary file read that auto-uploads the file to Telegram |
| 3.5 | `camera_make` is string-interpolated into the WHERE clause (LIKE-pattern injection today, structural injection risk) |
| 4.1 | A single panic permanently poisons the DB mutex; a DEBUG block with `unwrap()` runs per face while holding the lock |
| 4.2 | The global lock is held across ONNX inference and 39 blocking `std::fs` calls on the async runtime |
| 4.3 | Duplicate detection is O(n squared) with 2 allocations per comparison, under the global lock |
| 5.2 | README download links point at a different GitHub owner, for an unsigned installer |
| 6.1 | The Settings path to enable encryption has no verification and no save affordance, silently destroying recoverability |
| 6.2 | Recovery-key print window never closes and fails silently; clipboard rejection unhandled |
| 6.3 | Startup retries forever by string-matching a Rust error message, with no cap and no cleanup |
| 6.4 | Inline `ItemWrapper` components remount every visible grid cell on every parent render; index-based keys reuse wrong nodes |
| 6.5 | Zero `aria-label` in app code against 26 icon buttons; the photo grid is unreachable by keyboard |
| 7.1 | No CI: nothing builds, formats, lints, tests or type-checks on push |
| 7.2 | The installer is unsigned and there is no updater, so fixes cannot reach existing users |
| 7.3 | No ESLint, Prettier, rustfmt or clippy configuration exists |

**Medium (22).**

| ID | Finding |
|---|---|
| 1.6 | Nonce carries 64 bits of entropy rather than 96; master key never rotated |
| 1.7 | A used recovery key is never invalidated; no change-passphrase command exists |
| 1.8 | 8-character passphrase floor, trim inconsistency, no policy, no unlock throttling, and no check at all on the reset path |
| 1.9 | DPAPI called without secondary entropy |
| 1.10 | Metadata, including GPS coordinates, is never encrypted |
| 2.6 | Missing indexes on 7 hot columns; zero `prepare_cached` |
| 2.7 | Non-atomic read-modify-write; no `UNIQUE` on `upload_queue`; stranded `uploading` rows |
| 3.6 | Unbounded allocation from an attacker-controlled chunk length |
| 3.7 | Two unclamped paginations; negative `limit as usize` becomes `usize::MAX` |
| 3.8 | Migration leaves plaintext copies in Telegram and reports success anyway |
| 4.4 | 80 discarded results, several hiding real state corruption and false success reports |
| 4.5 | `errors.rs` is dead code; all 74 commands return `Result<T, String>` |
| 4.6 | `database.rs` is 3,264 lines with 17 copy-pasted row mappings and an unused helper |
| 5.3 | README documents `npm run build` as the production build; it builds only the frontend |
| 5.4 | Four README claims are true but materially incomplete |
| 6.6 | Three login implementations, three dead files, a hand-rolled `window` event bus, pagination copy-pasted 7 times in 2 incompatible variants |
| 6.7 | Error boundary not outermost and unrecoverable; 2 broken listener cleanups; prop-mirroring state; uncleared timeout; swallowed errors |
| 7.4 | 8 Rust tests, 0 frontend tests, and none covering truncation, tampering, migrations or backup restore |
| 7.5 | Committed debris, an unused 1.2 MB model, two lockfiles, no `LICENSE`, no `SECURITY.md` |
| 7.6 | Version `0.0.0` everywhere, `name: "tauri-app"`, no `[profile.release]` |
| 7.7 | 7 npm audit findings (build tooling only) |
| 7.8 | No error tracking on either side; 876 kB single chunk; 4 unused deps still pre-bundled |

**Low (10).**

| ID | Finding |
|---|---|
| 4.7 | `try_lock().unwrap()` on the upload hot path |
| 4.7 | `escape_like_pattern` does not work (no `ESCAPE` clause), and its only caller is dead code |
| 4.7 | FTS5 query construction can emit a syntax error on ordinary input |
| 4.7 | `.unwrap()` on a path conversion inside a spawned sync worker |
| 4.7 | `unchecked_transaction` used once inconsistently |
| 4.7 | 50 `println!` bypass the initialized logger and are discarded in release |
| 4.7 | A timestamp-seeded non-cryptographic RNG sits next to the crypto module (used only for device id) |
| 4.7 | Dead `.env` machinery, and `.env` is not gitignored |
| 4.7 | `windows-sys` not target-gated while `bundle.targets` is `"all"` |
| 6.7 | Dead and duplicated UI, three confirmation idioms, stale `index.html` title, a disarmed `ts-expect-error` |
