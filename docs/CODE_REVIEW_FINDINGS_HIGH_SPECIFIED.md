# Code Review, Security Audit & Refactor Findings

> **Author:** [code]smith, an asynchronous cloud coding agent from Blacksmith.
> **Date:** 2026-08-13
> **Scope:** Full review of `rons-space/wanderer` — a Tauri 2 desktop photo manager: React 19 + TypeScript 5.8 + Vite 7 frontend (~10.9k LOC) and a Rust backend (~10.7k LOC) with local AI (CLIP, ArcFace face recognition, object detection), SQLite, at-rest encryption, and Telegram sync.
> **Companion PR:** [#2 — centralize pure format helpers into `lib/format` + unit tests](https://github.com/rons-space/wanderer/pull/2).

## About this document

This is a point-in-time engineering assessment produced during an automated code review pass, combining four lenses: a security audit (both the Rust/Tauri backend and the React frontend), a code-quality/refactor review, a dependency check (`npm audit` + `cargo audit`), and a test-coverage review. Findings are grouped by severity and cite concrete `file:line` locations.

Nothing here changes runtime behavior. The only code shipped alongside it is PR #2, which extracts already-duplicated pure formatting helpers into `src/lib/format.ts` and adds tests (verified behavior-identical, `tsc --noEmit` clean). Everything else is a recommendation.

### Who produced this

[code]smith is Blacksmith's cloud coding agent. It reads the code before theorizing about it, cites only what it has actually read, and ships fixes as pull requests. This review was performed by exploring the repository directly (every `#[tauri::command]`, the capabilities/CSP config, the SQLite layer, the crypto module, the Telegram/AI/sync workers, and the React component tree), running `npm audit` and `cargo audit`, and writing/running the accompanying test suite.

### Threat model note

This is a **local-first, single-user** desktop app. There is no server and no multi-user boundary, and every Tauri command is invoked by the app's own trusted frontend. That lowers the severity of "path/SQL comes from the frontend" issues, because the frontend is not normally the attacker. It does **not** eliminate risk: the WebView runs under a permissive CSP (`script-src 'unsafe-eval' 'unsafe-inline'`), so any XSS or malicious npm dependency escalates directly into an extremely broad native command + filesystem surface, and the backend still parses genuinely untrusted **data** (image/EXIF/RAW bytes, downloaded AI models, Telegram content, sync manifests). Findings are prioritized with that in mind.

---

## Executive summary

The engineering quality here is, in several places, high: the cryptography in `security/mod.rs` (Argon2id + AES-256-GCM, random per-wrap salts/nonces, PHC recovery-key verifier) is well done, SQL is almost entirely parameterized, TLS is enforced via rustls, `unsafe` is confined to two audited Windows FFI wrappers, and the frontend is strict-TypeScript with a fully typed Tauri command facade (`src/lib/api.ts`) and zero `@ts-ignore`.

The dominant risk is **capability over-provisioning**: the asset protocol and the `fs` plugin are scoped to the entire filesystem (`**`, `C:\**`), which, combined with the permissive script CSP, turns any frontend compromise into arbitrary local file read. Secondary risks are unconfined path handling in several native commands (export/backup/delete take client-supplied destinations), a plaintext Telegram session at rest, and unverified AI-model downloads. On the frontend, the biggest issues are correctness (stale-closure searches, stale-response overwrites, listener/timer leaks) and heavy duplication (seven copy-pasted paginated list views).

| Area | Headline |
|---|---|
| Security (Rust/Tauri) | Critical: asset + `fs` scope = whole-drive read under a permissive CSP. Unconfined export/backup/delete paths; plaintext Telegram session; unverified model downloads. |
| Security/quality (frontend) | Stale-closure search, stale-response overwrites, leaked Tauri listeners/timers; a Windows-broken thumbnail URL. |
| Code quality | Four god files (Settings 1,302 lines), seven duplicated list views, MediaGrid owning mutations + a shadow item copy, no ESLint. |
| Dependencies | npm: 7 (5 high). cargo: 15 vulns + 8 unsound warnings incl. high `quinn-proto`, `quick-xml`, `rustls-webpki`. |
| Tests | Was zero. PR #2 adds Vitest + 18 tests for the pure format helpers. |

---

## 1. Security — Rust / Tauri backend

### Critical

- **C1 — Asset protocol scoped to the entire filesystem.** `src-tauri/tauri.conf.json:26-35` sets the `assetProtocol` scope to `**`, `C:\**`, `C:/**` plus app-data dirs. Any script in the WebView can `convertFileSrc("C:/Users/victim/.ssh/id_rsa")` (or `/etc/passwd`) and read it. Under the permissive CSP (`script-src 'unsafe-eval' 'unsafe-inline'`, `tauri.conf.json:25`), a single XSS or malicious dependency = arbitrary local file read. *Fix:* restrict the scope to the specific app-data subdirectories that must be served (thumbnails/view-cache/backup); remove `**` and `C:\**`.
- **C2 — `fs` plugin granted read/exists over all of `C:\**`.** `src-tauri/capabilities/default.json:13-40` exposes `fs:allow-read`/`fs:allow-exists` for `C:\**` and all app data — a second, independent arbitrary-read primitive callable straight from JS (`readTextFile`, `exists`). *Fix:* drop `C:\**`; scope reads narrowly, or remove generic FS access from the WebView capability entirely (the app mostly uses backend commands + the asset protocol).

### High

- **H1 — `import_sync_manifest` reads an arbitrary client path + untrusted JSON.** `lib.rs:2325-2331` → `sync_manifest.rs:102-106`: `path: String` flows into `fs::read_to_string` + `serde_json::from_str` with no canonicalization, root check, version check, or entry bound; contents drive DB mutations (favorites, ratings, album creation). *Fix:* constrain to a dialog-chosen file, validate `version`, bound entries.
- **H2 — `export_media` / `backup_database` write to arbitrary client-supplied paths.** `lib.rs:1371-1388` (`create_dir_all` + `fs::copy` to `destination`) and `1862-1902`. No allowed-root confinement → write decrypted photo copies or the DB anywhere writable (e.g. the Startup folder). *Fix:* route through the dialog plugin's user-selected folder, or canonicalize + verify against an export root.
- **H3 — `remove_local_copy` / `permanent_delete` / `download_local_copy` operate on DB-stored paths with no root confinement.** `lib.rs:1943-1975`, `database.rs:2210-2259`, `lib.rs:1977-2065`: `fs::remove_file(media.file_path)` etc. with no check that the path is inside managed roots. Combined with H1 (a manifest that sets `file_path`/`thumbnail_path`), this becomes arbitrary file delete. *Fix:* canonicalize and assert the path is inside the backup/cache/thumbnail roots before any remove/write.
- **H4 — Telegram session stored in plaintext, and credential storage is Windows-only.** `telegram.rs:105-112` writes the grammers `session.db` (full access to the user's Telegram "Saved Messages" photo store) unencrypted even when encryption mode is on; `security/mod.rs:547-559` DPAPI is a no-op off Windows, so `set_telegram_api_credentials` can't store securely on macOS/Linux (`lib.rs:429-435,945-969`). Exfiltrating `session.db` (via C1/C2) hands over the account. *Fix:* encrypt the session at rest with the master key; use the OS keychain instead of DPAPI-only.

### Medium / Low

- **M1** SQL injection (bounded): `search_fts` interpolates the frontend-supplied `camera_make` into `LIKE '%{}%'` with only hand-rolled `'`→`''` escaping (`database.rs:1532-1535`); everything else is parameterized. *Fix:* bind it as a parameter.
- **M2** FFmpeg resolved from `PATH` (`media_utils.rs:158-234`) — argv array avoids shell injection, but a planted `ffmpeg.exe` earlier in `PATH` runs on the next video import. *Fix:* bundled/absolute path.
- **M3** AI models (CLIP/ArcFace/object-detection) downloaded from HuggingFace/GitHub with **no checksum/signature** before feeding to the ONNX/tokenizer parsers (`clip.rs:444-508`, `ai/mod.rs:199-325`). TLS is enforced (good), but a hijacked upstream serves a malicious model. *Fix:* pin + SHA-256 verify, pin immutable revisions.
- **M4** Verbose `println!`/log of full file paths and Telegram message IDs (`lib.rs` many, `telegram.rs:392-423`). No secrets are logged (verified — good). *Fix:* downgrade to `debug`, scrub paths.
- **L1** `set_config` allows arbitrary key writes except the `security_` prefix (`lib.rs:1686-1694`) — whitelist settable keys. **L2** `unwrap()` on untrusted BLOB→array conversions can panic the DB mutex holder (`database.rs:778,793,920`; `sync_worker.rs:148`; `progress_stream.rs:66`) — use checked handling. **L3** AES-GCM chunk framing doesn't authenticate total chunk count, so trailing-chunk truncation is undetected (`security/mod.rs:325-359`) — otherwise the crypto is sound; add a terminator/length record.

### Done well (backend)

Strong key management (Argon2id 64 MiB/t=3, random salts/nonces, AES-256-GCM, PHC recovery verifier, constant-time verify); no hardcoded secrets and none logged (the committed `2`/`build_log.txt`/`output.txt`/`.env.example` contain only placeholders); SQL almost entirely parameterized with clamped pagination; TLS enforced via rustls; `unsafe` confined to two audited DPAPI FFI wrappers; command injection avoided (argv, not shell); encryption applied to thumbnails/uploads/view-cache/DB backups; graceful poisoned-mutex recovery.

---

## 2. Security & correctness — React frontend

- **F1 — Stale-closure search.** `Search.tsx:82-94`: the effect calls `performSearch` with deps `[selectedTag]` only, so it closes over stale `query`/`isAiSearch`/`createFilters`. Wrong results, and a dependency bug ESLint would have caught (there is no ESLint config).
- **F2 — Stale-response overwrites (no cancellation anywhere).** Grep confirms zero `AbortController`/request-guards. Within a view, a slower earlier response wins: `Search.tsx:123-180`, `Tags.tsx:33-41` (click tag A then B → A's photos under B's header), `SmartAlbums.tsx:65-85`, and `MediaViewer.tsx:73-121` (open slow cloud item A, close, open B → A overwrites B). *Fix:* a monotonic request-id / `active` flag in a shared fetch hook.
- **F3 — Leaked Tauri listeners/timers.** `Gallery.tsx:60-87` and `Settings.tsx:125-133` assign `unlisten` after `await`s, so unmounting before `listen()` resolves leaks the listener permanently (fires `setState` on a dead component). `MediaGrid.tsx:652,861-863` never clears `hideHeaderTimeout`; `App.tsx:43-59` retries every 250ms forever with no cap and no unmount cancel. *Fix:* a `useTauriEvent` hook with a cancelled flag; clear timers on unmount.
- **F4 — Windows-broken thumbnail URL.** `Tags.tsx:84-88` hand-builds `asset://localhost/${encodeURIComponent(path)}` instead of `convertFileSrc`, which breaks on Windows (the app's primary platform). 15-minute fix.
- **F5 — Component-type churn causing remounts.** `Gallery.tsx:158` (`SelectableItemWrapper`) and `Trash.tsx:124` (`ItemWrapper`) define components **inside render**, so every parent render unmounts/remounts every wrapped cell (flicker, image reload). Hoist out.
- **F6 — Unhandled rejections / silent failures.** `Gallery.tsx:77-79` (`getMedia().then(setItems)` with no `.catch`); load failures in Favorites/Archive/Trash/PersonDetail/AlbumDetail/Search/MapView go to `console.error` only, so the user sees an empty view indistinguishable from "no items". Error handling is inconsistent (some views toast, most don't).
- **F7 — Single top-level ErrorBoundary** with a raw `error.toString()` and no reset/retry (`ErrorBoundary.tsx`); a crash in any of the 13 views nukes the whole UI.

---

## 3. Code Quality & Refactoring

- **God files.** `Settings.tsx` (1,302 lines, 19 `useState`: auth + encryption + config + backup + CLIP + sync + about), `MediaGrid.tsx` (914: custom virtualizer + cell + context menu + 8 mutation handlers + date grouping), `AppSidebar.tsx` (747: two near-identical themed sidebars), `Onboarding.tsx` (701: 6-step wizard, 21 `useState`).
- **Seven copy-pasted paginated list views (biggest structural smell).** `Favorites`, `Archive`, `Trash`, `Gallery`, `PersonDetail`, `AlbumDetail`, `Search` each reimplement `items/hasNextPage/loadItems/loadNextPage/dedupe/try-catch`; Favorites and Archive are byte-for-byte identical but the API call. Copies have already diverged (some dedupe by id, some don't → duplicate React keys). *Fix:* one `usePaginatedMedia(fetcher)` hook + a `MediaListView` shell (kills ~400 lines and fixes F2/dup-keys in one place).
- **MediaGrid owns mutations + a shadow copy of items.** `MediaGrid.tsx:647-657` keeps `localItems` synced from props and mutates it in 8 handlers → two sources of truth; every consumer also refetches albums+config on mount. `Cell` is not memoized and gets 8 fresh closures per render → scroll jank on large libraries. *Fix:* lift mutations to a `useMediaActions` hook/context, make MediaGrid presentational, `React.memo(Cell)` + `useCallback`.
- **Duplicated helpers** (path basename ×5, byte/speed/ETA formatting, login form logic ×3). PR #2 addresses the format subset.
- **No ESLint/Prettier config at all** despite the strict TS setup; `npm run build` runs `tsc` only. Adding `eslint-plugin-react-hooks` would have flagged F1/F3 automatically.
- **Dead code.** Unused components `Sidebar.tsx`, `LoginView.tsx`, `ThemeSwitcher.tsx`; unused api methods (`searchMedia`, `detectFaces`, `getDeviceId`, `permanentDeleteMedia`); unused deps (`react-window*`, `react-virtualized-auto-sizer`, `motion`, `date-fns`); **both `package-lock.json` and `pnpm-lock.yaml`** committed; `main.tsx:7-8` debug logs; doubled `<ContextMenuSeparator/>` (`MediaGrid.tsx:576-578,597-599`).

### Top quick wins

1. Fix `Tags.tsx` to use `convertFileSrc` (F4) — likely broken on Windows today.
2. Delete dead code + one lockfile + debug logs.
3. Hoist the in-render wrapper components (F5) and memoize `Cell`.
4. Add a request-guard to the shared fetch paths (F2).
5. Add ESLint (`react-hooks`) + the Vitest suite in PR #2 (extend to the extracted date/format helpers).

---

## 4. Dependency Check

**npm — 7 vulnerabilities (5 high, 1 moderate, 1 low), all fixable:** high `vite`, `rollup`, `postcss`, `nanoid`, `picomatch` (build toolchain), moderate `yaml`, low `@babel/core`. Run `npm audit fix`.

**cargo (`cargo audit`) — 15 vulnerabilities + 8 unsound/yanked warnings.** Notable high-severity, all with upgrade fixes:
- `quinn-proto` 0.11.13 — DoS (8.7) and remote memory exhaustion (7.5) → ≥0.11.15.
- `quick-xml` 0.38.4 — quadratic parse + unbounded namespace allocation DoS (7.5 ×2) → ≥0.41.0.
- `rustls-webpki` 0.103.9 — name-constraint bypasses and a CRL-parse panic → ≥0.103.13.
- `bytes` (integer overflow → ≥1.11.1), `crossbeam-epoch`, `tar` (symlink chmod / PAX size, medium → ≥0.4.45), `time` (stack-exhaustion DoS → ≥0.3.47), `tract-nnef` (integer overflow → OOB read on model load → ≥0.21.16).
- Unsound warnings: `event-listener`, `glib`, `memmap2`, `rand` (×3 versions), yanked `core2`.

Recommended: `cargo update` the fixable crates (many are transitive via Tauri/reqwest and move with a lockfile bump), then re-run `cargo audit`; treat `tract-nnef` and `quick-xml` as priorities since they parse untrusted model/metadata input.

---

## 5. Tests

Previously **zero** test infrastructure and no linter. PR #2 adds Vitest (node env, `@` alias, `npm test`) and 18 tests for the pure format helpers now centralized in `src/lib/format.ts`.

Highest-value modules to test next are pure functions currently buried in components (extract as you test): `parseDateTakenToTimestamp` and the day/month/year grouping helpers (`MediaGrid.tsx:40-88`), the `buildDisplayRows` grouping algorithm and `findRowIndexAtOffset` binary search (`MediaGrid.tsx:198-242,796-837`), search-history helpers (`Search.tsx:23-37`), `createFilters` (`Search.tsx:114-121`), and `toErrorMessage` (`Onboarding.tsx:79-87`). Then the extracted `usePaginatedMedia` hook against a mocked fetcher (dedupe, `hasNextPage=false` on empty page, race-guard).

---

## Suggested remediation sequencing

1. **Lock down capabilities first:** restrict the asset scope and the `fs` capability (remove `**`/`C:\**`), and drop `unsafe-eval`/`unsafe-inline` from the script CSP (C1, C2). Single biggest exposure.
2. Encrypt the Telegram `session.db` at rest (H4).
3. Add allowed-root canonicalization to every command that reads/writes/deletes a frontend- or DB-supplied path (H1, H2, H3); parameterize `camera_make` (M1).
4. Checksum-verify downloaded AI models (M3); bump `tract-nnef`/`quick-xml` and run `cargo audit` clean.
5. Frontend correctness: shared fetch hook with request-guards + a `useTauriEvent` cleanup hook (F1, F2, F3), fix `Tags.tsx` (F4), hoist in-render wrappers (F5).
6. Then refactors: `usePaginatedMedia` + `MediaListView`, lift mutations out of MediaGrid, split Settings, add ESLint, extend the test suite.

The reassuring part: this codebase already demonstrates the right patterns in the places that matter most (careful crypto, parameterized SQL, a typed command facade). The Critical items are mostly about tightening configuration and confining paths, not rewriting logic.
