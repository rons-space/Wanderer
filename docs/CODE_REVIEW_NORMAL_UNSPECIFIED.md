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
