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
