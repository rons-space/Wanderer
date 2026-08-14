# Remediation Task List

Actionable backlog derived from [`CODE_REVIEW_MERGED.md`](CODE_REVIEW_MERGED.md), which supersedes
the three source reviews (`CODE_REVIEW_NORMAL_UNSPECIFIED.md`, `CODE_REVIEW_FINDINGS_HIGH_SPECIFIED.md`,
`CODE_REVIEW_FINDINGS_HIGH_UNSPECIFIED.md`). Task IDs `T1`-`T58` follow the merged document's
consolidated remediation plan (Section 11) so the two stay cross-referenceable; the parenthesised
`Finding` reference points at the section of the merged review that explains the defect and cites the
evidence.

**Severities:** Critical = permanent data loss, silent defeat of the security promise, or a
remote-control / key-extraction path. High = data loss or security degradation under realistic
conditions. Medium = correctness or reliability defect. Low = hygiene.

## How to work this list

- Stage order is risk-reduction order, not section order. Stage 0 before any further distribution.
- Two hard dependencies: **T2 (2.1) must ship before anyone is told the backup works**, and
  **T11 (CI) is what stops everything else on this list from silently regressing**.
- Branch per task (or per small cluster) off `dev`, PR into `dev`, per
  [`GIT_WORKFLOW.md`](GIT_WORKFLOW.md).
- There is no CI yet (T11). Until then run the narrowest local check that covers the edit:
  `npm run build`, `npm test`, `cargo test`, `cargo fmt --check`. Do not attempt a full `cargo build`.

## Counts

| Stage | Theme | Tasks | Highest severity |
| --- | --- | --- | --- |
| 0 | Stop the bleeding (hours) | 10 | Critical x4 |
| 1 | Make regression impossible (days) | 9 | High |
| 2 | Correctness and durability (1-2 weeks) | 18 | High |
| 3 | Quality and maintainability (ongoing) | 21 | High |

Findings by severity in the source review: **4 Critical, 26 High, 26 Medium, 13 Low**.

## Status snapshot

Re-verified against the current tip of `dev` while writing this list. Nothing from the review has
been remediated yet; the only change since the review is the branching workflow.

| Check | Result |
| --- | --- |
| `tauri_plugin_mcp_bridge::init()` gated | No, still unconditional at `src-tauri/src/lib.rs:863` |
| Thumbnail eviction listener deletes files | Yes, still at `src-tauri/src/cache.rs:12-24` |
| `Tags.tsx` hand-built `asset://` URL | Yes, still at `src/components/Tags.tsx:86` |
| CI workflow (build / lint / test) | None. `.github/workflows/` holds only `sync-dev-to-main.yml` |
| ESLint / Prettier / `rustfmt.toml` / `clippy.toml` | Absent |
| `LICENSE` / `SECURITY.md` | Absent |
| Lockfiles committed | Two (`package-lock.json`, `pnpm-lock.yaml`) |
| Stray files | `src-tauri/2`, `src-tauri/build_log.txt`, `src-tauri/output.txt` still tracked |

---

## Stage 0 — Stop the bleeding

Ship immediately. T1, T2 and T5 are the three that change the risk profile of the product; the rest
of this stage is additive and low-risk.

- [ ] **T1 — Gate or delete the MCP bridge plugin** (Finding 3.1, **Critical**)
  Read the plugin's source first and confirm what it binds and whether it authenticates.
  *Where:* `src-tauri/src/lib.rs:863`, `src-tauri/Cargo.toml:43`.
  *Done when:* the plugin is deleted, or registered only under `#[cfg(debug_assertions)]`, and a
  release build is confirmed to open no listener.

- [ ] **T2 — Make the encrypted backup decryptable** (Finding 2.1, **Critical**)
  The wrapped master key lives only inside `library.db`, and the backup of `library.db` is encrypted
  with that key, so the artifact seals in its own key material. Export the `SecurityBundle`
  unencrypted alongside the `.wbenc` artifact (it is already Argon2id-protected by the passphrase),
  or write it as a plaintext header.
  *Where:* `src-tauri/src/lib.rs:1901-1930`, `src-tauri/src/security/mod.rs:99-114`.
  *Done when:* a test restores a backup on a machine with no prior `library.db`, using only the
  passphrase, and separately using only the recovery key.

