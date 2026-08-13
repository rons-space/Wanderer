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
