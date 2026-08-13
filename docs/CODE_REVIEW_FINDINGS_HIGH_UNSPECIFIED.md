# Code Review Findings: Wanderer (Tauri 2 media manager)

**Date:** 2026-08-13
**Reviewer:** [code]smith, a cloud coding agent from Blacksmith (https://www.blacksmith.sh). This review was produced autonomously by reading every layer of the repository: the Rust core (`lib.rs`, `database.rs`, workers), the security/crypto module, the Telegram and AI integrations, the React/TypeScript frontend, and the Tauri configuration and build tooling. Every finding cites the file and line range where it was verified; nothing is speculative.

**Scope:** Full-repo deep dive covering the Tauri security posture, cryptography, the Rust command surface and database, backend correctness/performance, the frontend and IPC layer, and repo hygiene.

**Severity legend:** CRITICAL = exploitable now with direct data-loss, key-exposure, or full-machine-access impact. HIGH = serious security or correctness defect. MEDIUM = quality/robustness issue that will bite under real libraries or change. LOW = hygiene.

---

## Executive summary

Wanderer is a Tauri 2 desktop media manager (photos/videos, albums, people via on-device face detection, maps, tags, trash, duplicate review) with optional end-to-end-encrypted backup to a user's own Telegram account and on-device AI (CLIP semantic search, face/object detection via ONNX). The cryptographic primitives are genuinely well built: Argon2id-wrapped random master key, AES-256-GCM chunked file format, a recovery-key flow, fail-closed locking, and unit tests. SQL is almost entirely parameterized, TypeScript strict mode is on and gates the build, and no secrets are hardcoded or committed.

The dominant risk is **configuration, not algorithms**. The app collapses its own security model with a webview that can read the entire filesystem (`asset://` scope `**` plus `fs:allow-read` on `C:\**`), a CSP that permits `unsafe-eval`/`unsafe-inline`, and an always-on IPC-inspection bridge (`tauri-plugin-mcp-bridge`) shipped in release builds, all against an unscoped surface of 75 commands including `unlock_encryption`, `set_telegram_api_credentials`, and `permanent_delete_media`. On top of that, the Telegram MTProto session (full account access) is stored as a plaintext SQLite file, a thumbnail cache eviction listener silently deletes on-disk thumbnails still referenced by the database, and the encrypted file format has no whole-file integrity so backups can be truncated or spliced undetectably. Sections 1-3 should be addressed before promoting this beyond an early Windows release.

---

## 1. Tauri security posture (CRITICAL)

These four settings compound into a single failure mode: any script that runs in the webview (an XSS, or a compromised npm dependency, none currently present but nothing prevents one) gains full-disk read plus the entire privileged command surface.

- **Asset protocol scoped to the whole filesystem.** `src-tauri/tauri.conf.json:26-35`: `assetProtocol.scope: ["**", "C:\\**", "C:/**", ...]`. `"**"` makes every file on every drive fetchable via `asset://`. For an app whose selling point is encrypted-at-rest media, this means a webview compromise is a full-disk read, including `session.db` and `library.db`.
- **CSP permits `unsafe-eval` and `unsafe-inline` scripts.** `tauri.conf.json:25`: `script-src 'self' 'unsafe-eval' 'unsafe-inline'`. This neutralizes CSP as an XSS mitigation. (No `dangerouslySetInnerHTML`/`innerHTML` exists in `src/` today, so no concrete XSS vector was found, but there is zero defense in depth.)
- **`fs` plugin read access to all of `C:\`.** `src-tauri/capabilities/default.json:27-40`: `fs:allow-read` and `fs:allow-exists` are granted for `C:\**`. The frontend can read/probe any file on the system drive directly through the fs plugin, independent of the asset protocol, even though `@tauri-apps/plugin-fs` is never imported in the frontend.
- **Every command is exposed to the single window with no per-command capability scoping.** All 75 registered commands (`lib.rs:1104-1195`) are reachable from the one `main` window.

Remediation: narrow both scopes to `$APPDATA/**` + `$LOCALAPPDATA/**` (the only dirs the app uses), remove `unsafe-eval`/`unsafe-inline` from `script-src`, and scope capabilities per window/command.

### 1.1 IPC-inspection bridge shipped in release (HIGH)

`src-tauri/src/lib.rs:863` registers `tauri_plugin_mcp_bridge::init()` unconditionally (dependency at `Cargo.toml:43`). Per crates.io this plugin "enables IPC monitoring and backend inspection" and runs a local WebSocket server (`tokio-tungstenite` in `Cargo.lock`). Shipped in production, it exposes the privileged command surface to any local process that can connect to the bridge. Gate it behind `#[cfg(debug_assertions)]` or a dev-only feature. (The pinned 0.7.0 is also well behind the current 0.12.0.)

---

## 2. Cryptography and secrets

### 2.1 Telegram MTProto session stored as plaintext SQLite (HIGH)

`src-tauri/src/telegram.rs:105-112`: the grammers session is opened as `SqliteSession` at `<app_data>/session.db`. That file contains the MTProto auth key, i.e. full access to the user's entire Telegram account (all chats, not just backups). The far less sensitive `api_id`/`api_hash` are DPAPI-protected (`lib.rs:412-443`), so the protection is inverted, and combined with the `C:\**` read scope (Section 1) the session is readable from the webview. DPAPI-wrap the session (or use encrypted session storage) to match the credential handling.

### 2.2 Encrypted format has no whole-file integrity (HIGH)

`src-tauri/src/security/mod.rs:410-443` (`decrypt_file`): chunks are individually AES-GCM authenticated with `aad = chunk_index` only; the decrypt loop simply `break`s on EOF. There is no authenticated total chunk count, file ID, or end-of-file marker, and the header (magic/version/chunk_size) is not bound as AAD. Consequences for whoever controls the stored blob (Telegram, or an account compromise):

- Truncating a backup at any 1 MiB chunk boundary still "decrypts successfully" (silent corruption of restores).
- Chunk *i* of file A can be swapped with chunk *i* of file B (same key, colliding AAD) undetected.
- Flipping the magic bytes makes `is_encrypted_file` return false, so `decrypt_file_if_needed` (`mod.rs:449-465`) copies raw ciphertext into the library as if it were plaintext media instead of failing.

Fix: authenticate total length / a per-file nonce / a final-chunk flag (a STREAM-style construction).

### 2.3 One global key with a 64-bit-random nonce prefix (MEDIUM)

Every file, thumbnail, and DB backup is encrypted directly with the single master key (no per-file subkey). `derive_chunk_nonce` (`mod.rs:289-293`) overwrites the last 4 bytes of a random 12-byte base nonce with the chunk counter, leaving only 64 bits of per-file randomness. Nonces are unique *within* a file and random *per* file (verified, no deterministic reuse), but by NIST SP 800-38D's 2⁻³² collision bound, 64 bits supports only ~90,000 encryptions per key before a (key, nonce) collision becomes likely, and a media library (each photo + each thumbnail + backups = one encryption each) can plausibly exceed that. A GCM nonce collision is catastrophic (keystream reuse + GHASH key recovery). Fix: derive a per-file subkey via HKDF from the master key plus a random file ID, or switch to XChaCha20-Poly1305 / AES-GCM-SIV.

### 2.4 Plaintext materialized to the OS temp dir and never purged on lock (MEDIUM)

Decrypted thumbnails (`lib.rs:166-190`, `wanderer-thumb-cache`) and decrypted full media for viewing (`lib.rs:2178-2205`, `wanderer-view-cache-materialized`) are written to `std::env::temp_dir()`. `lock_encryption` (`lib.rs:360-364`) only clears the in-memory key; it never deletes these plaintext copies, which survive after lock/exit. This defeats "encrypted at rest" for anything the user has viewed. The view-cache cleanup also runs only once, ~10s after boot (`lib.rs:1055-1090`), so it grows unbounded during a session.

### 2.5 AI models fetched from unverified mirrors, no integrity check (MEDIUM)

ArcFace is downloaded from a personal GitHub release and unofficial HF accounts (`ai/mod.rs:212-221`, validated only by size), MobileNet similarly (`object_detection.rs:311-314`), and CLIP with no size or hash check at all (`clip.rs:458-465`). A compromised mirror can silently swap ~350 MB of model content; `tract-onnx` is memory-safe so RCE is unlikely, but model substitution and parser DoS are possible. Pin SHA-256 hashes. The committed `version-RFB-320*.onnx` files (embedded via `include_bytes!`) also lack documented provenance.

### 2.6 Lower-severity crypto/secret notes (LOW)

- Master key and passphrases are not zeroized: `master_key` is a `Copy [u8;32]` freely copied into every worker (`upload_worker.rs:125`, `sync_worker.rs:68`, `watcher.rs:231`); no `zeroize` usage. Copies persist in freed memory / crash dumps.
- `.env` is not gitignored while `dotenvy::dotenv()` runs (`lib.rs:840`) and `.env.example` documents `TG_ID`/`TG_HASH`; one accidental `git add` from leaking credentials. (No `.env` is currently or historically tracked, verified.) The dotenvy path is also dead: nothing reads those vars.
- Private Telegram message text is logged: `telegram.rs:139-141` logs `message.text()` of every incoming message.
- DPAPI credential storage is Windows-only; `set_telegram_api_credentials` fails outright on macOS/Linux (`mod.rs:547-559`), fail-closed but unsupported.

**Verified-correct crypto:** Argon2id (64 MiB / t=3 / v0x13, 16-byte random salts) meets OWASP guidance; master key is `OsRng`-generated and never written in plaintext; key-wrap nonces/salts are random and single-use; separate PHC verifier for the recovery key; vault-locked states fail closed (uploads/sync/backup all defer); GCM tag failures abort with generic errors leaking no plaintext; encrypted→unencrypted downgrade is refused in place.

---

## 3. Rust core: correctness and data-loss

### 3.1 Thumbnail cache eviction deletes live thumbnails (HIGH, data loss)

`src-tauri/src/cache.rs:14-24` registers a moka `async_eviction_listener` that `fs::remove_file`s the evicted thumbnail, with capacity 2000 (`lib.rs:845`). `ThumbnailCache::insert` is called on every thumbnail generation (`watcher.rs:198/211`, `sync_worker.rs:284`), but the cache is never read anywhere except on insert. Once a library exceeds 2000 photos, moka evicts entries and the listener deletes the `.jpg` from disk while `media.thumbnail_path` still points at it (nothing updates the DB). The cache is effectively a file-deletion machine for any library larger than 2000 items.

### 3.2 Blocking CPU/I/O on the async runtime (HIGH, performance)

All DB access goes through a `std::sync::Mutex<Connection>` (`database.rs:133-148`) called from async commands, so every query blocks a runtime worker. Worse, several heavy operations run inline on the async runtime instead of `spawn_blocking`:

- `semantic_search` runs ONNX text inference while holding the DB mutex through the entire embedding scan/sort (`lib.rs:2450-2492`), serializing all other commands.
- `index_pending_clip` (`lib.rs:2529`) and `scan_duplicates`/`find_duplicates` (`lib.rs:1505-1513, 1562-1574`) decode images and run inference inline.
- `sync_worker` re-hashes files with `hash_file_streaming` (full read; videos can be 10 GB+) inline every 60s for the 20 newest Telegram messages (`sync_worker.rs:55, 88, 201`), and holds the Telegram `client` mutex across entire uploads/downloads (`telegram.rs:276-338, 431-460`), blocking sync/view/delete for the duration.

Thumbnailing already uses `spawn_blocking` (`media_utils.rs:85/175`); the same pattern should cover the above.

### 3.3 Non-transactional filesystem/DB deletes (MEDIUM)

`empty_trash` (`database.rs:2283-2323`) deletes local files inside the loop before `tx.commit()`; if the transaction fails, files are gone but rows remain. `permanent_delete` (`database.rs:2210-2259`) deletes files then the row with no transaction at all. A legacy migration (`database.rs:338-355`) also `DROP TABLE IF EXISTS config` for any DB at `user_version < 7`, losing all config on upgrade.

### 3.4 Query and index gaps (MEDIUM)

- `get_media_by_tag` (`database.rs:3074`) and `get_media_by_person` (`database.rs:2758`) do not clamp `limit`/`offset`; `limit = -1` means "no limit" in SQLite.
- No index on `media(file_path)` despite hot lookups and the FTS join; no index on `(is_deleted, is_archived)` or `date_taken`, so every timeline page does an unindexable full-sort on `ORDER BY COALESCE(date_taken, ...)`.
- `find_duplicates` is O(n²) pairwise Hamming over the whole library inside the connection lock (`database.rs:2653-2660`).
- FTS drifts from reality: insert errors are swallowed (`database.rs:1069`), nothing deletes/updates `media_fts` on permanent delete or path change, and search joins on `file_path`, so deleted files stay "searchable" and stale rows silently drop results.
- One instance of string-built SQL: `search_fts` interpolates `camera_make` with manual quote-doubling (`database.rs:1518-1537`); the rest of the file is parameterized and clamps ranges. Should be a bound parameter.

### 3.5 Lower-severity Rust notes (LOW)

- Panics on external input: `sync_worker.rs:148` (`to_str().unwrap()` on non-UTF-8 path), `progress_stream.rs:66` (`try_lock().unwrap()` in `poll_read`), `ai/worker.rs:48` (`build().unwrap()`).
- `export_media` builds export folders from unsanitized EXIF `date_taken` (`lib.rs:1396-1410`); `import_sync_manifest`/`backup_database`/`export_media` accept arbitrary frontend paths (redundant with Section 1 today, but would bypass any future scope tightening).
- Leftover LLM self-dialogue comments in `setup()` (`lib.rs:878-901`), a shipped `debug_reset_faces` command, extensive `println!` of user paths/IDs, weak device-ID entropy (`sync_manifest.rs:222-231`), and `import_files` deduping by filename only.

---

## 4. Frontend (React/TypeScript)

The frontend is a hand-rolled single-`view` router (`App.tsx`) over ~70 thin `invoke()` wrappers (`src/lib/api.ts`), no state library, with grid virtualization written by hand. `src/types.ts` mirrors the Rust structs accurately (spot-checked). Largest files: `Settings.tsx` (1302, five concerns in one component), `MediaGrid.tsx` (914), `AppSidebar.tsx` (747, two near-duplicate sidebar trees), `Onboarding.tsx` (701).

### 4.1 Functional bugs (HIGH/MEDIUM)

- **Map is broken under the enforced CSP and leaks location in a "privacy-first" app (HIGH).** `MapView.tsx:14-17` loads marker icons from `https://unpkg.com` and tiles from `https://{s}.tile.openstreetmap.org`, but the CSP `img-src` allows no `https:` host, so tiles/markers are blocked in packaged builds, and the requests contradict the "photos never leave your device" positioning.
- **Infinite scroll can deadlock on large viewports (HIGH).** Pagination is triggered only inside the scroll handler (`MediaGrid.tsx:839-845`); with a 20-item initial load (`Gallery.tsx:66`) on a wide/4K window, the content may not overflow, `onScroll` never fires, and `loadNextPage` is never called.
- **`media-added` silently resets pagination (MEDIUM).** `Gallery.tsx:75-80` replaces `items` with only the first 20 on every event, snapping a deep-scrolled user back to the top.
- **Trash exposes invalid actions via nested context menus (MEDIUM).** `Trash.tsx:39-50` wraps items in a restore-only menu, but `MediaGrid`'s cell always adds its own full menu (delete/album/archive), so the two Radix menus nest.
- **Stale-closure race + no request sequencing in Search (MEDIUM).** `Search.tsx:82-94` depends only on `[selectedTag]` while reading stale `query`/`hasSearched`; overlapping searches resolve out of order and clobber results; failures are swallowed to `console.error`.
- **Optimistic-update dual source of truth (MEDIUM).** `MediaGrid.tsx:648-753` mutates a local mirror that parent re-fetches overwrite, while parents that don't pass `onItemsChange` never learn about deletions.
- **Rate-limit progress assumes 60s (MEDIUM).** `UploadQueue.tsx:198` computes `(1 - countdown/60)*100`; Telegram FLOOD_WAITs over 60s produce negative progress.
- **Missing fallback asset.** `DuplicateReview.tsx:26,229` falls back to `/placeholder.jpg`, which does not exist in `public/`; the `onError` handler re-assigns the same missing URL.

### 4.2 Error handling and robustness (MEDIUM)

Single root `ErrorBoundary` (`App.tsx:81`) with no retry and no per-view boundaries, so one render crash bricks the app (and removes the `Toaster`, which lives inside it). Backend command errors are plain `String`s handled inconsistently: some toast raw error text, several have no `.catch` at all (`Gallery.tsx:77-79`, `MediaGrid.tsx:540`), and `App.tsx:49-50` retries `get_security_status` every 250ms forever with no cap or cleanup. Event-listener cleanup is racy in `Settings.tsx:125-133` (leaks if unmounted before the `listen` promise resolves).

### 4.3 Dead code, fake UI, hygiene (LOW)

Never-imported components `Sidebar.tsx`, `LoginView.tsx`, `ThemeSwitcher.tsx`; unused deps `react-window`, `react-window-infinite-loader`, `react-virtualized-auto-sizer`, `motion`, `date-fns`, `@tauri-apps/plugin-fs`; hardcoded decorative UI (fake tags, a fabricated `{user}@wander.app` email, non-functional toolbar buttons); a permanently disabled "Log Out (Not Implemented)" button beside a working one; the Telegram `apiHash` rendered in a plain (non-masked) text input in `Onboarding.tsx:620-627`; `alert`/`window.confirm` mixed with Tauri dialogs; debug `console.log`s left in (`main.tsx`, `MediaViewer.tsx:87/102`, `People.tsx:19-21`); accessibility is minimal (icon-only buttons unlabeled, grid cells are click-only `div`s with no keyboard path).

---

## 5. Tooling, build, and repo hygiene

- **No CI, no lint config, no JS tests (MEDIUM).** No `.github/` directory at all; no ESLint/Prettier/clippy/rustfmt config; `package.json` scripts are only `dev`/`build`/`preview`/`tauri`. Rust unit tests DO exist in 7 modules (security, media_utils, clip, object_detection, sync_manifest, raw_support, progress_stream) but nothing runs them automatically.
- **No code signing, no updater (MEDIUM).** `tauri.conf.json` bundle config has no signing or updater; the README distributes an unsigned `Wanderer._0.0.0_x64-setup.exe`, so users get SmartScreen warnings and no secure update path.
- **Dual lockfiles committed (MEDIUM).** Both `package-lock.json` and `pnpm-lock.yaml` are tracked while the project uses npm; the pnpm lockfile is stale/conflicting.
- **2.4 MB of ONNX models committed in `src/` (MEDIUM).** Two near-duplicate variants of the face-detection model (`version-RFB-320.onnx`, `version-RFB-320_simplified.onnx`) live in the Rust source tree; they belong in Git LFS, a release asset, or a runtime download (the app already downloads CLIP models on demand).
- **Stray junk files (MEDIUM).** `src-tauri/2` (captured npm output from a `2>&1` typo), `src-tauri/build_log.txt`, and `src-tauri/output.txt` are committed, each a placeholder note explaining why it shouldn't exist.
- **Placeholder identity (LOW).** `package.json` name `tauri-app` / version `0.0.0`; `Cargo.toml` `name = "tauri-app"`, `description = "A Tauri App"`, `authors = ["you"]`; `index.html` title `Tauri + React + Typescript`, stock favicon; no LICENSE despite a public releases page.
- **Dependency hygiene (LOW).** grammers pinned to a git rev; three copies of the `image` crate compiled in (`0.24`, `0.23.14`, and `0.25.9` via mcp-bridge); a dead commented `# sqlite` dep; a broken `// ts-expect-error` comment (plain `//`, no `@`) in `vite.config.ts:5`.

**Verified positives:** CSP is defined (not null); TypeScript strict mode is fully on and `tsc` gates the build; no hardcoded secrets anywhere (only user-supplied credential handling and empty `.env.example` keys); `fs` write access is NOT granted to the webview and window permissions are a tight explicit set; AI degrades gracefully instead of crashing when the face detector fails to init; Vite config follows Tauri best practice; the README is genuinely good end-user documentation, honest about the plaintext local `backup/` folder and the one-way encryption toggle.

---

## 6. Recommended remediation order

1. **Close the webview blast radius.** Narrow `assetProtocol.scope` and `fs:allow-read` from `**`/`C:\**` to `$APPDATA/**` + `$LOCALAPPDATA/**`; remove `unsafe-eval`/`unsafe-inline` from the CSP `script-src`; gate `tauri_plugin_mcp_bridge::init()` behind `#[cfg(debug_assertions)]`; scope capabilities per window/command.
2. **Protect the Telegram session** (DPAPI-wrap or encrypt `session.db`) and stop logging message text.
3. **Fix the data-loss bug**: stop the thumbnail-cache eviction listener from deleting DB-referenced files (or update the DB and use it as a read cache).
4. **Harden the encrypted format**: authenticate whole-file length / final-chunk marker and bind the header as AAD; move to per-file subkeys (HKDF) or a nonce-misuse-resistant AEAD; delete materialized plaintext on `lock_encryption`.
5. **Get heavy work off the async runtime** (`spawn_blocking` for ONNX inference, hashing, pHash; don't hold the DB or Telegram mutex across long operations); consider a connection pool.
6. **Make deletes transactional**, clamp all `limit`/`offset`, add the missing indexes, and keep the FTS table in sync.
7. **Frontend correctness**: fix the map CSP/remote-tile issue, the large-viewport infinite-scroll deadlock, the `media-added` pagination reset, and the nested Trash context menus; normalize IPC errors in `api.ts`; add per-view error boundaries.
8. **Ship-safety and hygiene**: add CI (`tsc`, `cargo test`, `cargo clippy`, ESLint) and a lint config; configure bundle signing and an updater; pick one lockfile; move the ONNX models out of source; delete the stray files and LLM-dialogue comments; set real app identity/version and add a LICENSE.