- [ ] **T3 — Withdraw the current backup guidance** (Finding 2.1, **Critical**)
  Tell existing encrypted-mode users to keep a copy of `library.db`, and treat prior "encrypted
  backup" guidance in the README and in-app copy as withdrawn until T2 ships.

- [ ] **T4 — Derive `should_encrypt` from the security bundle and fail closed** (Finding 1.1, **Critical**)
  `.ok().flatten().unwrap_or("unset")` on the duplicated `security_mode` row silently yields
  "not encrypted" on a read error or a missing row, and plaintext is uploaded while the UI reports
  the library as encrypted.
  *Where:* `upload_worker.rs:115-165`, `sync_worker.rs:60-66`, `watcher.rs:223-228`,
  `lib.rs:2110-2117`, `lib.rs:1907-1914`.
  *Done when:* every decision point reads `load_security_bundle()?.mode`, a read failure defers the
  upload instead of sending plaintext, and `FILE_MAGIC` (`WBENC1`) is asserted on the artifact
  immediately before `upload_file_with_progress`.

- [ ] **T5 — Stop the thumbnail cache from deleting live thumbnails** (Finding 2.8, **High, data loss**)
  The moka eviction listener unlinks the `.jpg` while `media.thumbnail_path` still points at it, at
  capacity 2000, and the cache is never read except on insert.
  *Where:* `src-tauri/src/cache.rs:12-24`, capacity at `lib.rs:845`, inserts at `watcher.rs:198,211`
  and `sync_worker.rs:284`.
  *Done when:* either the listener is gone, or eviction nulls `media.thumbnail_path` in the same
  operation and the cache is consulted on the thumbnail resolution path.

- [ ] **T6 — Filter `security_*` out of `get_all_config`** (Finding 3.2, **Critical**)
  The command returns the whole `config` table, including `security_bundle_v1` (Argon2 salts plus
  both wrapped master keys) and the DPAPI credential blob, to JavaScript on every `MediaGrid` mount.
  *Where:* `lib.rs:1677-1684`, `database.rs:2856-2858`; callers `src/lib/api.ts:211`,
  `Settings.tsx:164`, `MediaGrid.tsx:661`.
  *Done when:* no key with the `security_` prefix can reach the webview, mirroring the existing
  write-path guard at `lib.rs:1686-1690`.

- [ ] **T7 — Tighten CSP and filesystem scopes** (Finding 3.3, **High**)
  *Where:* `src-tauri/tauri.conf.json:25` (CSP), `:26-35` (`assetProtocol.scope`),
  `src-tauri/capabilities/default.json:13-40` (`fs:allow-read`, `fs:allow-exists`).
  *Done when:* `'unsafe-eval'` and `'unsafe-inline'` are gone from `script-src`, and `**` / `C:\**`
  are replaced with the app-data subdirectories actually served (thumbnails, view cache, backup).

- [ ] **T8 — Confine every command that takes a caller-supplied path** (Finding 3.4, **High**)
  *Where:* `import_files` (`lib.rs:1216-1244`), `import_sync_manifest`
  (`lib.rs:2325-2331` / `sync_manifest.rs:102-106`), `export_media` (`lib.rs:1371-1410`),
  `backup_database` (`lib.rs:1862-1902`), and the delete paths `remove_local_copy` /
  `permanent_delete` / `download_local_copy` (`lib.rs:1943-2065`, `database.rs:2210-2259`).
  *Done when:* import sources are limited to dialog-chosen paths, export and backup destinations are
  canonicalized against an allowed root, the sync manifest is version-checked and entry-bounded, the
  EXIF-derived export folder name is sanitized, and every unlink asserts the path is inside a managed
  root.

