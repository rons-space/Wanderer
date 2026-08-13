# Code Review (Merged): Wander(er) (`rons-space/Wanderer`)

**Date:** 2026-08-13
**Reviewer:** [code]smith, the cloud coding agent from [Blacksmith](https://blacksmith.sh)
**Scope:** Full repository. Tauri 2 desktop media manager. 21 Rust files / 10,752 LOC in `src-tauri/src`, 62 TypeScript files / 10,915 LOC in `src`, 74 IPC commands, 19 schema migrations.

---

## What this document is

Three independent code reviews of this repository were produced on the same day, at different depths and with different framings. They overlap heavily, disagree in a handful of places, and each contains findings the other two missed. This document merges them into one authoritative report.

| # | Source document | Size | Role in this merge |
| --- | --- | --- | --- |
| **M** | `CODE_REVIEW_NORMAL_UNSPECIFIED.md` | 1,482 lines, 59 numbered findings | **Master.** Structure, severity model, finding IDs and citations are inherited from this document. |
| **S** | `CODE_REVIEW_FINDINGS_HIGH_SPECIFIED.md` | 131 lines, ~30 findings | Compared against master. Contributes 7 findings master does not have. |
| **U** | `CODE_REVIEW_FINDINGS_HIGH_UNSPECIFIED.md` | 160 lines, ~45 findings | Compared against master. Contributes 14 findings master does not have. |

### Merge method

1. **M is the spine.** Every M finding keeps its original ID (`1.1`, `3.4`, `6.7`, and so on) so existing references stay valid.
2. **Corroboration is recorded, not duplicated.** Where S or U found the same defect, it is tagged inline as `[also S-x]` / `[also U-x]` rather than restated. A finding confirmed independently by two or three passes is more trustworthy than a solo finding, and the tags let you see which is which at a glance.
3. **Findings absent from M are merged in as new IDs**, slotted into the section they belong to and marked `[new, from S]` or `[new, from U]`. There are 20 of these, and two of them are High.
4. **Disagreements are not silently resolved.** Section 10 lists all nine places where the three reviews contradict each other, with the reconciliation and the evidence behind it.
5. **Contested claims were re-verified against the current `main`** (`378707a`) while merging. Where re-verification changed the answer, the merged document uses the new answer and Section 10 records the correction.

### Coverage overlap

| | Findings | Also in M | Absent from M |
| --- | --- | --- | --- |
| S | ~30 | 23 | **7** |
| U | ~45 | 31 | **14** |

The two shorter reviews are close to strict subsets of the master on security configuration, frontend hygiene and repo hygiene. They diverge usefully on ground the master explicitly did not cover: S ran `cargo audit` (master did not), and U read `cache.rs` and the Telegram/upload UI paths in more detail. Conversely, **all four Critical findings originate in the master alone**, and three of them (2.1, 1.1, 3.2) were missed by both shorter passes. Depth mattered here.

---

## Executive summary

**The cryptography is sound. The system built on top of it can lose all of your data, and in one case is guaranteed to.**

Wander(er) is a local-first photo manager that encrypts media before backing it up to a user's Telegram account. The feature set is broad and coherent: import and watch folders, timeline and album browsing, cloud-only storage with on-demand restore, map view, duplicate detection, face grouping, semantic search, ratings, tags, trash and restore. The crypto core is AES-256-GCM with Argon2id at 64 MiB, per-file random nonces from `OsRng`, a 160-bit recovery key, and a dual-wrapped master key. All three reviews independently reached the same verdict on the primitives: they are correct, and the failures are in the plumbing around them.

Five defects make it unsafe to rely on as a backup tool. Four are Critical, and the fifth is the highest-impact finding contributed by the merge.

1. **The encrypted backup cannot be decrypted (2.1).** `backup_database` encrypts a copy of `library.db` with the master key, but the wrapped master key exists **only inside `library.db`**. The key material required to open the artifact is sealed inside the artifact. Lose the local disk and neither the passphrase nor the recovery key recovers the backup, or any media in the Telegram archive.
2. **Encryption enforcement fails open to plaintext upload (1.1).** Workers decide whether to encrypt by reading a duplicated `security_mode` config row with `.ok().flatten().unwrap_or("unset")`, so a read error or missing row silently yields `should_encrypt == false`. The UI derives "encrypted" from a different row, so the app reports the library as encrypted while shipping plaintext to Telegram.
3. **A remote-control plugin ships in release builds (3.1).** `tauri-plugin-mcp-bridge` is registered unconditionally at `lib.rs:863`, in a process that holds the decrypted master key in memory. Confirmed still present at HEAD.
4. **The wrapped master key is handed to the webview (3.2)**, by a `get_all_config` command with no key filter, under a CSP permitting `'unsafe-inline'` and `'unsafe-eval'` and an asset-protocol scope of `**`.
5. **The thumbnail cache is a file-deletion machine (2.8)** `[new, from U]`. `cache.rs:14-24` registers a moka eviction listener that `fs::remove_file`s the evicted thumbnail, at capacity 2000, while nothing ever updates `media.thumbnail_path`. Any library over 2,000 photos silently loses thumbnails that the database still references. The master review never read `cache.rs`.

Beyond those, "encrypted at rest" has a large asterisk: viewing anything in encrypted mode writes a fully decrypted copy into the OS temp directory, and nothing ever deletes it, including `lock_encryption` (1.2).

There is no CI, no `.github/` directory at all, so nothing type-checks, formats, lints, tests or builds on push. The installer the README links to is unsigned and there is no updater, so there is no mechanism to ship the fixes above to users who already installed it.

If this has been distributed to real users who enabled encryption, the backup defect (2.1) should be treated as an active incident: those users believe they have an off-site backup and do not.

### Measured state of the codebase

Measurements are the master's, re-verified against `378707a` where the merge touched them.

| Metric | Value |
| --- | --- |
| `npx tsc --noEmit` | passes (0 errors, `strict: true`) |
| `npx vite build` | succeeds, **876.57 kB** JS in a single chunk (255.52 kB gzip) |
| `cargo fmt --check` | **7 files** with formatting drift |
| `cargo build` / `cargo test` | **not run** (see limitations) |
| `npm audit` | **7 vulnerabilities: 5 High, 1 Moderate, 1 Low** (all build tooling) |
| `cargo audit` (from S) | **15 vulnerabilities + 8 unsound/yanked warnings** |
| CI workflows (`.github/`) | **0** (re-verified at HEAD) |
| ESLint / Prettier / rustfmt.toml / clippy.toml configs | **0 / 0 / 0 / 0** |
| Rust unit tests / test modules | 8 / 7 |
| Frontend tests | **18** in `src/lib/__tests__/format.test.ts` (see Section 10, item 6) |
| Tauri IPC commands | **74** (re-verified; U's count of 75 is off by one) |
| `unwrap()` in Rust (non-test) | 7 |
| `let _ =` discarded results | **80** |
| `println!` in Rust / `console.*` in frontend | 50 / 77 |
| `as any` / `@ts-ignore` / `@ts-expect-error` | **2 / 0 / 0** |
| `aria-label` in application code | **0** (2 total, both in generated shadcn) |
| `React.memo` usages | **0** |
| SQL statements built with string interpolation of free text | **1** |
| Schema migrations / committed `.sql` schema files | 19 / **0** |
| Committed but unused ONNX model | **1,244 KB** |
| Lockfiles committed | **2** |
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

### Threat model note (from S)

This is a **local-first, single-user** desktop app. There is no server and no multi-user boundary, and every Tauri command is invoked by the app's own trusted frontend. That lowers the severity of "path or SQL comes from the frontend" issues, because the frontend is not normally the attacker. It does **not** eliminate the risk: the WebView runs under a permissive CSP, so any XSS or malicious npm dependency escalates directly into a very broad native command and filesystem surface, and the backend still parses genuinely untrusted **data** (image/EXIF/RAW bytes, downloaded AI models, Telegram content, sync manifests). All three reviews prioritized with this in mind and arrived at the same top-of-list: capability over-provisioning.

---

## 1. Encryption and key management

The primitives are correct. All three reviews hunted specifically for the four defects that kill hand-rolled file encryption (weak KDF, nonce reuse, unauthenticated ciphertext, non-CSPRNG randomness) and none of the three found any of them. Details and citations are in [Section 9](#9-what-is-already-good). Everything below is a failure in the layer that *decides when and whether* to use that crypto.

### 1.1 Encryption enforcement reads a duplicated flag and fails open to plaintext upload (Critical)

*Master only. Missed by both S and U.*

The authoritative crypto state is the `SecurityBundle` in config key `security_bundle_v1`. But every worker that decides "encrypt or not" reads a **second, separate** config row, `security_mode`:

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

**This is reachable without an attacker.** The two rows are written by two independent, non-transactional `INSERT OR REPLACE` statements (`lib.rs:105-115`), so a crash, a power loss, or a transient DB error between them leaves `security_bundle_v1 = encrypted` and `security_mode` absent.

**The failure is invisible.** `get_security_status` derives the user-facing state from the *bundle* (`lib.rs:257-266`), so the UI reports "encrypted, unlocked" while the upload worker streams plaintext originals into cloud storage. Nothing verifies after the fact that the uploaded blob starts with the `WBENC1` magic. Because `library.db` is an ordinary unencrypted SQLite file, any local process running as the user can also set `security_mode = 'unencrypted'` and silently disable encryption of all future uploads.

**Fix:** delete `security_mode` as a decision input. Derive `should_encrypt` from `load_security_bundle()?.mode`, treat a read failure as **fail-closed** (defer the upload rather than send plaintext), and assert `FILE_MAGIC` on the artifact immediately before handing it to `upload_file_with_progress`.

### 1.2 Decrypted plaintext accumulates in the OS temp directory and is never deleted (High)

*[also U-2.4]*

In encrypted mode, viewing anything writes a fully decrypted copy to the system temp directory. Two sinks, both permanent: thumbnails to `std::env::temp_dir().join("wanderer-thumb-cache")` (`lib.rs:166-190`) and full-size originals to `wanderer-view-cache-materialized` (`lib.rs:2178-2187`).

The only cleanup routine, `view_cache::cleanup_cache`, is pointed exclusively at the *encrypted* blob directory under the app data dir, and runs **once**, ten seconds after startup (`lib.rs:1077-1088`). Nothing ever deletes `wanderer-thumb-cache/`, `wanderer-view-cache-materialized/`, `wanderer-encrypted-uploads/`, `wanderer-download-staging/`, `wanderer-view-cache-staging/` or `wanderer-local-restore-staging/`. Locking the vault does not touch them:

```rust
// src-tauri/src/lib.rs:360-364
async fn lock_encryption(state: State<'_, AppState>) -> Result<(), String> {
    state.security_runtime.lock().await.master_key = None;
    Ok(())
}
```

So the entire *viewed* portion of an "encrypted" library accumulates as plaintext in `%TEMP%` indefinitely, readable with no passphrase, after the user has locked the vault and closed the app. On Windows `%TEMP%` is per-user, which limits this to same-user access and is why this is High rather than Critical. On Linux and macOS, where the README says support is planned, `/tmp` is shared. U adds that the view-cache cleanup running only once also means the encrypted staging dirs grow unbounded during a session.

**Fix:** track materialized paths, delete them in `lock_encryption`, on window close and on startup, and prefer serving decrypted bytes from memory through a custom protocol handler over writing plaintext files at all.

### 1.3 The Telegram session file is a full account credential stored with no protection (High)

*[also S-H4, U-2.1]. The only High corroborated by all three reviews.*

```rust
// src-tauri/src/telegram.rs:105-112
let session_path = app_data_dir.join("session.db");
let session = SqliteSession::open(session_path)?;
```

No DPAPI wrapping, no master-key encryption, no ACL hardening. This is a strict inversion of value: the **low**-sensitivity API ID and hash get DPAPI (`lib.rs:429`), while the **high**-sensitivity MTProto authorization key does not. S makes the chain explicit: combined with the `C:\**` read scope (3.3), the session file is readable straight from the webview, and exfiltrating it hands over the entire Telegram account, not just the backups.

*Inferred, not verified:* the `grammers-session` source was not read (it is a git dependency), but `sqlite-storage` conventionally persists the DC address, user id and the MTProto `auth_key` in plaintext columns.

Credit where due: logout deletes the file carefully, with retries (`telegram.rs:488-514`).

**Fix:** DPAPI-wrap the session or encrypt it with the master key, matching the credential handling. S notes the correct long-term answer is the OS keychain rather than DPAPI, since DPAPI is Windows-only (see 1.9 and 4.7).

### 1.4 The file format authenticates chunks but not the file (High)

*[also S-L3, U-2.2]. Note the severity spread: S rated this Low, U and M rated it High. Reconciled in Section 10, item 8.*

The header is `magic | version | chunk_size | base_nonce`, and the only AAD is the chunk index (`security/mod.rs:328-348`). Decryption loops until EOF and treats EOF as normal termination (`security/mod.rs:410-415`). Three consequences:

- **Truncation is undetectable.** Deleting trailing chunks yields a shorter file that decrypts with zero errors, because there is no total-length field, no final-chunk flag, and no length in the AAD.
- **Substitution is undetectable.** Nothing binds a blob to a media item, so with a single master key across all files, an attacker with write access to the Telegram account can swap photo A's blob for photo B's and both decrypt cleanly. A `key_id` **is** generated and persisted (`security/mod.rs:112-114`) but is never written into a header nor checked.
- **The header is unauthenticated.** `version` and `chunk_size` are read and trusted (`security/mod.rs:380-394`) without being covered by any AAD.

Compounding it, whether a blob is encrypted at all is decided by sniffing six magic bytes, with no reference to the DB's `is_encrypted` column, and plaintext is silently accepted and re-encrypted as though authentic (`lib.rs:2150-2168`). U observes the mirror-image case: flipping the magic bytes makes `is_encrypted_file` return false, so `decrypt_file_if_needed` (`mod.rs:449-465`) copies raw ciphertext into the library as if it were plaintext media instead of failing.

So in encrypted mode with the vault unlocked, an attacker who substitutes a blob can inject arbitrary content that the app treats as authentic media. The AEAD provides confidentiality but, at the application level, effectively no authenticity for cloud-sourced data.

**Fix:** a v2 format that puts `magic || version || chunk_size || base_nonce || key_id || total_chunks` into the AAD, plus an explicit terminator chunk. When the bundle says encrypted and the row says `is_encrypted`, require the magic and hard-fail otherwise, then compare the decrypted content against the stored blake3 hash.

### 1.5 No zeroization; the master key is a `Copy` type fanned out across the process (High)

*[also U-2.6]*

`zeroize` is not a direct dependency, and no `Zeroize`, `Zeroizing` or `ZeroizeOnDrop` appears anywhere in `src-tauri/src`. The master key is a plain `Copy` array in long-lived state (`security/mod.rs:81-86`), so every read leaves an uncleaned copy behind: `lib.rs:204-206`, `upload_worker.rs:125`, `watcher.rs:231`, `sync_worker.rs:68` and `:303`. Worse, `lib.rs:496-531` copies the key and **moves it into a `tokio::spawn`'d migration task**, so a running migration holds a live plaintext key copy that `lock_encryption()` cannot reach. Also unzeroized: the derived KDF output, the unwrapped plaintext `Vec<u8>` from GCM, the passphrase arriving over IPC (`lib.rs:318`, `344`, `369`, `387`), and the recovery key string returned to the frontend.

The `#[derive(Debug)]` on `RuntimeState` is also one careless `{:?}` away from printing the raw master key to a log. No such statement exists today.

**Fix:** add `zeroize`, wrap the key in a non-`Copy` `ZeroizeOnDrop` newtype so every copy becomes explicit and reviewable, take passphrases as `Zeroizing<String>`, and zeroize on lock.

### 1.6 Nonce carries 64 bits of entropy, not 96 (Medium)

*[also U-2.3]*

**This is not nonce reuse.** The base nonce is fresh from `OsRng` per file, and within a file each chunk gets a distinct index with checked overflow. But the counter **overwrites** the last four random bytes (`security/mod.rs:289-293`), so the per-file random prefix is only 64 bits.

U frames the bound more aggressively than M: by NIST SP 800-38D's 2^-32 collision bound, 64 bits supports roughly 90,000 encryptions per key before a collision becomes likely, and a media library (each photo, each thumbnail, each backup) can plausibly exceed that. M computes ~3 x 10^-8 at 2^20 files and calls the margin acceptable but needless. Both agree on the fix and on Medium severity; U's number is the one to plan against, since thumbnails and backups are separate encryptions. Compounding either way: **the master key is never rotated** (neither `recover_and_rewrap` nor `regenerate_recovery_key` re-keys).

**Fix:** store an 8-byte random `file_id` in the header, derive a per-file subkey from it, and use a full-width counter nonce. Composes with the `key_id` work in 1.4.

### 1.7 A used recovery key is never invalidated, and there is no change-passphrase path (Medium)

*Master only.*

Recovery re-wraps only the passphrase; the recovery wrap and its verifier are carried over untouched (`security/mod.rs:160-166`). A recovery key that was typed into a possibly-compromised machine, or left in `wanderer-recovery-key.txt` in Downloads (`Onboarding.tsx:89-99`), remains a permanently valid master credential. And there is **no change-passphrase command** among the 74: the only way to change a passphrase is `recover_encryption`, which *requires* the recovery key. So a user who suspects their passphrase was shoulder-surfed must expose their recovery key in order to rotate it.

**Fix:** rotate the recovery wrap and verifier inside `recover_and_rewrap` and return a fresh recovery key; add `change_passphrase(old, new)`.

### 1.8 Passphrase policy and unlock throttling (Medium)

*Master only.*

The only check is length, and it is inconsistent with what gets wrapped: `security/mod.rs:100-102` uses `passphrase.trim().len() < 8` while the wrap uses the untrimmed `passphrase.as_bytes()` (`security/mod.rs:107`), so `"1234567 "` is accepted and the trailing space becomes part of the secret. There is no strength estimation, no breach list, no character-class requirement, and `unlock_encryption` (`lib.rs:343-358`) has no attempt counter, backoff or lockout. Argon2id at 64 MiB is itself a strong rate limiter at roughly 0.1 to 0.3 seconds per guess, and the realistic threat is offline cracking of the on-disk wrap (3.3) rather than online guessing, which is why this is Medium.

`recover_and_rewrap` accepts `new_passphrase` and wraps it with **no length check at all** (`security/mod.rs:143-166`), so the 8-character floor is enforced on the initialize path but not on the reset path. Hoist the check into one shared validator.

### 1.9 DPAPI is used correctly but without secondary entropy (Medium)

*Master only; S-H4 and U-2.6 raise the related platform gap, covered in 4.7.*

The README's DPAPI claim is **true**, and the call is well formed: `CryptProtectData` with `CRYPTPROTECT_UI_FORBIDDEN`, user scope rather than `CRYPTPROTECT_LOCAL_MACHINE`, and `LocalFree` on both the output blob and the description string (`security/mod.rs:481-501`, `536-543`). The gap is that `pOptionalEntropy` is `null`, so **any** process running as the same user can call `CryptUnprotectData` on the blob lifted out of `library.db`. DPAPI here protects against offline theft of the file, not against same-user malware. Impact is bounded (an api_id and api_hash are not account credentials), but 1.3 is the credential that actually matters.

### 1.10 Metadata is never encrypted (Medium)

*Master only.*

`encrypt_file` covers media blobs, thumbnails and the backup artifact, but the live `library.db` is opened as a plain `rusqlite` database with no SQLCipher (`Cargo.toml:28` has no cipher feature). So filenames, full local paths, blake3 and perceptual hashes, extracted EXIF **including GPS coordinates**, face and person data, and album structure are all plaintext at rest, sitting alongside the wrapped-key material and the DPAPI blob. The README is explicit that `backup/` is plaintext but does not mention that the metadata index is too, and for a photo library the GPS trail is arguably more sensitive than any single image.

### 1.11 AI models are fetched from unverified mirrors with no integrity check (Medium)

*[new, from S-M3 and U-2.5]. The master explicitly listed the ONNX models as out of scope, so this is a genuine gap in the master, corroborated independently by both shorter reviews.*

Three model download paths, none of which verify what they receive before handing it to a parser:

- **ArcFace** is downloaded from a personal GitHub release and unofficial HuggingFace accounts, validated only by file size (`ai/mod.rs:199-325`, specifically `212-221`).
- **MobileNet** object detection, same pattern (`object_detection.rs:311-314`).
- **CLIP** has no size check and no hash check at all (`clip.rs:444-508`, specifically `458-465`).

TLS is enforced via rustls (a genuine positive, noted by S), so this is not a passive-network attack. But a compromised or hijacked upstream mirror can silently swap roughly 350 MB of model content. `tract-onnx` is memory-safe so RCE is unlikely, but model substitution (poisoned face matching, poisoned semantic search) and parser DoS are both live. S adds the supply-chain angle from the other direction: `tract-nnef` has a known integer-overflow-to-OOB-read on model load (see 8.4), so the parser is not unconditionally safe either.

The committed `version-RFB-320*.onnx` files, embedded via `include_bytes!`, also lack documented provenance (see 8.5).

**Fix:** pin SHA-256 hashes for every downloaded model, pin immutable revisions rather than moving tags, verify before the first parse, and document the provenance of the committed models.

---

## 2. Recoverability and data integrity

### 2.1 The encrypted backup is mathematically undecryptable (Critical)

*Master only. The single most serious finding in any of the three reviews, and both shorter passes missed it.*

This is a design defect rather than a bug: the code does exactly what it says, and what it says is circular.

The master key is **random**, not derived from the passphrase (`security/mod.rs:99-109`). The only copy of that wrapped key, with its Argon2 salts, is the `SecurityBundle`, persisted into the `config` table **inside `library.db`** (`lib.rs:105-108`). Now the backup:

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

Walk the disaster scenario the feature exists for. The user's drive fails. They reinstall on a new machine. They have their passphrase and their printed recovery key, exactly as the README told them to. They have `library_backup_1234.db.wbenc`, and they have every photo in Telegram. And there is no sequence of actions that recovers any of it, because unwrapping the master key requires the salts and wrapped ciphertext that are inside the encrypted blob they are trying to open.

The same reasoning applies to the entire cloud archive: every uploaded media file is encrypted with that same master key (`upload_worker.rs:147-155`). **Lose `library.db` and the Telegram backup is cryptographically destroyed.** `backup_database` will also happily upload the undecryptable artifact to Telegram when `upload_to_telegram` is set (`lib.rs:1927-1930`), so the off-site copy is off-site and useless.

**Fix, and it is small:** the `SecurityBundle` is *already* protected by Argon2id and the passphrase; that is its entire purpose. Export it **unencrypted** alongside the encrypted backup, or write it as a plaintext header on the `.wbenc` artifact. Then the passphrase and recovery key work as documented. Ship this before anything else in this report, and tell existing users to keep a copy of their current `library.db`.

### 2.2 Filesystem deletions happen inside a transaction that can roll back (High)

*[also U-3.3]*

`empty_trash` unlinks irrecoverable user files *before* the transaction commits (`database.rs:2281-2295`). If any later `tx.execute` fails on item N, the `?` propagates, the transaction **rolls back**, and the database still lists all N items as present while their bytes are gone. The user is left with a populated trash in which every entry is a dangling path. The two `let _ = std::fs::remove_file` calls also discard the failure case entirely.

`permanent_delete` has the mirror-image bug with **no transaction at all** (`database.rs:2234-2255`): file unlinked, then row deleted, so a failure between them leaves a row pointing at nothing.

**Fix:** collect paths, commit the transaction, *then* unlink. Irreversible side effects must never live inside a rollback scope.

### 2.3 SQLite runs without WAL or a busy timeout, and the backup is a raw copy of a live database (High)

*Master only.*

There is no `journal_mode`, `busy_timeout` or `synchronous` pragma anywhere in the codebase; the only pragma at open time is `PRAGMA foreign_keys = ON` (`database.rs:151-157`). So the database runs in rollback-journal mode at default durability, with no configured wait on contention, while the upload worker, sync worker, AI worker and filesystem watcher all write through a shared `Arc<Database>`. Then `backup_database` takes a raw `std::fs::copy` of that live file (`lib.rs:1901`). If a write is in flight, the copy can capture a torn page set whose hot journal is not copied, producing a backup that will not open. Combined with 2.1, the user's disaster-recovery story is a possibly-corrupt file that they also cannot decrypt.

**Fix:** set `journal_mode = WAL` and a `busy_timeout` at open, and use `rusqlite`'s online backup API or `VACUUM INTO` instead of `fs::copy`.

### 2.4 Migration defects: a stale version variable, two destructive steps, and no committed schema (High)

*[also U-3.3, which independently flagged (b)]*

There **is** a real versioned migration system keyed on `PRAGMA user_version`, with 19 numbered steps each in its own transaction, and three of them correctly implement the full SQLite table-rebuild dance. That is credited in Section 9. Four defects sit on top of it.

**(a) The `version` local is never updated after eight of the migrations.** Migration 5's update is commented out literally (`database.rs:317-321`), and the same omission occurs for 7, 8, 9, 10, 11, 12 and 13. Because each gate is `if version < N` and the stale value *under*-estimates, today's effect is only "run more migrations than necessary", which the `IF NOT EXISTS` guards absorb. But `ALTER TABLE ... ADD COLUMN` is **not** idempotent, and several steps use it. This survives purely because those steps happen to be gated by a `version` still below their threshold. Anyone inserting a new migration between an assigning and a non-assigning step gets a duplicate-column error and a hard startup failure with no obvious cause. Only migration 12 does this properly, probing with `pragma_table_info` (`database.rs:416-443`).

**(b) Migration 7 drops the config table without carrying rows across** (`database.rs:338-345`). Today that only wipes app preferences, which is survivable. But it establishes a drop-and-recreate pattern in **the table that now holds the encryption bundle from 2.1**. If that pattern is ever repeated, it is unrecoverable key destruction for every user. U flagged the same `DROP TABLE IF EXISTS config` independently.

**(c) Migration 15 can delete every named person** (`database.rs:512-517`). If face embeddings were never computed, which is the **default state** because AI is opt-in and off, then `faces.person_id` is uniformly `NULL`, the subquery returns the empty set, `NOT IN` is true for every row, and all persons with their user-assigned names are deleted. There is no backup and no undo.

**(d) The schema is not committed anywhere.** Zero `.sql` files in the repository. The only definition of the schema is roughly 480 lines of string literals inside `migrate()`, replayable only from version 0. There is no snapshot to diff against, no test that runs the chain, and no downgrade path.

Also: the v5/v12 person rename left an orphaned `people` table that is never dropped, and migration 11's `CREATE TABLE IF NOT EXISTS tags` was a silent no-op on upgraded databases whose legacy `tags` table had a different shape, meaning **all tag writes failed** until migration 16 repaired it (`database.rs:525-592`). Migration 16 is careful work, and it is also evidence of how expensive (a) already was.

### 2.5 The full-text index is insert-only (High)

*[also U-3.4]*

`media_fts` is written in exactly one place, and the result is discarded: `let _ = conn.execute("INSERT INTO media_fts (file_path) VALUES (?1)", [file_path]);` (`database.rs:1069`).

Three consequences. The `let _ =` means a failed insert leaves that photo **permanently unsearchable, silently**. There is no `DELETE FROM media_fts` in `permanent_delete` or `empty_trash`, and no triggers exist, so the index accumulates rows for deleted media forever. And `add_media_synced` (`database.rs:1074-1105`), the sync-worker ingest path, never inserts into `media_fts` at all, so **every photo restored from Telegram is invisible to search**. Because search joins on the text column rather than a rowid (`database.rs:1615-1616`), stale rows also resurrect as phantom joins if a path is ever reused, and neither side of that join is indexed (2.6).

**Fix:** an external-content FTS5 table with `INSERT`, `UPDATE` and `DELETE` triggers, which removes the manual call site entirely.

### 2.6 Missing indexes on the hottest columns (Medium)

*[also U-3.4]*

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

*Master only.*

`toggle_favorite` (`database.rs:2034-2047`) issues an `UPDATE ... SET is_favorite = NOT ...` and then a separate `SELECT` to report the new value, so a double-click can return a value contradicting the stored state. A single `RETURNING` clause fixes it.

`add_to_queue` (`database.rs:1688-1707`) is a check-then-act across two statements, and there is **no `UNIQUE` constraint on `upload_queue(file_path)`** in any of the 19 migrations. The watcher asserts the opposite in a comment at `watcher.rs:171` ("This is safe because database::add_to_queue now handles its own deduplication"). That comment is wrong, and the result is duplicate uploads. `upload_worker.rs:80-98` adds a third defensive hash check at upload time, which reads like scar tissue from exactly this bug.

Relatedly, `upload_worker.rs` discards **seven** `update_queue_status` results. A dropped reset to `"pending"` leaves an item stuck in `"uploading"` forever, invisible both to `get_next_pending_item` and to the retry path, which only resets rows with `status = 'failed'`. There is no reaper for stale `uploading` rows.

### 2.8 The thumbnail cache eviction listener deletes live, DB-referenced thumbnails (High, data loss)

*[new, from U-3.1]. The highest-impact finding contributed by the merge. The master never read `cache.rs`. Re-verified against HEAD while merging: the listener is still there.*

```rust
// src-tauri/src/cache.rs:12-24
let cache = Cache::builder()
    .max_capacity(capacity)
    .async_eviction_listener(|key, value: PathBuf, cause| {
        Box::pin(async move {
            println!("Evicting thumbnail for {}: {:?} (cause: {:?})", key, value, cause);
            if let Err(e) = fs::remove_file(&value).await {
                eprintln!("Failed to delete evicted thumbnail: {}", e);
            }
        })
    })
    .build();
```

Capacity is 2000 (`lib.rs:845`). `ThumbnailCache::insert` is called on every thumbnail generation (`watcher.rs:198`, `watcher.rs:211`, `sync_worker.rs:284`), but **the cache is never read anywhere except on insert**, so it functions purely as a bounded queue with a destructor. Once a library exceeds 2,000 photos, moka evicts the oldest entries and the listener deletes the `.jpg` from disk while `media.thumbnail_path` still points at it. Nothing updates the database. The user sees broken thumbnails for their older photos, and in encrypted mode the deleted file may be the only materialized copy.

This interacts badly with 1.2: the thumbnails being deleted here are in the app data dir, while the plaintext ones in `%TEMP%` that *should* be cleaned up never are. The cleanup is aimed at exactly the wrong directory.

**Fix:** either make the cache a real read cache (consult it in the thumbnail resolution path, and update `media.thumbnail_path` to `NULL` when evicting so regeneration is triggered), or drop the eviction listener entirely and let thumbnails persist. Deleting a file that a database row still references is never correct without updating the row in the same operation.

---

## 3. Attack surface

### 3.1 A remote-control plugin is registered unconditionally in release builds (Critical)

*[also U-1.1]. Missed by S. Re-verified against HEAD while merging: still present, still ungated.*

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

No `#[cfg(debug_assertions)]` gate and no feature flag. An MCP bridge exists to let an external agent process drive the application, and such plugins conventionally open a local listener; U confirms `tokio-tungstenite` is present in `Cargo.lock` and that crates.io describes the plugin as enabling "IPC monitoring and backend inspection", meaning **any local process that can connect to the bridge reaches the privileged command surface**. This ships, always on, in a process that holds the decrypted master key in memory and has commands that read arbitrary files and upload them to Telegram (3.4).

*Partially inferred:* the plugin's own source was not read, so its bind address and auth model are unconfirmed. What is **verified** is that it is registered in every build with no gate, and that it is conspicuously absent from `capabilities/default.json`, which suggests it was added during development and never removed. Capabilities gate frontend-to-Rust IPC; they do not gate a plugin's own listener. U adds that the pinned 0.7.0 is also well behind the current 0.12.0.

**Fix:** gate it behind `#[cfg(debug_assertions)]` or delete it, then cut a new release. Until the plugin's source has been read and its bind address and auth model confirmed, treat this as the highest-priority item alongside 2.1.

### 3.2 The wrapped master key is handed to the JavaScript context (Critical)

*Master only. Missed by both S and U.*

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
async fn get_all_config(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let db_guard = state.db.lock().await;
    let db = db_guard.as_ref().ok_or("Database not initialized")?;
    db.get_all_config().map_err(|e| e.to_string())
}
```

`get_all_config` returns every row of the `config` table with no filter (`database.rs:2856-2858`). That table holds `security_bundle_v1`, which is the Argon2 salts plus the AES-GCM-wrapped master key for **both** the passphrase and the recovery wraps, and `security_telegram_credentials`, the DPAPI blob. This is not a corner case: it is called from `src/lib/api.ts:211`, which is called from `Settings.tsx:164` and `MediaGrid.tsx:661`, so **every mount of the photo grid pulls the key material into JavaScript**.

That the `security_` prefix guard exists on the write path, and that all five security keys are correctly prefixed, is strong evidence the author understood this boundary. It was simply applied in one direction.

### 3.3 CSP allows inline script and eval, and the asset scope is the whole filesystem (High)

*[also S-C1, S-C2, U-1]. The one finding all three reviews lead with. S and U both rate the asset and `fs` scopes Critical in their own scales.*

```json
// src-tauri/tauri.conf.json (app.security)
"csp": "default-src 'self' ipc: http://ipc.localhost; img-src 'self' asset: http://asset.localhost blob: data:; media-src 'self' asset: http://asset.localhost blob: data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-eval' 'unsafe-inline';",
"assetProtocol": { "enable": true, "scope": ["**", "C:\\**", "C:/**", "$APPDATA/**", "$LOCALAPPDATA/**"] }
```

`script-src 'unsafe-eval' 'unsafe-inline'` (`tauri.conf.json:25`) removes the main structural defence against script injection in a webview. `assetProtocol.scope` of `"**"` (`tauri.conf.json:26-35`) is unrestricted: the asset protocol will serve **any file the process can open** to the webview, on any platform. `capabilities/default.json:13-40` separately grants `fs:allow-read` and `fs:allow-exists` over `C:\**`, the entire system drive, which S correctly identifies as a **second, independent arbitrary-read primitive** callable straight from JS via `readTextFile` and `exists`, even though `@tauri-apps/plugin-fs` is never imported by the frontend.

U adds a fourth compounding factor: all 74 commands are exposed to the single `main` window with no per-command capability scoping (`lib.rs:1104-1195`), including `unlock_encryption`, `set_telegram_api_credentials` and `permanent_delete_media`.

Individually each of these is a configuration smell. Together with 3.2 they compose into a real chain: any script injection, whether from a crafted EXIF field rendered unsafely or a compromised npm dependency, becomes arbitrary local file read **plus** exfiltration of the wrapped key material for offline Argon2 cracking, **plus** theft of the Telegram `api_hash`, **plus** direct read of `session.db` (1.3). No `dangerouslySetInnerHTML` or `innerHTML` exists in `src/` today, so no concrete XSS vector was found by any of the three reviews, but there is zero defence in depth.

Mitigating, and worth stating: no `fs:allow-write*` and no `fs:allow-remove` are granted, and there is no `shell:` permission at all, so the plugin surface cannot write or delete. The write primitives live in custom commands instead (3.4).

**Fix:** drop `'unsafe-eval'` and `'unsafe-inline'`, narrow `assetProtocol.scope` and the `fs` scopes to `$LOCALAPPDATA/com.wanderer.desktop/**` plus the configured backup directory, remove `**` and `C:\**` entirely, filter `security_*` out of `get_all_config`, and scope capabilities per window and per command.

### 3.4 `import_files` is an arbitrary file read that auto-uploads to Telegram (High)

*Master's core finding; [also S-H1, S-H2, S-H3 and U-3.5] for the surrounding path-confinement gaps.*

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

The other four, all independently flagged by S and U:

- **`import_sync_manifest`** (`lib.rs:2325-2331` to `sync_manifest.rs:102-106`) reads an arbitrary client path and `serde_json::from_str`s it with no canonicalization, root check, version check, or entry bound, and the contents drive DB mutations (favorites, ratings, album creation). Parse errors are returned to the frontend. **[S-H1, U-3.5]**
- **`export_media`** (`lib.rs:1371-1388`) does `create_dir_all` plus `fs::copy` to any client-supplied `destination`, so decrypted photo copies can be written anywhere writable, including the Startup folder. **[S-H2]** U adds that it also builds the export folder name from **unsanitized EXIF `date_taken`** (`lib.rs:1396-1410`), which is attacker-influenced data from image metadata. **[U-3.5]**
- **`backup_database`** (`lib.rs:1862-1902`) writes a DB copy to any path. **[S-H2]**
- **`remove_local_copy` / `permanent_delete` / `download_local_copy`** operate on DB-stored paths with no root confinement (`lib.rs:1943-1975`, `database.rs:2210-2259`, `lib.rs:1977-2065`). **[S-H3]** See Section 10, item 2, for the reconciliation between S's "arbitrary file delete" framing and the master's "no command deletes a frontend-supplied path".

U also notes `import_files` dedupes by **filename only**, so two different photos with the same basename collide on import.

**Fix:** canonicalize and confine `import_files` sources to paths returned by the dialog plugin in the same session; confine `export_media` and `backup_database` destinations to an allowed root; validate `version` and bound the entry count in `import_sync_manifest`; sanitize EXIF-derived path components; and canonicalize-and-assert every delete path against the managed roots before unlinking.

### 3.5 One SQL statement interpolates free text (High)

*[also S-M1, U-3.4]. Severity spread: S and U rated it Medium, the master High. Reconciled in Section 10, item 8.*

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

**Honest exploitability assessment.** Quote-doubling *is* the correct escape for a SQLite string literal, and SQLite honours no backslash escapes inside literals, so no arbitrary-statement injection could be constructed. What is live today is a **LIKE-pattern injection**: `%` and `_` are not escaped, so a user typing `%` matches every row, and a pattern like `%_%_%_%_%` forces pathological backtracking on a full table scan of an unindexed column. S's characterization as "SQL injection (bounded)" overstates it slightly; the master's framing is the accurate one.

The reason to fix it anyway is structural. This is a hand-rolled escaper on a concatenated clause: one future filter that forgets `.replace('\'', "''")` turns it into a real injection, and because the SQL text varies per filter combination it also defeats statement caching.

There are no dynamic table names, no dynamic column names, and no interpolated `ORDER BY`, `LIMIT` or `OFFSET` anywhere in 3,264 lines, and there is **no SQL outside `database.rs`** at all.

### 3.6 Unbounded allocation from an attacker-controlled length field (Medium)

*Master only.*

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

*[also U-3.4] for the unclamped pagination half; the negative cast is master only.*

Nine query methods clamp defensively with `limit.max(0).min(1000)`. Two do not: `get_media_by_person` (`database.rs:2758-2777`) and `get_media_by_tag` (`database.rs:3074-3092`) pass frontend values straight through, and as U notes, `limit = -1` means "no limit" in SQLite.

Worse, in `semantic_search` (`lib.rs:2469-2473`), `limit` is an `i32` from the frontend and `-1i32 as usize` is `usize::MAX`, so a negative limit takes everything. Those IDs then flow into `get_media_by_ids`, which builds one `?` placeholder per ID, blowing past `SQLITE_MAX_VARIABLE_NUMBER`. `bulk_delete` and `bulk_set_favorite` have the same unbounded-placeholder exposure with an arbitrary-length `Vec<i64>`.

### 3.8 Migration leaves plaintext copies in Telegram and reports success anyway (Medium)

*Master only.*

During migration to encrypted mode, the old plaintext message is deleted with the result thrown away (`lib.rs:609-615`). `let _ =` swallows rate limits, network errors and partial deletes, and `delete_messages` itself only reports `pts_count`, which can be lower than requested. The migration is then marked `succeeded` regardless (`lib.rs:622-623`). So plaintext originals can remain in Telegram cloud storage forever, with no record of which ones, while the app reports the library as fully migrated to encrypted. For a user who enabled encryption specifically to remove plaintext from Telegram, this silently fails to deliver the thing they asked for.

**Fix:** verify the deletion, retry on `FLOOD_WAIT`, and record un-deleted message IDs in a durable pending-purge list surfaced in the UI.

### 3.9 FFmpeg is resolved from `PATH` (Medium)

*[new, from S-M2]. Neither the master nor U covered this. The master's positives section notes that the three process spawns correctly pass argv arrays with no shell, which is true and is a different question from which binary gets executed.*

`media_utils.rs:158-234` invokes `ffmpeg` by bare name, first probing with `Command::new("ffmpeg").arg("-version")` (`media_utils.rs:177`) and then running it for video thumbnail extraction (`:183`, with a fallback at `:212`). The argv array construction correctly avoids shell injection, but a planted `ffmpeg.exe` earlier in the user's `PATH` executes on the next video import, in-process with the app's privileges and, in encrypted mode, with the vault potentially unlocked. On Windows the current-directory and per-user `PATH` entries make this a realistic local privilege-escalation vector rather than a theoretical one.

**Fix:** bundle ffmpeg and invoke it by absolute path from the app's resource directory, or resolve and pin the absolute path once at startup and validate it.

### 3.10 `set_config` allows arbitrary key writes outside the `security_` namespace (Low)

*[new, from S-L1]. The master treats the `security_` prefix guard purely as a positive (Section 9), which it is, but S is right that a denylist is the weaker construction.*

`set_config` (`lib.rs:1686-1694`) rejects any key starting with `security_` and accepts everything else. That is a denylist on a config table whose rows drive real behaviour: AI enablement flags (`ai_face_enabled`, `ai_tags_enabled`), backup directory, sync settings. Injected script can write arbitrary keys and arbitrary values, including keys the backend will later parse with `unwrap_or` defaults, and can flip AI processing on for a user who opted out.

**Fix:** invert it to an allowlist of settable keys, and validate each value against its expected type or domain at the boundary.

---

## 4. Rust code quality

### 4.1 A single panic bricks the database for the process lifetime (High)

*Master only, and it directly contradicts S, which lists "graceful poisoned-mutex recovery" as a strength. See Section 10, item 1: the master is correct.*

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

This maps poison to `Err` and returns it. `std::sync::Mutex` poisoning is **permanent**, so after one panic anywhere in a DB call, all 89 database methods fail forever with "Mutex poisoned" and all 74 commands degrade to error strings until the app is restarted. The honest fix is one call: `.unwrap_or_else(|e| e.into_inner())`.

The most likely trigger is debug code left in a production write path (`database.rs:696-708`): a `PRAGMA foreign_key_list('faces')` block with `println!("DEBUG FK: faces -> {}", fk.unwrap())` that runs **while the connection guard is held**, so any row-decode error panics with the lock held and poisons it permanently. The whole block also executes on every single face embedding stored.

### 4.2 The global lock is held across blocking work and CPU-bound inference (High)

*[also U-3.2, which additionally flags the Telegram client mutex]*

There are two lock layers: `AppState.db: Mutex<Option<Arc<Database>>>` (a **tokio** mutex) wrapping `Database.conn: Mutex<Connection>` (a **std** mutex).

The std guard never crosses an `await`. Every one of the 89 methods acquires and releases inside a synchronous body. **That is the single most important thing to get right with `Mutex<Connection>` in an async application, and it is right everywhere**, which is why this is High rather than Critical.

The tokio guard is the problem. `index_pending_clip` (`lib.rs:2513-2542`) holds it across synchronous ONNX inference for an entire batch, so no other command can touch the database and no other task on that thread progresses. `detect_faces` gets this right using `spawn_blocking` (`lib.rs:811-814`); this does not. U independently flags the same pattern in `semantic_search` (`lib.rs:2450-2492`, holding the DB mutex through the whole embedding scan and sort), `scan_duplicates` / `find_duplicates` (`lib.rs:1505-1513`, `1562-1574`), and adds two the master did not cover:

- **`sync_worker` re-hashes files with `hash_file_streaming` inline every 60 seconds** for the 20 newest Telegram messages (`sync_worker.rs:55`, `88`, `201`). Videos can be 10 GB or more, and this is a full read on the async runtime.
- **The Telegram `client` mutex is held across entire uploads and downloads** (`telegram.rs:276-338`, `431-460`), blocking sync, view and delete for the duration of a multi-gigabyte transfer.

The per-request equivalent is `materialize_media_items_for_response` (`lib.rs:193-202`): per item, a blocking `is_encrypted_file` read, a **fresh async lock acquisition**, `create_dir_all`, two `metadata` calls, and a full `decrypt_file`. For a 200-item gallery page that is 200 async lock round-trips and up to 200 synchronous AES-GCM decryptions on a runtime thread, and it runs on the return path of about a dozen commands. `lib.rs` contains **39** `std::fs::` calls, nearly all inside `async fn` bodies with no `spawn_blocking`. Thumbnailing already uses `spawn_blocking` (`media_utils.rs:85`, `175`); the same pattern should cover all of the above.

No deadlock is reachable today, but the lock *ordering* is inconsistent: `lib.rs:511` takes `security_runtime` then the DB lock, while `sync_worker.rs:60-68` takes the DB lock then `security_runtime`. That inversion is currently harmless only because the locks are held so briefly.

### 4.3 Duplicate detection is O(n squared) with two allocations per comparison, under the global lock (High)

*[also U-3.4]*

`database.rs:2653-2660` compares all pairs, and `hamming_distance` re-parses **both** hashes from base64 into an `ImageHash` on every call (`database.rs:113-131`). At 10,000 photos that is roughly 50M iterations with about 100M heap allocations, all while holding the connection guard taken at `database.rs:2567` for the entire function, so the UI and every background worker stall. The function also loads every candidate row with all 24 columns into memory first.

**Fix:** parse each hash once into a `Vec<u64>` up front, and bucket by phash prefix instead of comparing all pairs.

Related performance findings: **four N+1 query patterns** (`export_sync_manifest` at `lib.rs:2291-2302` runs two correlated subqueries per photo over the whole library; `scan_duplicates` at `lib.rs:1562-1573` and `find_duplicates` at `lib.rs:1504-1514` both reacquire the global lock *inside* the loop with one implicit transaction and one fsync per photo; `reconcile_cloud_only_flags` at `database.rs:2436-2441` does a blocking `Path::exists()` plus an individual `UPDATE` per candidate **at startup**). And `semantic_search` loads every CLIP embedding in the library into memory on each search, which the author flagged honestly in a comment at `lib.rs:2452-2453`.

### 4.4 Eighty discarded results, several hiding real state corruption (Medium)

*Master only.*

There are **80** `let _ =` and **32** `.ok()` sites. Most are legitimately fire-and-forget. These are not:

- `database.rs:1069`: the FTS insert, which silently loses searchability (2.5).
- `lib.rs:2349-2356`: the entire `import_sync_manifest` merge writes through discarded results, and `updated_count` increments regardless, so the command reports `"Synced N items"` after failing all N of them.
- `upload_worker.rs`: seven discarded `update_queue_status` calls, which can strand items in `"uploading"` forever (2.7).
- `sync_worker.rs:356`: a discarded `mark_media_encrypted_by_path`, after which the item is treated as unencrypted forever and the migration will re-upload it.
- `database.rs` has **eight** `filter_map(|r| r.ok())` sites that silently drop unreadable rows mid-iteration. At `2274` in `empty_trash` a trashed item is skipped and never deleted; at `2920` in `get_all_media_for_sync` an item silently vanishes from the sync manifest.

### 4.5 `errors.rs` is dead code and all 74 commands return `Result<T, String>` (Medium)

*Master only.*

`errors.rs` defines a well-structured `AppError` with `#[derive(Error, Serialize)]`, a `#[serde(tag = "type", content = "message")]` representation, seven variants, and `From` impls for `rusqlite::Error` and `std::io::Error`. It is referenced **zero** times outside its own file. Every command instead does `.map_err(|e| e.to_string())`, which flattens typed errors into opaque strings the frontend cannot branch on and leaks raw SQL and IO error text, including absolute filesystem paths, into the UI.

This has a concrete downstream cost: `App.tsx:50` retries startup by **string-matching** `message.includes("Database not initialized")`, a contract that breaks the moment the Rust error text changes (6.3).

### 4.6 Structure and duplication in `database.rs` (Medium)

*Master only. S and U both catalogue the frontend god files (6.6) but neither read `database.rs` closely enough to find this.*

`database.rs` is 3,264 lines with **89 public methods** across **three separate `impl Database` blocks** (lines 137, 2820, 2872) with no module boundary, two of which are unlabelled continuations. It mixes schema migration, media CRUD, queue management, albums, tags, face clustering, CLIP vector storage, config storage and duplicate detection, and it embeds two clustering *algorithms* (union-find at `2626-2683`, greedy face matching at `2743-2822`) in the data-access layer.

The duplication is measurable and the fix is unusually cheap. The 24-field `MediaItem` row mapping is written out inline **17 times**, and the long `SELECT id, file_path, file_hash, ...` column list is duplicated **15 times**. A `map_media_row` helper **already exists** at `database.rs:1401-1434` and is used exactly **3 times**. Applying the existing helper at the other 17 sites would delete roughly 500 lines with no behaviour change, and it is the single highest-value, lowest-risk refactor available in this codebase. Because the column list is manual and positional, adding a column today means editing 15 strings and 17 mappings in lockstep; the defensive `row.get::<_, Option<i32>>(21)?` pattern on the newer columns reads like scar tissue from that exact failure.

`lib.rs` at 2,545 lines holds all 74 command handlers plus startup wiring in one file, and would split cleanly along the same domain lines.

### 4.7 Smaller Rust findings (Low)

Master's list, with U's and S's additions merged in and attributed.

- **`progress_stream.rs:66`**: `self.bytes_read.try_lock().unwrap()` inside `poll_read`, on the upload hot path. It should never contend, but the panic would land inside an in-flight Telegram upload. It is also unnecessary: the method takes `Pin<&mut Self>`, so a plain `u64` field removes both the `Arc<Mutex<_>>` and the panic. **[also S-L2, U-3.5]**
- **`escape_like_pattern` does not work.** `media_utils.rs:252-256` escapes `%` and `_` with backslashes, but no query uses an `ESCAPE '\'` clause, and SQLite only honours a backslash escape when one is specified. So the function fails to neutralize wildcards *and* inserts literal backslashes that make a search for `my_photo` fail to match `my_photo`. Its unit tests assert the string transformation rather than the SQL behaviour, so they pass while the feature is broken. Mitigating: the only caller, `Database::search_media`, is dead code. `Database::get_persons` is likewise dead.
- **FTS5 query construction can produce syntax errors on ordinary input.** `database.rs:1606-1610` maps each token to `"\"{}\"*"`, so a token consisting only of `"` becomes `""*`, an FTS5 syntax error surfaced raw to the user. Correctly bound as a parameter, so not injection.
- **`sync_worker.rs:148`**: `.unwrap()` on `to_str()` of a non-UTF-8 path, inside a spawned worker, where a panic silently kills sync for the session. Every other path conversion uses `to_string_lossy()`. **[also S-L2, U-3.5]**
- **The remaining 6 non-test `unwrap()`s are safe by invariant**: `database.rs:778` and `920` are guarded by preceding length checks, `793` is reachable only when a preceding comparison assigned the value, and `ai/worker.rs:48` unwraps a runtime build inside a dedicated thread. S rates these higher (S-L2 treats the BLOB-to-array conversions as panic risks); the guards make them safe today, but checked handling would be cheap insurance.
- **`unchecked_transaction` used once inconsistently** (`database.rs:3224`) where the other five transactions use the checked `conn.transaction()`.
- **50 `println!` calls** bypass the initialized `env_logger` and go to a stdout that the `windows_subsystem = "windows"` release attribute discards. Several log user file paths and IDs. **[also S-M4, U-3.5]**
- **Private Telegram message text is logged.** `telegram.rs:139-141` logs `message.text()` of every incoming message. **[new detail, from U-2.6]** S-M4 flags the related logging of Telegram message IDs and full file paths. All three agree that no *secrets* are logged, which was verified independently by each pass.
- **`cargo fmt --check` reports drift in 7 files**: `ai/object_detection.rs`, `ai/worker.rs`, `clip.rs`, `database.rs`, `lib.rs`, `security/mod.rs`, `upload_worker.rs`.
- **A non-cryptographic RNG exists in the tree.** `sync_manifest.rs:212-231` seeds an unkeyed `DefaultHasher` from a timestamp. It feeds only `generate_device_id()` and touches **no** key, nonce, salt or recovery key. Harmless today, but it is a foot-gun sitting next to a crypto module. U-3.5 flags the same code as "weak device-ID entropy", which is the practical consequence: device IDs are predictable and collidable across machines that start within the same clock tick.
- **Dead `.env` machinery.** `dotenvy::dotenv().ok()` is called at `lib.rs:840` and `.env.example` advertises `TG_ID` and `TG_HASH`, but neither is ever read; credentials come exclusively from the DPAPI-protected config. Meanwhile **`.env` is not gitignored**, so a developer following `.env.example` would have real credentials staged by default, for no functional benefit. **[also U-2.6]**
- **`windows-sys` is not target-gated** (`Cargo.toml:63`), and the non-Windows DPAPI stubs hard-error, so onboarding cannot complete on Linux or macOS while `bundle.targets` is `"all"`. The stubs correctly **fail closed** with no plaintext fallback, which is right; the packaging is what is wrong. S-H4 frames the same gap from the product side: `set_telegram_api_credentials` simply cannot store credentials securely off Windows, and the long-term fix is an OS keychain abstraction rather than DPAPI.
- **Leftover LLM self-dialogue comments in `setup()`** (`lib.rs:878-901`), and a shipped `debug_reset_faces` command. **[new, from U-3.5]** The master notes `debug_reset_faces` only in passing, as the one backend command with no frontend caller.

---

## 5. README accuracy

*Master only. Neither S nor U audited the documentation, and U's only overlapping remark is that the README is "genuinely good end-user documentation, honest about the plaintext local `backup/` folder and the one-way encryption toggle", which agrees with the finding below.*

A user-facing README that makes security promises is part of the security posture: a false promise there causes users to take real risks. 51 concrete factual claims were checked against the code.

**The result is unusually good.** 34 claims verify as fully true, including **every one of the six security claims**, and including the self-critical one. Two are false, both about distribution rather than behaviour.

### 5.1 The six security claims all verify

| Claim | Verdict | Evidence |
| --- | --- | --- |
| "Files are encrypted before Telegram cloud upload" | **True** | `upload_worker.rs:146-155` encrypts to a temp `.wbenc` before `upload_file_with_progress`. See 1.1 for the fail-open caveat. |
| "Thumbnails are encrypted at rest" | **True** | `watcher.rs:231-246`, and it deletes the plaintext thumbnail rather than leaving it when the vault is locked. |
| "View cache is encrypted at rest" | **True** | `lib.rs:2150-2168` writes only `.wbenc` blobs into `view_cache/`. |
| "Database backup artifact is encrypted" | **True** | `lib.rs:1914-1922`. True, and unfortunately also finding 2.1. |
| "API ID and hash are stored locally with Windows DPAPI" | **True** | Real `CryptProtectData` with `CRYPTPROTECT_UI_FORBIDDEN` and user scope, `security/mod.rs:481-501`. See 1.9 for the entropy caveat. |
| "Local files in `backup/` are still plaintext at rest" | **True** | Correct, and **voluntarily disclosed**. `encrypt_file` is never applied to the `backup/` tree. |

That last row deserves emphasis. A README that spends a section accurately explaining what its own encryption does **not** protect, without being asked, is rare.

All seven documented storage paths under `%LOCALAPPDATA%\com.wanderer.desktop\` are exactly right. "AI is opt-in, default OFF" is true and enforced in the schema (`database.rs:349-350`, `606-607`). The onboarding flow, the one-way encryption warning, the recovery-key verification step, the `tg://` share-link caveat, the partial RAW support, the unimplemented mobile companion and the incomplete metadata preservation are all accurate.

### 5.2 The download links point at the wrong repository (High, documentation)

```
README.md:34-36
- Releases page (all versions): https://github.com/ronimuliawan/Wanderer/releases
- Direct download (Windows x64, v0.0.0): https://github.com/ronimuliawan/Wanderer/releases/download/0.0.0/Wanderer._0.0.0_x64-setup.exe
- Direct download (Windows x64, latest): https://github.com/ronimuliawan/Wanderer/releases/latest/download/Wanderer._0.0.0_x64-setup.exe
```

The repository's actual remote is `https://github.com/rons-space/Wanderer`. All three download links point at a **different owner namespace**.

For most projects this would be a Medium typo. Here it is High, because the artifact being distributed is an **unsigned Windows installer** (7.2) for an application that will hold the user's entire photo library and a Telegram account credential. "Download this unsigned .exe from a GitHub namespace that is not the project's" is precisely the shape of a supply-chain phishing instruction, and a user has no way to tell the difference.

The one populated in-app link points at a **third** project name: `Settings.tsx:49-53` sets `github: "https://github.com/ronimuliawan/wanderbackup-rust"`. So the repository, the README's download links, and the app's own About tab name three different GitHub locations.

### 5.3 "Production build: `npm run build`" is false (Medium, documentation)

`"build": "tsc && vite build"` (`package.json:8`) type-checks and bundles **the frontend only**, emitting `dist/`. It succeeds in 2.71 seconds and produces no application. The production build is `npm run tauri build`, which is never mentioned.

### 5.4 Claims that are true but materially incomplete (Medium, documentation)

- **"Thumbnails / view cache are encrypted at rest"** omits that viewing anything writes an unencrypted copy into `%TEMP%` that is never cleaned up (1.2). This is the most important omission in the file.
- **"If you lose both passphrase and recovery key, encrypted data is unrecoverable"** is true, but the README never warns about the case that actually bites: losing `library.db` while *retaining* both secrets is **also** unrecoverable (2.1).
- **"Minimum 8 chars"** is enforced on the initialize path but **not** on the recovery/reset path (1.8).
- The README does not mention that the metadata index, including GPS coordinates, is plaintext (1.10).
- Not mentioned anywhere: thumbnails for libraries over 2,000 photos are silently deleted from disk (2.8).

### 5.5 Audit summary

| Verdict | Count |
| --- | --- |
| True | 34 |
| True but materially incomplete | 5 |
| True with a backend enforcement gap | 1 |
| True but the cited link is wrong | 1 |
| **False** | **2** (production build command; release URLs) |
| Unverifiable from the repository | 4 |

Failures cluster in exactly two places: developer-facing instructions, and distribution metadata. The user-facing security documentation is accurate and, in places, more forthcoming than it had to be.

---

## 6. Frontend

The frontend is a hand-rolled single-`view` router (`App.tsx`) over ~70 thin `invoke()` wrappers (`src/lib/api.ts`), no state library, with grid virtualization written by hand. `src/types.ts` mirrors the Rust structs accurately. All three reviews agree on the largest files: `Settings.tsx` (1,302 lines, five unrelated concerns), `MediaGrid.tsx` (914), `AppSidebar.tsx` (747, two near-duplicate sidebar trees), `Onboarding.tsx` (701, 21 `useState`).

### 6.1 The Settings path to enable encryption can silently destroy recoverability (High)

*Master only.*

There are **two** paths that enable encryption, and they diverge badly. Onboarding does it correctly (Section 9). Settings (`Settings.tsx:601-609`) renders the recovery key as static text and is missing **everything that makes the onboarding flow safe**: no verification step, so nothing forces the user to have actually read the key (onboarding requires retyping two segments, `Onboarding.tsx:170-194`); no Download, Print or Copy buttons; and the state is never cleared, so despite the "(shown once)" label nothing enforces that. A user can enable encryption from Settings, navigate away without reading the key, and has now permanently lost the ability to recover if they forget the passphrase.

This is a direct consequence of the duplication in 6.6: the same security-critical operation implemented twice, once carefully.

**Fix:** extract one `<EnableEncryption>` component containing the verification gate and the save affordances, and use it in both places.

### 6.2 Recovery-key handling defects in the onboarding flow (High)

*Master only.*

Three issues around the one-time display of an unrecoverable secret.

**The print window is never closed and fails silently** (`Onboarding.tsx:101-111`). After `print()` returns, or if the user cancels, a window containing the plaintext master-recovery secret stays open on the desktop indefinitely. And `if (!printWindow) return` is a **silent** no-op: in a Tauri WebView2 window with `decorations: false`, `window.open` is likely blocked, so the user clicks Print and nothing happens, with no toast and no error. For the only display of an unrecoverable key, a silent no-op is a data-loss path.

**The clipboard copy has no error handling.** `Onboarding.tsx:526-528` calls `navigator.clipboard.writeText(recoveryKey || "")` with the promise unhandled and no success toast, so a permission failure is indistinguishable from success. A safe helper already exists at `Settings.tsx:240-251`.

**The download revokes the blob URL synchronously after `click()`** (`Onboarding.tsx:89-99`), and the anchor is never appended to the document. Both work in current WebView2, but this is a known-fragile idiom for a one-shot secret.

### 6.3 Startup retries forever by string-matching a Rust error message (High)

*[also S-F3, U-4.2]*

`App.tsx:43-63` catches a startup failure, tests `message.includes("Database not initialized")`, and calls `setTimeout(..., 250)` to retry. Four problems in twenty lines. The timeout handle is never captured or cleared, so the effect has no cleanup and under React 19 `StrictMode` (`main.tsx:11`) two independent 4 Hz polling chains start in development. There is **no retry cap and no timeout**, so if the Rust side never initializes the database the app sits on "Loading secure startup..." polling IPC forever with no user-visible error and no way out. The retry decision is made by string-matching an error message, which is the downstream cost of the dead `errors.rs` in 4.5. And `catch (e: any)`.

### 6.4 The photo grid remounts every visible cell on every parent render (High)

*[also S-F5]*

`Gallery.tsx:158` (`SelectableItemWrapper`) and `Trash.tsx:124` (`ItemWrapper`) define components **inside** render. Both are new function identities on every render, and both are passed as the `ItemWrapper` **component type** prop, which `MediaGrid.tsx:476` uses as a JSX element. React compares element types by reference, so a new type means unmount and remount of that subtree. In `Gallery`, `selectedIds` changes on every click, so **every selection toggle tears down and rebuilds every visible cell's DOM**, including the `<img>` elements, which re-enter the network and decode path.

This compounds with two more issues. `VirtualGrid.handleScroll` triggers **two** state updates per scroll event (`MediaGrid.tsx:260`, `855`), one unconditionally. And there are **zero** `React.memo` usages in the codebase, while `MediaGrid` passes ten `on*` handler props with none wrapped in `useCallback`. So every scroll frame re-renders the grid and every visible `Cell`, and each `Cell` re-runs `convertFileSrc` and rebuilds a full Radix context menu subtree with a six-item rating submenu.

Also: the grid cell key is an **absolute index** into the array (`MediaGrid.tsx:335`) while `handleDelete` and `handleArchive` splice the array, so every item after a removal shifts index and React reuses the wrong DOM node and image for a different photo. `DuplicateReview.tsx:206` has the same hazard.

**Fix:** hoist the wrappers to module scope, `memo()` the `Cell`, `useCallback` the handlers, key by `item.id`, and throttle scroll state through `requestAnimationFrame`.

### 6.5 Zero accessibility affordances (High)

*Master's detail; [also U-4.3] at a summary level ("accessibility is minimal").*

There are **0** `aria-label` attributes in application code (2 in the repository, both in generated shadcn files) against **26** `size="icon"` buttons and 9 raw `<button>` elements. A screen reader announces the favourite toggle, the eight repeated Copy and ExternalLink pairs in the About tab, and the mobile menu control as just "button".

Keyboard navigation is absent from the core surface. The clickable grid cell is a `<div onClick=...>` (`MediaGrid.tsx:412-415`) with no `tabIndex`, no `role` and no `onKeyDown`, and across the whole application there are exactly **2** occurrences of `onKeyDown`, `tabIndex` or `role=` combined. **The photo grid is entirely unreachable by keyboard.** `MediaViewer` has no arrow-key navigation, and structurally cannot: it receives a single `item` prop rather than a list and an index. Escape and focus trapping work only because Radix `Dialog` provides them.

Enabling `eslint-plugin-jsx-a11y` (7.3) would catch most of this mechanically.

### 6.6 Dead code, triplicated flows, and a hand-rolled event bus (Medium)

*[also S (section 3) and U-4.3], which independently identified the same dead files and the same duplicated pagination.*

**Three separate implementations** of the Telegram phone-and-code login exist: `Onboarding.tsx:218-252`, `Settings.tsx:316-346`, and `LoginView.tsx:20-49`. `LoginView.tsx` is **never imported or rendered anywhere**, and it is also the only file in the app that calls `invoke()` directly rather than going through the typed `api.ts` layer, plus the only source of "Check console" error strings and a `"Log out (Stub)"` button. Also fully dead: `Sidebar.tsx` (147 lines, still containing a 2-second polling loop and the app's only two `alert()` calls) and `ThemeSwitcher.tsx` (57 lines, now inlined into Settings). All three reviews name the same three files.

`Settings.tsx` at 1,302 lines holds **17 `useState` hooks** spanning five unrelated domains, does six independent fetches on mount, and returns one 940-line block of JSX. Its About tab alone is roughly 140 lines of four copy-pasted link rows.

There is **no state management library and no server-state cache**. Cross-cutting auth state is propagated through a hand-rolled pub/sub over `window` (`Settings.tsx:144` dispatches, `AppSidebar.tsx:134` listens). `MediaGrid` refetches `getAllConfig()` **and** `getAlbums()` on every mount, and it is mounted by seven different parents. `getAlbums()` is independently fetched in three places.

The `loadNextPage` pagination block is copy-pasted across **seven** components in **two mutually incompatible variants**. `Favorites.tsx` and `Archive.tsx` are byte-for-byte identical except for one API method name and two log strings. The variant used by `Favorites`, `Archive` and `Trash` appends without de-duplicating by id, so any overlapping page, which fixed-offset pagination trivially produces when an item is deleted mid-scroll, yields **duplicate React keys**. S sizes the fix at roughly 400 lines deleted via one `usePaginatedMedia(fetcher)` hook plus a `MediaListView` shell, which would also fix 6.10 and the duplicate-key bug in one place.

`MediaGrid` also **owns mutations and a shadow copy of items** (`MediaGrid.tsx:647-657`), keeping `localItems` synced from props and mutating it in eight handlers, so there are two sources of truth and parents that do not pass `onItemsChange` never learn about deletions. **[S, U-4.1]**

### 6.7 Error handling and other frontend findings (Medium / Low)

- **The error boundary does not cover the whole app.** `ErrorBoundary` is mounted inside `App` (`App.tsx:81-108`), but `main.tsx:12` mounts `ThemeProvider` **outside** it. `ThemeProvider` reads `localStorage` in six lazy initializers and throws from `useTheme` (`ThemeContext.tsx:186`), so a failure there escapes the boundary and produces a blank white window. The fallback also has **no reset or reload button** and renders raw error text including absolute paths. Nothing is persisted or reported. **[also S-F7, U-4.2]**
- **Tauri event listener cleanup is broken in two places.** `Settings.tsx:125-133` and `Gallery.tsx:60-87` assign the unlisten function inside a `.then()`, so unmounting before the promise resolves leaves the listener registered forever, calling `setState` on an unmounted component. The correct pattern **already exists** at `UploadQueue.tsx:80-86`. **[also S-F3, U-4.2]**
- **`MediaGrid` mirrors props into state** (`MediaGrid.tsx:648-657`), double-rendering on every load and fighting the four optimistic-update handlers in the same file. **[also S, U-4.1]**
- **A scroll timeout is never cleared** (`MediaGrid.tsx:861`), firing `setState` after unmount. **[also S-F3]**
- **`Search.tsx:82-94`** has a `useEffect` whose dependency array lists only `[selectedTag]` while the closure reads `query`, `hasSearched` and four filter states, and whose body contains five lines of question-mark comments admitting the control flow is not understood. **[also S-F1, U-4.1]** All three reviews found this one independently.
- **Errors swallowed:** four bare `.catch(console.error)` sites where a failed `getAlbums()` renders as "No albums", and three places where an IPC failure is rendered identically to "logged out", including `Settings.tsx:310-313` which silently boots the user to the login form on any transient error. S-F6 adds `Gallery.tsx:77-79` (`getMedia().then(setItems)` with no `.catch`) and notes that load failures in Favorites, Archive, Trash, PersonDetail, AlbumDetail, Search and MapView go to `console.error` only, so the user sees an empty view indistinguishable from "no items".
- **The error banner is unreadable.** `Settings.tsx:426` styles it `bg-red-50 text-red-500`, raw Tailwind palette values rather than the app's semantic `destructive` token, while the app defaults to dark mode.
- **Three confirmation idioms** coexist: native `confirm()` (`Settings.tsx:137`, `BulkActionBar.tsx:116`), the Tauri dialog plugin's `ask()` (`AppSidebar.tsx:140-144`), and a Radix `AlertDialog` (`Trash.tsx:148-187`).
- **`BulkActionBar` fires N sequential IPC calls** for "Cloud Only" (`BulkActionBar.tsx:125-133`) while every other bulk action in the same file uses a real batch command. 2,000 photos means 2,000 serialized round-trips behind one spinner.
- **Non-virtualized grids elsewhere** load 100 full images at once with no `loading="lazy"` (`SmartAlbums.tsx`, `Tags.tsx`, `DuplicateReview.tsx`); only 2 `loading="lazy"` attributes exist. `MapView.tsx:35` fetches 500 rows in one shot.
- **The map cannot render.** `MapView.tsx:91-92` loads OpenStreetMap tiles over `https:` and `MapView.tsx:15-17` loads Leaflet marker icons from `unpkg.com`, but the CSP's `img-src` allows no `https:` host, so tiles and markers are blocked in packaged builds. **[also U-4.1]**, which additionally frames it as a privacy contradiction: a "photos never leave your device" app making outbound requests to a third-party tile server. Silver lining: because the CSP blocks them, photo GPS coordinates are **not** currently leaking. Decide which behaviour you want.
- **Dead and duplicated UI**: a doubled `<ContextMenuSeparator />` (`MediaGrid.tsx:576-578`), a `theme` prop threaded through two components only to be explicitly ignored (`MediaGrid.tsx:158`), a permanently disabled "Log Out (Not Implemented)" button directly beneath a working "Disconnect Account" (`Settings.tsx:469-475`), three hardcoded fake tags with no `onClick` while a real tag system exists (`AppSidebar.tsx:371-388`), three separate controls navigating to the same timeline view, and a fabricated display email (`{(user || "guest") + "@wander.app"}`). **[also U-4.3]**
- **Cosmetics:** `index.html:7` still reads `<title>Tauri + React + Typescript</title>`, `package.json` still declares `"name": "tauri-app"`, and `vite.config.ts:5` has a **disarmed** suppression comment (`// ts-expect-error`, missing the `@`) so it does nothing. Two debug banners log on every launch (`main.tsx:7-8`). **[also S, U-5]**

### 6.8 The Tags view hand-builds an `asset://` URL and is broken on Windows (Medium)

*[new, from S-F4]. Neither the master nor U caught this. Re-verified against HEAD while merging: `Tags.tsx:86` still constructs the URL by hand.*

```tsx
// src/components/Tags.tsx:84-88
? `asset://localhost/${encodeURIComponent(item.thumbnail_path)}`
```

Every other thumbnail in the app goes through Tauri's `convertFileSrc`, which emits the platform-correct form. On Windows, the app's primary and only shipped platform, the custom protocol is served from `http://asset.localhost/` rather than `asset://localhost/`, so this URL does not resolve and the Tags view renders broken images. S rates it a 15-minute fix and lists it as the single highest-value quick win in the report, which is a fair call: it is likely user-visible today.

**Fix:** `import { convertFileSrc } from "@tauri-apps/api/core"` and use it, as `MediaGrid` already does.

### 6.9 Additional functional frontend bugs (High / Medium)

*[new, from U-4.1]. These five were found only by U, which read the pagination, upload-queue and duplicate-review paths more closely than the other two passes.*

- **Infinite scroll can deadlock on large viewports (High).** Pagination is triggered only inside the scroll handler (`MediaGrid.tsx:839-845`). With a 20-item initial load (`Gallery.tsx:66`) on a wide or 4K window, the content may not overflow the container, `onScroll` never fires, and `loadNextPage` is never called. The user sees 20 photos and no way to reach the rest. This deserves the High rating: on the target hardware for a desktop photo manager, it is a plausible default state, not an edge case.
- **`media-added` silently resets pagination (Medium).** `Gallery.tsx:75-80` replaces `items` with only the first 20 on every event, snapping a deep-scrolled user back to the top whenever the watcher ingests a file.
- **Trash exposes invalid actions via nested context menus (Medium).** `Trash.tsx:39-50` wraps items in a restore-only menu, but `MediaGrid`'s cell always adds its own full menu (delete, add to album, archive), so two Radix menus nest and the user is offered actions that make no sense on a trashed item.
- **Rate-limit progress assumes 60 seconds (Medium).** `UploadQueue.tsx:198` computes `(1 - countdown/60)*100`, so a Telegram `FLOOD_WAIT` longer than 60 seconds produces negative progress. Telegram routinely returns multi-minute waits.
- **Missing fallback asset (Low).** `DuplicateReview.tsx:26,229` falls back to `/placeholder.jpg`, which does not exist in `public/`, and the `onError` handler re-assigns the same missing URL, so a failed thumbnail produces an infinite error loop.

Also from U, on the credential-handling side: the Telegram `apiHash` is rendered in a **plain, non-masked text input** in `Onboarding.tsx:620-627`, so it is shoulder-surfable and will be captured by screen recordings and screenshots during onboarding.

### 6.10 No request sequencing anywhere: stale responses overwrite fresh ones (Medium)

*[new, from S-F2]. The master covers the `Search.tsx` instance under 6.7; S grepped for the general pattern and found it is systemic, with zero cancellation anywhere in the codebase.*

There are **no `AbortController`s and no request guards** in the frontend. Within any view, a slower earlier response wins over a faster later one:

- `Search.tsx:123-180`: overlapping searches resolve out of order and clobber results.
- `Tags.tsx:33-41`: click tag A then tag B, and A's photos render under B's header.
- `SmartAlbums.tsx:65-85`: same pattern.
- `MediaViewer.tsx:73-121`: open a slow cloud-only item A, close it, open B, and A's content overwrites B when it finally arrives. In encrypted mode this means the viewer can display the wrong decrypted photo.

**Fix:** a monotonic request-id or `active` flag inside the shared fetch hook proposed in 6.6, which fixes all four sites at once.

---

## 7. Operational readiness

### 7.1 No CI of any kind (High)

*[also S, U-5]. All three reviews independently identify this as the highest-leverage single item.*

There is **no `.github/` directory** (re-verified at HEAD). Nothing runs on push or pull request: not `cargo build`, not `cargo clippy`, not `cargo fmt --check`, not `cargo test`, not `tsc`, not a lint, not a bundle.

The consequences are visible throughout this report rather than hypothetical. A job that ran only the commands the README documents would have caught the false build claim (5.3) immediately. `cargo fmt --check` would have kept 7 files formatted. `cargo clippy -D warnings` would very likely have flagged the held-lock-across-blocking-work in 4.2 and several of the 80 discarded results in 4.4. And a job that ran the 8 existing Rust tests would at least prove the crate compiles, which no automated system has verified for this commit.

It is roughly 40 lines of YAML and it converts most of the other findings from "will drift again" to "cannot drift again".

**Fix:** add `.github/workflows/ci.yml` running `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `npm ci`, `npm run build` and `npm test` on pull requests to `main`. Then add a migration test that runs the chain from version 0 to 19 (2.4d).

### 7.2 The installer is unsigned and there is no updater (High)

*[also U-5]*

`tauri.conf.json` has no `plugins.updater` block, no `pubkey`, and no code-signing configuration, while `bundle.targets` is `"all"`. So the artifact the README tells users to download is an **unsigned Windows executable** that triggers a SmartScreen warning, and there is **no mechanism to ship any of the fixes in this report to anyone who has already installed it**.

That interacts badly with 2.1, 2.8 and 3.1. If the backup defect, the thumbnail deletion bug and the shipped MCP bridge are real in a distributed build, there is currently no push channel to remediate them.

Also, `targets: "all"` emits an MSI alongside the NSIS installer, and the README documents only the `.exe`.

**Fix:** add `tauri-plugin-updater` with a signing `pubkey`, set up Authenticode signing, pin `targets` to `["nsis"]`, and add a `release.yml` workflow so releases are reproducible rather than hand-built.

### 7.3 No linting or formatting configuration exists (High)

*[also S, U-5]*

There are **zero** ESLint, Prettier, `rustfmt.toml` and `clippy.toml` files, and no `lint` script in `package.json`. Linting has never run in this project.

This is not a style complaint. It is the direct explanation for a specific cluster of findings in Section 6: `eslint-plugin-react-hooks` would have flagged the missing `useEffect` dependencies in 6.7 and `Search.tsx:82-94`, the exhaustive-deps rule plus a components-in-render rule would have flagged the remount bug in 6.4, `jsx-a11y` would have flagged most of 6.5 mechanically, and `no-unused-vars` at module scope would have surfaced the three dead files in 6.6. S makes the same point from the other direction: the frontend has strong *type* discipline (`strict`, zero `@ts-ignore`) and no *lint* discipline, and the defects that survived are precisely the ones types cannot catch.

**Fix:** add ESLint 9 flat config with `react-hooks`, `jsx-a11y` and `@typescript-eslint`, add a `lint` script, add `rustfmt.toml` and `clippy.toml`, and wire all of it into 7.1. Expect a large first-pass backlog.

### 7.4 Test coverage is thin and structurally misplaced (Medium)

*[also S, U-5]. The frontend-test count differs between the three reviews; see Section 10, item 6.*

There are **8** Rust unit tests across **7** `#[cfg(test)]` modules (security, media_utils, clip, object_detection, sync_manifest, raw_support, progress_stream). The crypto ones are genuinely valuable: `security/mod.rs:577-594` covers recovery-key verification round-trip and asserts that a wrong passphrase fails.

On the frontend, both M and U report zero tests. That was true when they were written and is **no longer true at HEAD**: PR #2 (merged, `378707a`) added Vitest and 18 tests for the pure format helpers now centralized in `src/lib/format.ts`, with `npm test` wired up. S is the accurate one here. Nothing runs any of it automatically, because there is still no CI (7.1).

The coverage still does not point at the risk. There is **no test that**: encrypts a file and then decrypts it byte-for-byte; detects a **truncated** `.wbenc` file (1.4, which is exactly why truncation is undetectable); rejects a tampered chunk; runs the 19-migration chain and asserts the resulting schema (2.4); or round-trips a database backup through the documented recovery procedure, which is the test that would have caught 2.1 the day it was written.

S's suggested next targets are the right ones, and they are all pure functions currently buried in components: `parseDateTakenToTimestamp` and the day/month/year grouping helpers (`MediaGrid.tsx:40-88`), the `buildDisplayRows` grouping algorithm and `findRowIndexAtOffset` binary search (`MediaGrid.tsx:198-242`, `796-837`), search-history helpers (`Search.tsx:23-37`), `createFilters` (`Search.tsx:114-121`), and `toErrorMessage` (`Onboarding.tsx:79-87`). Then the extracted `usePaginatedMedia` hook against a mocked fetcher, asserting dedupe, `hasNextPage=false` on an empty page, and the race guard from 6.10.

### 7.5 Committed debris, a dead 1.2 MB model, and two lockfiles (Medium)

*[also S, U-5]*

Tracked in git and serving no purpose: `src-tauri/2` (140 bytes, captured npm output from a `2>&1` typo), `src-tauri/build_log.txt` (106 bytes), `src-tauri/output.txt` (84 bytes). Each is a placeholder note explaining why it should not exist.

More significantly, **both ONNX models are committed but only one is used**: `src-tauri/src/ai/version-RFB-320.onnx` (1,244 KB, never referenced) and `version-RFB-320_simplified.onnx` (1,088 KB, `include_bytes!` at `ai/mod.rs:12`). They are the two largest tracked files in the repository. See Section 10, item 5, for the disagreement with U on whether the *used* model should also be moved out.

**Two lockfiles are committed**: `package-lock.json` (204 KB) and `pnpm-lock.yaml` (132 KB), while the project uses npm. They will drift, and contributors will resolve dependencies differently depending on which package manager they reach for.

Also missing: **no `LICENSE`** and **no `SECURITY.md`**. For a security-sensitive application distributing binaries, the absence of a `SECURITY.md` means a researcher who finds something has no disclosure channel, and the absence of a license means nobody can legally fork or contribute.

### 7.6 Version and release metadata (Medium)

*[also U-5]*

All three manifests declare version `0.0.0`, and `package.json` still declares `"name": "tauri-app"`. `Cargo.toml` declares `name = "tauri-app"`, `description = "A Tauri App"`, `authors = ["you"]`. There is no `[profile.release]` section at all, so the release build gets no LTO, no `strip`, and default codegen units; for a Rust binary shipping ONNX Runtime, `lto = true` and `strip = true` are typically worth several MB and a measurable startup improvement.

A version of `0.0.0` also means the updater in 7.2, once added, has no meaningful version to compare against, and users cannot report which build they are on.

### 7.7 No error tracking, and 876 kB in a single chunk (Medium / Low)

There is no Sentry or equivalent on either side. Observability is 50 `println!` calls in Rust that the Windows release subsystem discards, plus 77 `console.*` calls in the frontend that nobody can see in a packaged desktop app. `componentDidCatch` only calls `console.error` (6.7), so a crash in a shipped build is unreportable, which combined with 7.2 means you cannot learn about a problem *or* fix it for users.

`vite build` emits a single **876.57 kB** JavaScript chunk (255.52 kB gzipped) with no code splitting. Four declared dependencies (`react-window`, `react-window-infinite-loader`, `react-virtualized-auto-sizer`, and their types) are **never imported**, yet `vite.config.ts:16-18` pre-bundles three of them in `optimizeDeps`, because `MediaGrid` hand-rolls its own virtualizer. U adds `motion`, `date-fns` and `@tauri-apps/plugin-fs` to the unused-dependency list. For a local desktop app the bundle size costs startup time rather than bandwidth, so this is Low, but deleting the unused deps and the stale `optimizeDeps` block is free.

---

## 8. Dependency supply chain

*The master covered npm and the git-pinned Telegram client but explicitly deferred `cargo audit` to CI. S ran it. This section merges both.*

### 8.1 npm: 7 vulnerabilities, all build tooling (Medium)

*[also S]*

`npm audit` reports **7 vulnerabilities: 5 High, 1 Moderate, 1 Low**, in `vite`, `rollup`, `postcss`, `picomatch`, `nanoid` (High), `yaml` (Moderate) and `@babel/core` (Low). All fixable with `npm audit fix`.

To be accurate about severity: **every one of these is build tooling, not shipped runtime code.** The Vite dev-server path-traversal and `server.fs.deny` bypass issues affect a developer running `npm run dev`, not an end user running the installer. They are still worth fixing, since a compromised dev machine is how supply-chain attacks on signed releases begin, but they are not user-facing and should not be reported as such.

### 8.2 cargo: 15 vulnerabilities and 8 unsound or yanked warnings (High)

*[new, from S]. The master did not run `cargo audit`, so this is the largest single gap the merge fills. Unlike the npm findings, several of these are in shipped runtime code that parses untrusted input.*

| Crate | Issue | Fix |
| --- | --- | --- |
| `quinn-proto` 0.11.13 | DoS (CVSS 8.7) and remote memory exhaustion (7.5) | >= 0.11.15 |
| `quick-xml` 0.38.4 | Quadratic parse and unbounded namespace allocation DoS (7.5 x2) | >= 0.41.0 |
| `rustls-webpki` 0.103.9 | Name-constraint bypasses and a CRL-parse panic | >= 0.103.13 |
| `tract-nnef` | Integer overflow leading to out-of-bounds read **on model load** | >= 0.21.16 |
| `bytes` | Integer overflow | >= 1.11.1 |
| `time` | Stack-exhaustion DoS | >= 0.3.47 |
| `tar` | Symlink chmod and PAX size issues (Medium) | >= 0.4.45 |
| `crossbeam-epoch` | Advisory | upgrade |

Unsound or yanked warnings: `event-listener`, `glib`, `memmap2`, `rand` (three versions), and yanked `core2`.

**`tract-nnef` and `quick-xml` are the priorities**, because both parse genuinely untrusted input in this app. `tract-nnef` parses the AI models downloaded from unverified mirrors (1.11), which turns "a hijacked mirror can substitute a model" into "a hijacked mirror can trigger an out-of-bounds read". `quick-xml` sits on the metadata parsing path. `rustls-webpki` matters because TLS enforcement is the only thing currently protecting the model downloads.

Most of these are transitive via Tauri and reqwest and move with a lockfile bump.

**Fix:** `cargo update` the fixable crates, re-run `cargo audit`, and add `cargo audit` to CI (7.1) so this stays clean.

### 8.3 The Telegram client is correctly pinned (informational)

*Master's finding; corrects U's characterization. See Section 10, item 4.*

U lists the git dependency on `grammers` under "dependency hygiene (LOW)". A git dependency often *is* a supply-chain risk, but not this one:

```toml
# src-tauri/Cargo.toml:25-26
grammers-client  = { git = "https://github.com/Lonami/grammers", rev = "b595a8c4fdfa5c3a8abcb5766c959ecfe30e9f6e", ... }
grammers-session = { git = "https://github.com/Lonami/grammers", rev = "b595a8c4fdfa5c3a8abcb5766c959ecfe30e9f6e", ... }
```

A `rev` pin, not a branch, and the same rev for both crates. That is the right way to do it, and it should not be "fixed".

The crypto crates are all current and none are RUSTSEC-flagged (`aes-gcm 0.10.3`, `argon2 0.5.3`, `rand 0.8.5`, `blake3 1.8.3`). There are no path dependencies outside the repository, no wildcard versions, and no vendored code.

### 8.4 Three copies of the `image` crate are compiled in (Low)

*[new, from U-5].* Versions `0.24`, `0.23.14`, and `0.25.9`, the last pulled in transitively by `tauri-plugin-mcp-bridge`. Deleting the MCP bridge (3.1) removes one of them for free. There is also a dead commented `# sqlite` dependency in `Cargo.toml`.

### 8.5 The committed ONNX models have no documented provenance (Low)

*[new, from U-2.5].* `version-RFB-320.onnx` and `version-RFB-320_simplified.onnx` are embedded via `include_bytes!` with no recorded source URL, upstream hash, or license. For a face-detection model shipped inside a security-sensitive binary, that should be documented alongside the checksum work in 1.11.

---

## 9. What is already good

A review that lists only defects gives a false picture, and here it would give a badly false one. Notably, **all three reviews independently reached the same positive verdict on the cryptographic core**, which is the strongest form of corroboration in this document. Several findings above are fixed by copying a pattern that already exists a few files away.

**The cryptography is correct, and each review checked specifically for the ways it usually is not.**

*Argon2id with strong, deliberate parameters.* 64 MiB memory cost, 3 iterations, 32-byte output, comfortably above the OWASP floor, with the algorithm and version pinned explicitly (`security/mod.rs:189-193`). No PBKDF2, no raw SHA or blake3 used as a KDF, no fast hash on any passphrase path.

*No nonce reuse anywhere.* Every nonce and salt comes fresh from `OsRng`, including on re-encryption paths and on the fixed-filename temp upload path. There is no constant nonce, no nonce derived from content or file ID, and no key-plus-nonce pair used twice on any code path any of the three reviews could find. Finding 1.6 is about entropy *width*, not reuse.

*Authenticated encryption everywhere, with no unauthenticated mode available.* AES-256-GCM is the only cipher in the codebase. No CBC, no CTR, no hand-rolled MAC. Tags are always verified, and failures map to opaque errors that leak no oracle detail (`security/mod.rs:431-433`).

*Chunk index bound into both the nonce and the AAD* (`security/mod.rs:343-348`), which defeats chunk reordering, duplication and mid-file deletion, the three attacks most often missed in hand-rolled chunked-AEAD formats. Streaming through `BufReader`/`BufWriter` also means multi-GB videos are never fully loaded into memory, and counter overflow is explicitly checked on both paths.

*A 160-bit recovery key from a CSPRNG* (`security/mod.rs:278-287`), hex-encoded and grouped for transcription. *`OsRng` exclusively* for all key material across all nine RNG call sites. *Per-wrap random 16-byte salts*, so the passphrase wrap and the recovery wrap of the same master key get different salts and nonces. *Strict length validation on every decoded field* before use. *Constant-time credential comparison* via `argon2::verify_password`. *No hardcoded secrets anywhere*, verified separately by all three reviews, including that the committed `2`, `build_log.txt`, `output.txt` and `.env.example` contain only placeholders.

**Encryption fails closed in the paths that check the key, and it does so thoughtfully.** Uploads are deferred rather than sent in the clear when the vault is locked (`upload_worker.rs:126-136`). And the watcher deliberately destroys a plaintext thumbnail rather than leaving it on disk:

```rust
// src-tauri/src/watcher.rs:248-252
} else {
    // Avoid leaving plaintext thumbnail when vault is locked.
    let _ = fs::remove_file(&thumb_path);
    thumbnail_path = None;
}
```

That is not the obvious thing to write. Someone thought about residue, which is what makes 1.2 frustrating rather than damning.

**The vault starts locked and never auto-unlocks.** No passphrase caching, no key on disk, no "remember me" (`lib.rs:931-938`). **Encryption downgrade is blocked in the backend**, not just the UI (`lib.rs:303-309`), and `initialize_encryption` refuses to clobber an existing bundle (`lib.rs:324-328`). **DPAPI is real, not aspirational**, correctly called with user scope and proper `LocalFree` cleanup, and its non-Windows stubs **fail closed** with no plaintext fallback.

**The generic `set_config` command refuses to touch security keys** (`lib.rs:1686-1690`), and all five security-relevant keys are correctly `security_`-prefixed. This is the mitigation that keeps 3.2 from also being a write primitive. (Finding 3.10 argues it should be an allowlist rather than a denylist, which is a hardening step, not a contradiction.)

**TLS is enforced via rustls** on every outbound request, including the model downloads, which is what keeps 1.11 at Medium rather than High.

**Parameter binding is the default, including the tricky case.** 87 of roughly 90 SQL statements bind everything, and the dynamic-arity `IN (...)` pattern is textbook-correct with `params_from_iter` (`database.rs:1262-1277`). No dynamic table or column names, no interpolated `ORDER BY` or `LIMIT`, and **no SQL outside `database.rs` at all**. Most query methods also clamp inputs defensively.

**The std connection mutex never crosses an `await`.** All 89 methods acquire and release inside synchronous bodies, and several commands explicitly `drop(db_guard)` before awaiting with a comment saying why. This is the single most important thing to get right with `Mutex<Connection>` in an async app.

**No arbitrary-file-delete command exists.** Every `remove_file` operates on a path read from the database or generated by the app, never on IPC input, and no `fs:allow-write*` or `fs:allow-remove` capability is granted. (S-H3 raises the indirect chain through `import_sync_manifest`; see Section 10, item 2.) **No shell injection either**: the three process spawns all pass arguments as arrays with no shell, and ffmpeg availability is probed first with graceful degradation. (Which binary gets found is a separate problem: 3.9.)

**`unsafe` is confined to two audited Windows FFI wrappers** (the DPAPI calls), a point S verified explicitly.

**A real versioned migration system**, keyed on `PRAGMA user_version`, with each step in its own transaction. Migrations 13, 14 and 16 correctly implement the full SQLite table-rebuild dance to repair foreign keys, and migration 16 detects and repairs a legacy schema shape by probing `pragma_table_info`. **Real transactions where they matter most**: `add_faces`, `add_tags`, `merge_persons` and `bulk_add_to_album` are all correctly atomic.

**Graceful degradation and cooperative cancellation.** The face detector is `Option`al and the app runs without it. Workers take a `CancellationToken` and check it each iteration. The upload worker honours Telegram's own `FLOOD_WAIT` hint rather than blindly retrying. `resolve_app_data_dir` has a real fallback path. `clip.rs` tries multiple model candidates and gives an actionable error naming the Settings screen when all fail. The database also recovers gracefully from a *poisoned* mutex in intent, though not in fact (4.1).

**The frontend's type discipline is genuinely strong for a project this young.** `strict`, `noUnusedLocals`, `noUnusedParameters` and `noFallthroughCasesInSwitch` are all on, and the codebase honours them: zero `@ts-ignore`, zero `@ts-expect-error`, zero `@ts-nocheck`, and two `as any` in 10,915 lines, one of which is the canonical documented Leaflet workaround. `npm run build` is `tsc && vite build`, so this is enforced at build time.

**`lib/api.ts` is a properly typed IPC boundary, and it is accurate.** Every wrapper declares a concrete return type and no `invoke` in it returns bare `any`. A mechanical diff of the frontend command names against the Rust `#[tauri::command]` attributes shows **every single frontend invoke resolves to a real backend command**, with no typos and no drift, and the only unwired backend command is `debug_reset_faces`.

**The onboarding recovery-key flow gets the hard parts right.** It forces verification before proceeding, and, the detail most implementations miss, it actively purges the secret from state once verified (`Onboarding.tsx:185-194`). It states the tradeoffs of each mode honestly, requires an explicit risk acknowledgement for unencrypted mode, and includes a genuinely helpful inline tutorial for obtaining Telegram API credentials. And it was verified three separate ways that **no secret is written to `localStorage`, `sessionStorage`, a log, or an error message anywhere in the application**.

**`withBusy` and `toErrorMessage` are the right small abstractions** (`Onboarding.tsx:70-87`): the `finally` in `withBusy` makes a stuck spinner structurally impossible, and `toErrorMessage` types its input as `unknown` rather than `any`. They belong in `lib/utils.ts`.

**`MediaGrid`'s hand-rolled virtualizer is competent**, with a `ResizeObserver` hook, a variable-height row model, a binary search for the row at a given scroll offset, and a configured overscan window. Finding 6.4 is about memoization around it, not about the virtualization itself.

**Pure formatting helpers are now centralized and tested.** `src/lib/format.ts` plus 18 Vitest tests landed in PR #2, removing a duplication S had flagged (path basename x5, byte/speed/ETA formatting) and establishing the frontend test harness the other two reviews reported as absent.

**Honest inline documentation of known limits.** `lib.rs:2452-2453` notes that semantic search needs a real index for large datasets. `database.rs:809-810` flags a suspected transaction interaction. The README does the same at the product level.

---

## 10. Where the three reviews disagree

Nine contradictions surfaced during the merge. Each was re-checked against the code at `378707a` rather than resolved by seniority.

| # | Subject | S says | U says | M says | Resolution |
| --- | --- | --- | --- | --- | --- |
| 1 | Poisoned DB mutex | "Graceful poisoned-mutex recovery" (listed as a strength) | silent | 4.1: maps poison to `Err`, permanently bricking all 89 methods | **M is correct.** `database.rs:138-148` returns `Err`; `std::sync::Mutex` poisoning is permanent. The doc comment and log message claim recovery, which is what S read. The comment is the bug. |
| 2 | Arbitrary file delete | H3: `remove_local_copy` / `permanent_delete` / `download_local_copy` are an arbitrary-delete primitive when chained with H1 | silent | 3.4: "there is no command that deletes a frontend-supplied path" | **Both, precisely.** No command takes a delete path over IPC (M is right about the direct surface). But `import_sync_manifest` can write an attacker-chosen `file_path` into the DB, and the delete paths then read it back (S is right about the chain). Merged as an extension of 3.4: confine delete paths to managed roots regardless. |
| 3 | IPC command count | not stated | 75 | 74 | **74.** Re-counted at HEAD: `grep -c '^#\[tauri::command\]' src-tauri/src/lib.rs` returns 74. |
| 4 | `grammers` git dependency | silent | listed under "dependency hygiene (LOW)" | 8.3: correctly pinned to an immutable rev, not a defect | **M is correct.** Both crates pin the same `rev`, which is the recommended practice. Not an action item. |
| 5 | Committed ONNX models | silent | 2.4 MB, both should move to LFS or a runtime download | 7.5: only the unused 1,244 KB copy is indefensible | **M's nuance, U's floor.** Committing the *used* model is defensible for a face detector that must work offline. Delete the unused one now; moving the used one is optional. Either way, document its provenance (8.5). |
| 6 | Frontend tests | Vitest + 18 tests (via PR #2) | 0 | 0 | **S is correct at HEAD.** PR #2 is merged; `src/lib/__tests__/format.test.ts` and `"test": "vitest run"` both exist at `378707a`. M and U were written against a tree without them. |
| 7 | `cargo audit` | ran it: 15 vulns + 8 warnings | not run | explicitly deferred to CI | **No conflict, a gap.** S's results are merged in as Section 8.2 and are the single largest addition from either shorter review. |
| 8 | Severity of `camera_make` SQL and the file-format integrity gap | SQL: Medium ("bounded"). Format: Low | SQL: Medium. Format: High | SQL: High. Format: High | **Split.** On SQL, M's *analysis* is the accurate one (no arbitrary-statement injection is constructible, LIKE-pattern injection is live) but S and U's Medium rating fits that analysis better than M's High; carried at High to preserve the master's index, with the honest exploitability assessment inline. On the file format, S's Low understates it: undetectable truncation of a backup is a data-integrity failure, so High stands. |
| 9 | AI model download verification | M3: Medium, no checksum | 2.5: Medium, no checksum | out of scope, not assessed | **S and U are correct.** Merged as new finding 1.11. Two independent passes found it and the master declared the models out of scope, so there is no contradiction, only a gap. |

Two structural observations worth recording alongside these.

**Depth found the Criticals.** All four Critical findings (1.1, 2.1, 3.1, 3.2) come from the master, and three of the four appear in neither shorter review. They share a shape: each requires following a value across two or more files (a config row read in one worker and written in another; a key wrapped in one module and backed up in another; a command handler traced into a `HashMap` return). Neither shorter pass traced those chains.

**Breadth found the outliers.** The findings unique to S and U are not deeper, they are in files the master never opened: `cache.rs` (2.8, a High data-loss bug), `media_utils.rs`'s ffmpeg resolution (3.9), `Tags.tsx` (6.8), the upload-queue UI (6.9), and the `cargo audit` output (8.2). This is a good argument for running independent passes rather than one deeper pass.

---

## 11. Consolidated remediation plan

The master's staging, with the merged-in findings slotted by risk. Ordered by risk reduction per unit of effort, not by section number. Two dependencies worth noting: **2.1 must ship before you tell anyone the backup works**, and **7.1 (CI) is what stops the rest of this list from silently regressing**.

### Stage 0: stop the bleeding (hours, ship immediately)

| # | Change | Finding | Source |
|---|---|---|---|
| 1 | Gate `tauri_plugin_mcp_bridge::init()` behind `#[cfg(debug_assertions)]`, or delete it. Read the plugin's source and confirm what it binds | 3.1 | M, U |
| 2 | Export the `SecurityBundle` unencrypted alongside the encrypted backup (or as a plaintext header), so the passphrase and recovery key actually work | 2.1 | M |
| 3 | Tell existing encrypted-mode users to keep a copy of `library.db`, and treat prior "encrypted backup" guidance as withdrawn | 2.1 | M |
| 4 | Derive `should_encrypt` from `SecurityBundle.mode`, fail closed on read error, and assert `WBENC1` on the artifact before upload | 1.1 | M |
| 5 | **Stop the thumbnail-cache eviction listener from deleting DB-referenced files** (drop the listener, or null out `media.thumbnail_path` in the same operation) | **2.8** | **U** |
| 6 | Filter `security_*` keys out of `get_all_config` | 3.2 | M |
| 7 | Drop `'unsafe-eval'` and `'unsafe-inline'` from `script-src`; narrow `assetProtocol.scope` and the `fs` scopes off `**` and `C:\**` | 3.3 | M, S, U |
| 8 | Guard `import_files` sources against arbitrary paths; confine `export_media` and `backup_database` destinations; sanitize the EXIF-derived export folder name | 3.4 | M, S, U |
| 9 | Bound `ct_len` by the header's `chunk_size` | 3.6 | M |
| 10 | Fix the README download URLs and the in-app About link; bump all three versions to `0.1.0` | 5.2, 7.6 | M |

Items 1, 2 and 5 are the three that change the risk profile of the product. Everything else in this stage is additive and low-risk.

### Stage 1: make regression impossible (days)

| # | Change | Finding | Source |
|---|---|---|---|
| 11 | Add `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo audit`, `npm ci && npm run build && npm test` | 7.1, 8.2 | M, S, U |
| 12 | Add ESLint 9 flat config with `react-hooks` and `jsx-a11y`, plus `rustfmt.toml` and `clippy.toml`; add a `lint` script | 7.3 | M, S, U |
| 13 | **`cargo update` the 15 flagged crates, prioritizing `tract-nnef` and `quick-xml`; re-run `cargo audit` clean**; `npm audit fix` | **8.2**, 8.1 | **S** |
| 14 | **Pin and SHA-256 verify every downloaded AI model before the first parse; pin immutable revisions** | **1.11** | **S, U** |
| 15 | Add `WBENC1` round-trip, truncation and tamper tests; add a 0-to-19 migration chain test; add a backup-and-restore test | 7.4 | M |
| 16 | Correct the two false README claims; document the `%TEMP%` residue, the `library.db` dependency, and the plaintext metadata index | 5.3, 5.4 | M |
| 17 | Add code signing and `tauri-plugin-updater` with a `pubkey`; pin `bundle.targets` to `["nsis"]`; add `release.yml` | 7.2 | M, U |
| 18 | Fix the `version` assignments in migrations 5, 7 through 13; guard migration 15 against wiping all persons | 2.4 | M |
| 19 | Delete one lockfile; `git rm` the 3 stray files and the unused 1.2 MB model; add `LICENSE` and `SECURITY.md`; gitignore `.env` | 7.5, 4.7 | M, S, U |

Item 18 belongs here rather than later because it is a latent hard-startup-failure waiting for the next migration anyone writes.

### Stage 2: correctness and durability (1 to 2 weeks)

| # | Change | Finding | Source |
|---|---|---|---|
| 20 | Purge `%TEMP%\wanderer-*` on `lock_encryption`, on exit, and on startup; prefer serving decrypted bytes from memory | 1.2 | M, U |
| 21 | v2 file format: authenticate the header plus `key_id`, add a terminator chunk, require the magic when the bundle says encrypted | 1.4 | M, S, U |
| 22 | Protect `session.db` with DPAPI-plus-entropy or the master key; move toward an OS keychain abstraction so it works off Windows | 1.3 | M, S, U |
| 23 | Add `zeroize`; make the master key a non-`Copy` `ZeroizeOnDrop` newtype | 1.5 | M, U |
| 24 | Move filesystem deletions out of the transaction in `empty_trash`; wrap `permanent_delete` | 2.2 | M, U |
| 25 | Set `journal_mode = WAL` and `busy_timeout`; use the online backup API instead of `fs::copy` | 2.3 | M |
| 26 | Convert `media_fts` to external-content FTS5 with triggers | 2.5 | M, U |
| 27 | Add the 7 missing indexes; switch to `prepare_cached` | 2.6 | M, U |
| 28 | Add `UNIQUE` on `upload_queue(file_path)`; make `toggle_favorite` a single `RETURNING`; add a stale-`uploading` reaper | 2.7 | M |
| 29 | Fix the poisoned-mutex handling with `into_inner()`; delete the per-face DEBUG `PRAGMA` block | 4.1 | M |
| 30 | Move ONNX inference, `hash_file_streaming` and the 39 `std::fs` calls off the async runtime; stop holding the Telegram client mutex across transfers | 4.2 | M, U |
| 31 | **Resolve ffmpeg by absolute path (bundled or pinned at startup) instead of from `PATH`** | **3.9** | **S** |
| 32 | Wire up `AppError` and remove the string-matching retry in `App.tsx` | 4.5, 6.3 | M, S, U |
| 33 | Verify and retry Telegram plaintext deletion during migration; record un-purged IDs | 3.8 | M |
| 34 | Bind `camera_make`; clamp the 2 unclamped paginations; fix the negative `limit as usize` cast | 3.5, 3.7 | M, S, U |
| 35 | Commit a generated `schema.sql` | 2.4 | M |
| 36 | **Invert `set_config` to an allowlist of settable keys** | **3.10** | **S** |
| 37 | Stop logging Telegram message text; downgrade path and ID logging to `debug` | 4.7 | S, U |

### Stage 3: quality and maintainability (ongoing)

| # | Change | Finding | Source |
|---|---|---|---|
| 38 | **Fix `Tags.tsx` to use `convertFileSrc` (likely broken on Windows today)** | **6.8** | **S** |
| 39 | **Fix the large-viewport infinite-scroll deadlock; stop `media-added` resetting pagination; unnest the Trash context menus; fix the >60s FLOOD_WAIT progress; add the missing `placeholder.jpg`** | **6.9** | **U** |
| 40 | Extract one `<EnableEncryption>` component with the verification gate, used by both Onboarding and Settings | 6.1 | M |
| 41 | Close the print window, handle the clipboard rejection, defer the blob revoke; mask the `apiHash` input | 6.2, 6.9 | M, U |
| 42 | Hoist the inline `ItemWrapper`s; `memo()` the `Cell`; `useCallback` the handlers; key by `item.id` | 6.4 | M, S |
| 43 | **Add a monotonic request guard to the shared fetch path (fixes Search, Tags, SmartAlbums and MediaViewer at once)** | **6.10** | **S** |
| 44 | Add `aria-label` to 26 icon buttons; make the grid keyboard-navigable; give `MediaViewer` a list and arrow keys | 6.5 | M, U |
| 45 | Delete `LoginView.tsx`, `Sidebar.tsx`, `ThemeSwitcher.tsx`; extract one `<TelegramLogin>` and one `usePaginatedMedia` hook plus a `MediaListView` shell | 6.6 | M, S, U |
| 46 | Apply the existing `map_media_row` at the other 17 sites (about 500 lines deleted, no behaviour change) | 4.6 | M |
| 47 | Move `ErrorBoundary` outermost in `main.tsx`; add a reload button; add per-view boundaries; report crashes | 6.7 | M, S, U |
| 48 | Fix the two broken Tauri listener cleanups; remove the prop-mirroring state; clear the scroll timeout; lift mutations out of `MediaGrid` | 6.7, 6.6 | M, S, U |
| 49 | Parse phashes once and bucket by prefix in `find_duplicates`; fix the 4 N+1 patterns | 4.3 | M, U |
| 50 | Decide the map tile question: vendor Leaflet assets and allow `https:` in `img-src`, or remove the map | 6.7 | M, U |
| 51 | Rotate the recovery key on use; add `change_passphrase`; hoist the 8-char check into a shared validator | 1.7, 1.8 | M |
| 52 | Widen the nonce to full width via a per-file `file_id` and derived subkey | 1.6 | M, U |
| 53 | Delete the unused deps and the stale `optimizeDeps`; add code splitting | 7.7 | M, S, U |
| 54 | Add error reporting on both sides; replace `println!` with `log::`; audit for PII | 7.7, 4.7 | M, S, U |
| 55 | Fix `escape_like_pattern` (add `ESCAPE '\'`) or delete it with its dead caller; delete `Database::get_persons`; remove `debug_reset_faces` and the LLM-dialogue comments | 4.7 | M, U |
| 56 | Add `[profile.release]` with `lto` and `strip`; target-gate `windows-sys` | 7.6, 4.7 | M |
| 57 | Extend the Vitest suite to the grouping, search-history, filter and error-message helpers, then to `usePaginatedMedia` | 7.4 | S |
| 58 | Split `database.rs` and `lib.rs` along domain lines; split `Settings.tsx` into per-tab components | 4.6, 6.6 | M, S, U |

---

## Appendix A: cross-reference index

Every finding in all three source documents, mapped. `-` means the document does not contain the finding.

| Merged ID | Severity | Finding | S | U |
|---|---|---|---|---|
| 1.1 | Critical | Encryption enforcement reads a duplicated flag and fails open to plaintext upload | - | - |
| 1.2 | High | Decrypted plaintext accumulates in `%TEMP%` and survives `lock_encryption` | - | 2.4 |
| 1.3 | High | `session.db` is a full Telegram account credential stored unprotected | H4 | 2.1 |
| 1.4 | High | The file format authenticates chunks but not the file | L3 | 2.2 |
| 1.5 | High | No zeroization; master key is `Copy` and outlives lock in a spawned task | - | 2.6 |
| 1.6 | Medium | Nonce carries 64 bits of entropy, not 96; key never rotated | - | 2.3 |
| 1.7 | Medium | A used recovery key is never invalidated; no change-passphrase command | - | - |
| 1.8 | Medium | Passphrase policy, trim inconsistency, no unlock throttling, no check on reset | - | - |
| 1.9 | Medium | DPAPI called without secondary entropy | - | - |
| 1.10 | Medium | Metadata, including GPS, is never encrypted | - | - |
| **1.11** | Medium | **AI models fetched from unverified mirrors with no integrity check** | **M3** | **2.5** |
| 2.1 | Critical | The encrypted backup is mathematically undecryptable | - | - |
| 2.2 | High | Filesystem deletions inside a transaction that can roll back | - | 3.3 |
| 2.3 | High | No WAL or busy timeout; backup is a raw `fs::copy` of a live DB | - | - |
| 2.4 | High | Stale migration `version`, two destructive steps, no committed schema | - | 3.3 (partial) |
| 2.5 | High | The full-text index is insert-only | - | 3.4 |
| 2.6 | Medium | Missing indexes on 7 hot columns; zero `prepare_cached` | - | 3.4 |
| 2.7 | Medium | Non-atomic read-modify-write; no `UNIQUE` on `upload_queue` | - | - |
| **2.8** | **High** | **Thumbnail cache eviction deletes live, DB-referenced thumbnails** | - | **3.1** |
| 3.1 | Critical | MCP bridge registered unconditionally in release builds | - | 1.1 |
| 3.2 | Critical | `get_all_config` hands the wrapped master key to the webview | - | - |
| 3.3 | High | CSP allows inline script and eval; asset and `fs` scopes cover the whole disk | C1, C2 | 1 |
| 3.4 | High | `import_files` is an arbitrary read that auto-uploads to Telegram | H1, H2, H3 | 3.5 |
| 3.5 | High | `camera_make` interpolated into the WHERE clause | M1 | 3.4 |
| 3.6 | Medium | Unbounded allocation from an attacker-controlled chunk length | - | - |
| 3.7 | Medium | Unclamped pagination; negative `limit as usize` becomes `usize::MAX` | - | 3.4 (partial) |
| 3.8 | Medium | Migration leaves plaintext in Telegram and reports success anyway | - | - |
| **3.9** | Medium | **FFmpeg resolved from `PATH`** | **M2** | - |
| **3.10** | Low | **`set_config` is a denylist, allowing arbitrary non-security key writes** | **L1** | - |
| 4.1 | High | A single panic permanently poisons the DB mutex | conflict | - |
| 4.2 | High | Global lock held across ONNX inference and blocking `std::fs` | - | 3.2 |
| 4.3 | High | Duplicate detection is O(n squared) under the global lock | - | 3.4 |
| 4.4 | Medium | 80 discarded results, several hiding state corruption | - | - |
| 4.5 | Medium | `errors.rs` is dead code; all 74 commands return `Result<T, String>` | - | - |
| 4.6 | Medium | `database.rs`: 3,264 lines, 17 copy-pasted row mappings, unused helper | - | - |
| 4.7 | Low | Panics on external input, dead `escape_like_pattern`, 50 `println!`, message-text logging, dead `.env`, `windows-sys` not gated, LLM comments, `debug_reset_faces` | L2, M4, H4 | 2.6, 3.5 |
| 5.1 | - | All six README security claims verify | - | (agrees) |
| 5.2 | High | README download links point at a different GitHub owner | - | - |
| 5.3 | Medium | `npm run build` documented as the production build | - | - |
| 5.4 | Medium | Four README claims true but materially incomplete | - | - |
| 6.1 | High | Settings encryption path has no verification or save affordance | - | - |
| 6.2 | High | Recovery-key print window never closes and fails silently | - | - |
| 6.3 | High | Startup retries forever by string-matching a Rust error message | F3 | 4.2 |
| 6.4 | High | Inline `ItemWrapper`s remount every grid cell; index-based keys | F5 | - |
| 6.5 | High | Zero `aria-label`; the photo grid is unreachable by keyboard | - | 4.3 |
| 6.6 | Medium | Three login implementations, three dead files, pagination copied 7 times | (yes) | 4.3 |
| 6.7 | Medium | Error boundary not outermost; broken listener cleanups; swallowed errors; map blocked by CSP; dead UI | F1, F3, F6, F7 | 4.1, 4.2, 4.3 |
| **6.8** | Medium | **`Tags.tsx` hand-builds an `asset://` URL, broken on Windows** | **F4** | - |
| **6.9** | High/Med | **Infinite-scroll deadlock, `media-added` pagination reset, nested Trash menus, >60s FLOOD_WAIT progress, missing `placeholder.jpg`, unmasked `apiHash`** | - | **4.1, 4.3** |
| **6.10** | Medium | **No request sequencing anywhere: stale responses overwrite fresh ones in 4 views** | **F2** | - |
| 7.1 | High | No CI of any kind | (yes) | 5 |
| 7.2 | High | Unsigned installer, no updater | - | 5 |
| 7.3 | High | No ESLint, Prettier, rustfmt or clippy configuration | (yes) | 5 |
| 7.4 | Medium | Test coverage thin and structurally misplaced | (yes) | 5 |
| 7.5 | Medium | Committed debris, unused 1.2 MB model, two lockfiles, no LICENSE/SECURITY.md | (yes) | 5 |
| 7.6 | Medium | Version `0.0.0`, `name: "tauri-app"`, no `[profile.release]` | - | 5 |
| 7.7 | Medium | No error tracking; 876 kB single chunk; unused deps pre-bundled | (yes) | 4.3, 5 |
| 8.1 | Medium | npm: 7 vulnerabilities, all build tooling | (yes) | - |
| **8.2** | **High** | **cargo: 15 vulnerabilities + 8 unsound/yanked warnings** | **(yes)** | - |
| 8.3 | - | `grammers` correctly pinned to an immutable rev | - | conflict |
| **8.4** | Low | **Three copies of the `image` crate compiled in** | - | **5** |
| **8.5** | Low | **Committed ONNX models have no documented provenance** | - | **2.5** |

**Bold rows are the 20 findings that entered this document from S or U rather than from the master.**

---

## Appendix B: finding index by severity

**Critical (4).** All four originate in the master.

| ID | Finding |
|---|---|
| 2.1 | The encrypted database backup and the entire Telegram archive are undecryptable if `library.db` is lost |
| 1.1 | Encryption enforcement reads a duplicated `security_mode` row and fails open to plaintext upload |
| 3.1 | `tauri-plugin-mcp-bridge` is registered unconditionally in release builds, in a process holding the decrypted master key |
| 3.2 | `get_all_config` hands the wrapped master key and the DPAPI credential blob to the webview |

**High (26).** Two of these (2.8, 8.2) are contributed by the merge.

| ID | Finding |
|---|---|
| 1.2 | Decrypted plaintext accumulates in `%TEMP%` forever and survives `lock_encryption` |
| 1.3 | `session.db`, a full Telegram account credential, is stored with no protection |
| 1.4 | The file format authenticates chunks but not the file |
| 1.5 | No zeroization; the master key is `Copy` and is moved into a spawned task that outlives lock |
| 2.2 | Filesystem deletions happen inside a transaction that can roll back |
| 2.3 | No WAL or busy timeout; the backup is a raw `fs::copy` of a live database |
| 2.4 | Migration `version` not updated in 8 steps; migration 15 can delete every named person; no committed schema |
| 2.5 | The full-text index is insert-only, never deleted from, and never populated by the sync path |
| **2.8** | **The thumbnail cache eviction listener deletes live, DB-referenced thumbnails** |
| 3.3 | CSP allows inline script and eval; asset and `fs` scopes cover the whole filesystem |
| 3.4 | `import_files` is an arbitrary file read that auto-uploads the file to Telegram |
| 3.5 | `camera_make` is string-interpolated into the WHERE clause |
| 4.1 | A single panic permanently poisons the DB mutex |
| 4.2 | The global lock is held across ONNX inference and 39 blocking `std::fs` calls |
| 4.3 | Duplicate detection is O(n squared) with 2 allocations per comparison, under the global lock |
| 5.2 | README download links point at a different GitHub owner, for an unsigned installer |
| 6.1 | The Settings path to enable encryption silently destroys recoverability |
| 6.2 | Recovery-key print window never closes and fails silently |
| 6.3 | Startup retries forever by string-matching a Rust error message |
| 6.4 | Inline `ItemWrapper` components remount every visible grid cell |
| 6.5 | Zero `aria-label` in app code; the photo grid is unreachable by keyboard |
| **6.9a** | **Infinite scroll can deadlock on large viewports, stranding the user at 20 items** |
| 7.1 | No CI: nothing builds, formats, lints, tests or type-checks on push |
| 7.2 | The installer is unsigned and there is no updater |
| 7.3 | No ESLint, Prettier, rustfmt or clippy configuration exists |
| **8.2** | **15 `cargo audit` vulnerabilities, including OOB read in the model parser and DoS in the XML and QUIC stacks** |

**Medium (26).** Merge contributions: 1.11, 3.9, 6.8, 6.10, and the remainder of 6.9.

1.6, 1.7, 1.8, 1.9, 1.10, **1.11**, 2.6, 2.7, 3.6, 3.7, 3.8, **3.9**, 4.4, 4.5, 4.6, 5.3, 5.4, 6.6, 6.7, **6.8**, **6.9b-e**, **6.10**, 7.4, 7.5, 7.6, 7.7, 8.1

**Low (13).** Merge contributions: 3.10, 8.4, 8.5.

4.7 (nine sub-items), **3.10**, 6.7 (cosmetics), **8.4**, **8.5**

---

## Appendix C: reproducing the measurements

Every number in this document came from a command run against a clean checkout. The master's original appendix is reproduced below, with the merge's additional verification commands appended.

**Toolchain results (executed)**

```bash
npm ci
npx tsc --noEmit          # 0 errors, exit 0, strict: true
npx vite build            # succeeds; dist/assets/index-*.js = 876.57 kB (gzip 255.52 kB)
npm audit                 # 7 vulnerabilities: 5 high, 1 moderate, 1 low
cd src-tauri && cargo fmt --check   # drift in 7 files
cd src-tauri && cargo audit         # 15 vulnerabilities, 8 unsound/yanked warnings  [from S]
# NOT run: cargo build, cargo test, cargo clippy
```

**Size and shape**

```bash
find src-tauri/src -name '*.rs' | wc -l                      # 21 files, 10,752 LOC
find src -name '*.ts*' | wc -l                               # 62 files, 10,915 LOC
grep -c '^#\[tauri::command\]' src-tauri/src/lib.rs          # 74  (not 75)
rg -c '^\s{4}pub fn ' src-tauri/src/database.rs              # 89 public methods
```

**Rust counts**

```bash
rg -n '\.unwrap\(\)' src-tauri/src/                          # 7 non-test
rg -o 'let _ = ' src-tauri/src/ --no-filename | wc -l        # 80
rg -o '\.ok\(\)' src-tauri/src/ --no-filename | wc -l        # 32
rg -o 'println!' src-tauri/src/ --no-filename | wc -l        # 50
grep -rn '#\[test\]' src-tauri/src | wc -l                   # 8 in 7 modules
rg -n 'PRAGMA user_version =' src-tauri/src/database.rs | wc -l   # 19 migrations
rg -c 'CREATE INDEX' src-tauri/src/database.rs               # 7 (2 dead)
rg -o 'conn\.prepare\(' src-tauri/src/database.rs | wc -l    # 41, and 0 prepare_cached
rg -c 'file_hash: row.get\(2\)\?' src-tauri/src/database.rs  # 17 inline row mappings
rg -c 'std::fs::' src-tauri/src/lib.rs                       # 39
rg -n 'journal_mode|busy_timeout|synchronous|WAL' src-tauri/src/   # no matches
rg -n 'execute\(|query_row|prepare\(' src-tauri/src/ --glob '!database.rs'   # no matches
```

**Frontend counts**

```bash
grep -rn "as any" src --include=*.ts --include=*.tsx | wc -l          # 2
grep -rn "@ts-ignore\|@ts-expect-error\|@ts-nocheck" src | wc -l      # 0
grep -rn "console\." src --include=*.ts --include=*.tsx | wc -l       # 77
grep -rn "aria-label" src --include=*.tsx | grep -v "^src/components/ui/" | wc -l   # 0
grep -rn 'size="icon"' src --include=*.tsx | grep -v "^src/components/ui/" | wc -l  # 26
grep -rn "memo(" src --include=*.tsx | grep -v "^src/components/ui/" | wc -l        # 0
```

**Verification performed during this merge (against `378707a`)**

```bash
# 2.8: the eviction listener still deletes evicted thumbnails
sed -n '1,30p' src-tauri/src/cache.rs

# 3.1: the MCP bridge is still registered with no cfg gate
grep -n "mcp_bridge" src-tauri/src/lib.rs src-tauri/Cargo.toml
#   src-tauri/src/lib.rs:863  .plugin(tauri_plugin_mcp_bridge::init())
#   src-tauri/Cargo.toml:43   tauri-plugin-mcp-bridge = "0.7.0"

# 6.8: Tags.tsx still hand-builds the asset URL
grep -n "asset://localhost\|convertFileSrc" src/components/Tags.tsx
#   src/components/Tags.tsx:86  `asset://localhost/${encodeURIComponent(...)}`

# 3.9: ffmpeg is still invoked by bare name
grep -n '"ffmpeg"' src-tauri/src/media_utils.rs        # lines 177, 183, 212

# Section 10 item 3: the command count is 74, not 75
grep -c '^#\[tauri::command\]' src-tauri/src/lib.rs    # 74

# Section 10 item 6: frontend tests DO exist at HEAD
ls src/lib/__tests__                                    # format.test.ts
grep -n '"test"' package.json                           # "test": "vitest run"

# 7.1: still no CI
ls -a .github                                           # No such file or directory
```

---

*Merged from `CODE_REVIEW_NORMAL_UNSPECIFIED.md` (master), `CODE_REVIEW_FINDINGS_HIGH_SPECIFIED.md` and `CODE_REVIEW_FINDINGS_HIGH_UNSPECIFIED.md`. The three source documents are retained unchanged for provenance; this document supersedes them.*