- [ ] **T9 — Bound `ct_len` by the header's `chunk_size`** (Finding 3.6, Medium)
  A corrupted or hostile blob can declare `0xFFFFFFFF` and force a ~4 GiB allocation per chunk before
  any tag is verified.
  *Where:* `src-tauri/src/security/mod.rs:417-423`; the validated bound is at `:392-394`.
  *Done when:* `ct_len` is rejected unless `16 <= ct_len <= chunk_size + 16`.

- [ ] **T10 — Fix the release links and set a real version** (Findings 5.2, 7.6, **High** documentation)
  Three different GitHub locations are referenced today: the repo (`rons-space/Wanderer`), the README
  download links (`ronimuliawan/Wanderer`, `README.md:34-36`) and the in-app About tab
  (`ronimuliawan/wanderbackup-rust`, `Settings.tsx:49-53`). For an unsigned installer this reads like
  a phishing instruction.
  *Done when:* all three point at the real repository and `package.json`, `tauri.conf.json` and
  `Cargo.toml` are bumped off `0.0.0` to `0.1.0`.

---

## Stage 1 — Make regression impossible

- [ ] **T11 — Add `.github/workflows/ci.yml`** (Findings 7.1, 8.2, **High**)
  `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo audit`,
  `npm ci && npm run build && npm test`, on pull requests to `dev` and `main`. Roughly 40 lines of
  YAML, and it is what converts the rest of this list from "will drift again" to "cannot drift again".

- [ ] **T12 — Add lint and format configuration** (Finding 7.3, **High**)
  ESLint 9 flat config with `react-hooks`, `jsx-a11y` and `@typescript-eslint`; a `lint` script;
  `rustfmt.toml` and `clippy.toml`. Expect a large first-pass backlog: `react-hooks` alone covers T43
  and parts of T48, and `jsx-a11y` mechanically covers most of T44.

- [ ] **T13 — Clear the dependency advisories** (Findings 8.2, 8.1, **High**)
  `cargo update` the 15 flagged crates and re-run `cargo audit` clean; `npm audit fix`.
  **Priorities: `tract-nnef` (integer overflow to OOB read on model load) and `quick-xml`**, both of
  which parse untrusted input here; then `quinn-proto`, `rustls-webpki`, `bytes`, `time`, `tar`,
  `crossbeam-epoch`. The npm advisories are all build tooling, not shipped runtime code.

- [ ] **T14 — Verify AI model downloads** (Finding 1.11, Medium)
  *Where:* ArcFace `ai/mod.rs:199-325` (size check only), MobileNet `object_detection.rs:311-314`,
  CLIP `clip.rs:444-508` (no check at all).
  *Done when:* every download has a pinned SHA-256 verified before the first parse, and revisions are
  pinned immutably rather than to moving tags.

- [ ] **T15 — Add the tests that would have caught the Criticals** (Finding 7.4, Medium)
  `WBENC1` encrypt/decrypt round-trip byte-for-byte; a truncated `.wbenc` must fail; a tampered chunk
  must fail; the 0-to-19 migration chain must produce the expected schema; a backup must restore
  through the documented recovery procedure (this is the test that catches T2).

- [ ] **T16 — Correct the README** (Findings 5.3, 5.4, Medium)
  `npm run build` is the frontend bundle, not the production build (`npm run tauri build` is).
  Document the `%TEMP%` plaintext residue (T20), the `library.db` dependency for recovery (T2), the
  plaintext metadata index including GPS (Finding 1.10), and that the 8-character minimum is not
  enforced on the reset path (T51).

- [ ] **T17 — Sign the installer and add an updater** (Finding 7.2, **High**)
  `tauri-plugin-updater` with a signing `pubkey`, Authenticode signing, `bundle.targets` pinned to
  `["nsis"]`, and a `release.yml` so releases are reproducible. Without this there is no channel to
  ship any of the fixes above to existing users.

- [ ] **T18 — Fix the migration chain** (Finding 2.4, **High**)
  Assign `version` in migrations 5 and 7-13 (`database.rs:317-321` and onwards); guard migration 15
  so it cannot delete every named person when no face embeddings exist (`database.rs:512-517`).
  Latent hard-startup-failure the next time anyone inserts a migration, hence this stage.

- [ ] **T19 — Repo hygiene** (Findings 7.5, 4.7, Medium)
  Delete one lockfile; `git rm` `src-tauri/2`, `src-tauri/build_log.txt`, `src-tauri/output.txt` and
  the unused 1,244 KB `version-RFB-320.onnx`; add `LICENSE` and `SECURITY.md`; gitignore `.env`.

---

## Stage 2 — Correctness and durability

- [ ] **T20 — Purge decrypted plaintext from `%TEMP%`** (Finding 1.2, **High**)
  `lock_encryption` (`lib.rs:360-364`) clears only the in-memory key; `wanderer-thumb-cache`,
  `wanderer-view-cache-materialized` and four sibling staging dirs are never cleaned.
  *Done when:* materialized paths are tracked and deleted on lock, on window close and on startup;
  prefer serving decrypted bytes from memory over writing plaintext at all.

- [ ] **T21 — v2 encrypted file format** (Finding 1.4, **High**)
  Bind `magic || version || chunk_size || base_nonce || key_id || total_chunks` into the AAD, add an
  explicit terminator chunk, and require the magic when the bundle says encrypted instead of sniffing
  six bytes.
  *Where:* `security/mod.rs:289-293, 328-348, 380-443`, `lib.rs:2150-2168`.

- [ ] **T22 — Protect `session.db`** (Finding 1.3, **High**)
  The MTProto auth key (full Telegram account access) is stored unprotected while the far less
  sensitive api_id/api_hash get DPAPI.
  *Where:* `telegram.rs:105-112`; credential handling at `lib.rs:412-443`.
  *Done when:* the session is DPAPI-wrapped (with secondary entropy, Finding 1.9) or master-key
  encrypted, behind a keychain abstraction that also works off Windows.

- [ ] **T23 — Zeroize key material** (Finding 1.5, **High**)
  Add `zeroize`; make the master key a non-`Copy` `ZeroizeOnDrop` newtype so every copy is explicit;
  take passphrases as `Zeroizing<String>`.
  *Where:* `security/mod.rs:81-86`, copies at `lib.rs:204-206, 496-531`, `upload_worker.rs:125`,
  `watcher.rs:231`, `sync_worker.rs:68,303`.

- [ ] **T24 — Move filesystem deletes out of transaction scope** (Finding 2.2, **High**)
  `empty_trash` unlinks before `tx.commit()` (`database.rs:2281-2295`), so a rollback leaves rows
  pointing at deleted bytes; `permanent_delete` (`database.rs:2234-2255`) has no transaction at all.
  *Done when:* paths are collected, the transaction commits, and only then are files unlinked.

- [ ] **T25 — SQLite durability** (Finding 2.3, **High**)
  Set `journal_mode = WAL` and a `busy_timeout` at open (`database.rs:151-157`), and replace the raw
  `std::fs::copy` backup (`lib.rs:1901`) with the online backup API or `VACUUM INTO`.

- [ ] **T26 — Make the FTS index self-maintaining** (Finding 2.5, **High**)
  Convert `media_fts` to external-content FTS5 with insert/update/delete triggers, removing the single
  discarded manual insert at `database.rs:1069`. Today sync-ingested media is never indexed and
  deleted media is never removed.

- [ ] **T27 — Add the missing indexes** (Finding 2.6, Medium)
  `media.file_path`, `is_deleted`, `is_archived`, `created_at`, `date_taken`, `telegram_media_id`, the
  four AI status columns, `album_media.media_id`, `faces.media_id`. Switch the 41 `conn.prepare(`
  call sites to `prepare_cached`.

- [ ] **T28 — Fix upload-queue atomicity** (Finding 2.7, Medium)
  `UNIQUE` on `upload_queue(file_path)`; make `toggle_favorite` a single `RETURNING`
  (`database.rs:2034-2047`); add a reaper for rows stranded in `uploading`.

- [ ] **T29 — Unpoison the database mutex** (Finding 4.1, **High**)
  `get_conn` maps poisoning to `Err` (`database.rs:138-148`), so one panic disables all 89 database
  methods for the process lifetime; use `.unwrap_or_else(|e| e.into_inner())`. Delete the per-face
  DEBUG `PRAGMA foreign_key_list` block at `database.rs:696-708`, which is the most likely trigger and
  runs on every embedding write.

- [ ] **T30 — Get blocking work off the async runtime** (Finding 4.2, **High**)
  `spawn_blocking` for ONNX inference (`lib.rs:2513-2542`, `2450-2492`), `hash_file_streaming`
  (`sync_worker.rs:55,88,201`) and the 39 `std::fs::` calls in `lib.rs`; stop holding the Telegram
  client mutex across whole transfers (`telegram.rs:276-338, 431-460`); fix the inconsistent lock
  ordering between `lib.rs:511` and `sync_worker.rs:60-68`.

- [ ] **T31 — Resolve ffmpeg by absolute path** (Finding 3.9, Medium)
  A planted `ffmpeg.exe` earlier in `PATH` executes on the next video import.
  *Where:* `media_utils.rs:177, 183, 212`.

- [ ] **T32 — Wire up typed errors** (Findings 4.5, 6.3, Medium/**High**)
  `errors.rs` defines `AppError` and is referenced zero times; all 74 commands return
  `Result<T, String>`. Adopting it removes the string-matched startup retry at `App.tsx:43-63` and
  stops raw SQL and absolute paths leaking into the UI.

- [ ] **T33 — Verify Telegram plaintext purge during migration** (Finding 3.8, Medium)
  `lib.rs:609-623` discards the delete result and marks the migration succeeded regardless.
  *Done when:* deletion is verified, `FLOOD_WAIT` is retried, and un-purged message IDs are recorded
  durably and surfaced in the UI.

- [ ] **T34 — Bind `camera_make` and clamp pagination** (Findings 3.5, 3.7, **High**/Medium)
  Bind the interpolated filter at `database.rs:1530-1537`; clamp `get_media_by_person`
  (`database.rs:2758-2777`) and `get_media_by_tag` (`database.rs:3074-3092`); fix the
  `-1i32 as usize` cast in `semantic_search` (`lib.rs:2469-2473`) and bound the placeholder counts in
  `get_media_by_ids`, `bulk_delete` and `bulk_set_favorite`.

- [ ] **T35 — Commit a generated `schema.sql`** (Finding 2.4d, **High**)
  There is no schema snapshot anywhere; the only definition is ~480 lines of string literals inside
  `migrate()`.

- [ ] **T36 — Invert `set_config` to an allowlist** (Finding 3.10, Low)
  The `security_` denylist at `lib.rs:1686-1694` still lets script write any other key, including the
  AI opt-in flags. Validate each value against its expected domain.

- [ ] **T37 — Fix logging** (Finding 4.7, Low)
  Stop logging private Telegram message text (`telegram.rs:139-141`); downgrade the 50 `println!`
  calls to `log::debug` and scrub user paths and IDs.

---

## Stage 3 — Quality and maintainability

- [ ] **T38 — `Tags.tsx` must use `convertFileSrc`** (Finding 6.8, Medium)
  `src/components/Tags.tsx:86` hand-builds `asset://localhost/...`, which does not resolve on Windows.
  Likely user-visible today; ~15 minutes.

- [ ] **T39 — Five functional frontend bugs** (Finding 6.9, **High**/Medium)
  Infinite-scroll deadlock on large viewports (`MediaGrid.tsx:839-845` with the 20-item initial load
  at `Gallery.tsx:66`); `media-added` resetting pagination (`Gallery.tsx:75-80`); nested Trash context
  menus (`Trash.tsx:39-50`); `FLOOD_WAIT` progress going negative past 60s (`UploadQueue.tsx:198`);
  missing `public/placeholder.jpg` with a self-retriggering `onError` (`DuplicateReview.tsx:26,229`).

- [ ] **T40 — One `<EnableEncryption>` component** (Finding 6.1, **High**)
  The Settings path (`Settings.tsx:601-609`) has no verification gate and no download/print/copy
  affordances, so a user can enable encryption and permanently lose recoverability. Extract the safe
  onboarding flow (`Onboarding.tsx:170-194`) and use it in both places.

- [ ] **T41 — Recovery-key handling in onboarding** (Findings 6.2, 6.9, **High**)
  Close the print window and surface a blocked `window.open` (`Onboarding.tsx:101-111`); handle the
  clipboard rejection (`:526-528`); defer the blob revoke (`:89-99`); mask the `apiHash` input
  (`:620-627`).

- [ ] **T42 — Stop remounting every grid cell** (Finding 6.4, **High**)
  Hoist `SelectableItemWrapper` (`Gallery.tsx:158`) and `ItemWrapper` (`Trash.tsx:124`) to module
  scope; `memo()` the `Cell`; `useCallback` the ten handler props; key by `item.id` rather than array
  index (`MediaGrid.tsx:335`, `DuplicateReview.tsx:206`); throttle scroll state through `rAF`.

- [ ] **T43 — Add a request guard to the shared fetch path** (Finding 6.10, Medium)
  There are zero `AbortController`s or request guards; a monotonic request id fixes `Search.tsx`,
  `Tags.tsx`, `SmartAlbums.tsx` and `MediaViewer.tsx` at once. In encrypted mode the viewer can
  currently display the wrong decrypted photo.

- [ ] **T44 — Accessibility** (Finding 6.5, **High**)
  Zero `aria-label` in app code against 26 icon buttons; the photo grid is a click-only `<div>`
  (`MediaGrid.tsx:412-415`) and is unreachable by keyboard; `MediaViewer` has no arrow-key navigation
  because it receives a single item rather than a list and an index.

- [ ] **T45 — Delete dead code and de-duplicate the copied flows** (Finding 6.6, Medium)
  Remove `LoginView.tsx`, `Sidebar.tsx`, `ThemeSwitcher.tsx`; extract one `<TelegramLogin>` from the
  three implementations; replace the seven copy-pasted pagination blocks with one
  `usePaginatedMedia(fetcher)` hook plus a `MediaListView` shell (~400 lines deleted, and it fixes the
  duplicate-React-key bug in `Favorites`/`Archive`/`Trash`).

- [ ] **T46 — Apply the existing `map_media_row` helper** (Finding 4.6, Medium)
  It exists at `database.rs:1401-1434` and is used 3 times; the 24-field mapping is written inline 17
  more times. Roughly 500 lines deleted with no behaviour change: the highest-value, lowest-risk
  refactor in the repository.

- [ ] **T47 — Fix the error boundary** (Finding 6.7, Medium)
  Move it outermost in `main.tsx` so `ThemeProvider` is covered, add a reload button, add per-view
  boundaries, and report crashes instead of `console.error`.

- [ ] **T48 — Frontend lifecycle correctness** (Findings 6.7, 6.6, Medium)
  Fix the two broken Tauri listener cleanups (`Settings.tsx:125-133`, `Gallery.tsx:60-87`; the correct
  pattern is at `UploadQueue.tsx:80-86`); clear the scroll timeout (`MediaGrid.tsx:861`); remove the
  prop-mirroring `localItems` state and lift the eight mutation handlers out of `MediaGrid`.

- [ ] **T49 — Duplicate detection and the N+1 queries** (Finding 4.3, **High**)
  Parse each phash once and bucket by prefix instead of the O(n^2) pairwise scan under the connection
  lock (`database.rs:2653-2660`, `113-131`); fix the four N+1 patterns (`lib.rs:2291-2302`,
  `1562-1573`, `1504-1514`, `database.rs:2436-2441`).

- [ ] **T50 — Decide the map question** (Finding 6.7, Medium)
  `MapView` loads OSM tiles and unpkg marker icons that the CSP blocks, so the map cannot render, and
  the requests contradict the privacy positioning. Either vendor the Leaflet assets and allow the tile
  host in `img-src`, or remove the map.

- [ ] **T51 — Passphrase and recovery-key lifecycle** (Findings 1.7, 1.8, Medium)
  Rotate the recovery wrap and verifier inside `recover_and_rewrap` and return a fresh key; add
  `change_passphrase(old, new)`; hoist the 8-character check into one shared validator that also
  covers the reset path and resolves the trim inconsistency (`security/mod.rs:100-107, 143-166`).

- [ ] **T52 — Widen the nonce** (Finding 1.6, Medium)
  The chunk counter overwrites the last 4 bytes of the random base nonce (`security/mod.rs:289-293`),
  leaving 64 bits of per-file entropy. Store a random `file_id` in the header and derive a per-file
  subkey; composes with T21.

- [ ] **T53 — Trim the bundle** (Finding 7.7, Medium/Low)
  Delete the unused `react-window*`, `react-virtualized-auto-sizer`, `motion`, `date-fns` and
  `@tauri-apps/plugin-fs` dependencies and the stale `optimizeDeps` block (`vite.config.ts:16-18`);
  add code splitting to the single 876 kB chunk.

- [ ] **T54 — Add error reporting** (Findings 7.7, 4.7, Medium)
  Nothing is reportable from a shipped build today: `println!` goes to a discarded stdout and
  `console.*` is invisible in a packaged desktop app. Add reporting on both sides and audit the
  payload for PII.

- [ ] **T55 — Remove broken and dead helpers** (Finding 4.7, Low)
  `escape_like_pattern` (`media_utils.rs:252-256`) inserts literal backslashes with no `ESCAPE '\'`
  clause, so it breaks searches rather than escaping them; its only caller `Database::search_media` is
  dead. Also delete `Database::get_persons`, the `debug_reset_faces` command, and the LLM
  self-dialogue comments at `lib.rs:878-901`.

- [ ] **T56 — Release profile and target gating** (Findings 7.6, 4.7, Medium)
  Add `[profile.release]` with `lto` and `strip`; target-gate `windows-sys` (`Cargo.toml:63`) so
  non-Windows builds are possible.

- [ ] **T57 — Extend the Vitest suite** (Finding 7.4, Medium)
  Next targets are the pure functions still buried in components: `parseDateTakenToTimestamp` and the
  grouping helpers (`MediaGrid.tsx:40-88`), `buildDisplayRows` and `findRowIndexAtOffset`
  (`MediaGrid.tsx:198-242, 796-837`), the search-history helpers (`Search.tsx:23-37`), `createFilters`
  (`Search.tsx:114-121`), `toErrorMessage` (`Onboarding.tsx:79-87`); then `usePaginatedMedia` from T45
  against a mocked fetcher, asserting dedupe, `hasNextPage=false` on an empty page, and the T43 race
  guard.

- [ ] **T58 — Split the god files** (Findings 4.6, 6.6, Medium)
  `database.rs` (3,264 lines, 89 public methods, three unlabelled `impl` blocks) and `lib.rs` (2,545
  lines, all 74 command handlers) split cleanly along domain lines; `Settings.tsx` (1,302 lines, 17
  `useState`, five unrelated concerns) splits per tab.

---

## Not action items

Recorded so they are not "fixed" by mistake, per Section 10 of the merged review.

- The `grammers` git dependency is pinned to an immutable `rev`, the same one for both crates. That is
  correct practice (Finding 8.3).
- The cryptographic primitives are sound: Argon2id at 64 MiB / t=3 with random salts, `OsRng` master
  key, AES-256-GCM, a separate PHC recovery verifier, fail-closed locking. The defects above are in
  the layer that decides *when* to use them, not in the crypto itself (Section 9).
- Committing the *used* face-detection model is defensible for offline operation; only the unused
  duplicate is indefensible (T19). Document the provenance of whichever remains (Finding 8.5).
