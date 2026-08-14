# Agent Instructions

Wanderer, a Tauri 2 desktop media manager: a React 19 + TypeScript + Vite frontend and a
Rust backend with SQLite, on-device AI and encrypted backup to Telegram. This file covers
the conventions that are not discoverable from the code and that cause real damage when
guessed at. Read [`docs/GIT_WORKFLOW.md`](docs/GIT_WORKFLOW.md) before doing anything
involving branches or merges.

## Branching, in one paragraph

`main` is the default branch and deploys. `dev` is the integration branch. Feature and fix
branches are cut from `dev` and open pull requests **into `dev`**. Batches of `dev` are
promoted to `main` through a promotion pull request. A workflow syncs `dev` after anything
lands on `main`: a promotion is a fast-forward, so the two branches sit at the **same
commit** between promotions, while a hotfix landing while `dev` has moved on is merged
down instead and leaves `dev` containing `main` but ahead of it.

## Rules that break things if broken

1. **Promotion and hotfix pull requests must be merged with "Create a merge commit".**
   Squash and rebase rewrite history into commits `dev` has never seen, which makes the
   automatic fast-forward impossible and needs a force push to recover. Both are disabled
   in repository settings. Do not re-enable them.

2. **Never force push `main` or `dev`, and never sync them by hand.**
   `.github/workflows/sync-dev-to-main.yml` owns that. If it fails, read its run summary
   rather than fixing the branches manually.

## CI

`.github/workflows/ci.yml` runs on every pull request to `dev` and `main`, in three jobs:
the frontend one (`npm ci`, `npm run build`, `npm run lint`, `npm test`), the Rust one
(`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`) and
`cargo audit`. A full cycle takes two to four minutes, and a green pull request now means
something.

The lint budget is a ratchet: `npm run lint` is `eslint . --max-warnings 34`, so a new
warning fails the build and removing warnings means lowering the number in `package.json`.
Advisories that cannot be cleared are listed with their reasoning in
`src-tauri/.cargo/audit.toml` rather than silenced.

Locally, the frontend checks all work and are worth running before pushing:

- `npx tsc --noEmit` type-checks without bundling, and is the fastest of the three.
- `npm run lint` and `npm test`.
- `cargo fmt --check` in `src-tauri/`, which needs no compilation.

Do not try to build or run the whole desktop application to validate a change. A full
`cargo build` compiles ONNX Runtime, bundled SQLite and the image stack from scratch, which
costs far more than the change is usually worth and does not fit in a small sandbox. Prefer
the narrowest check that actually covers the edit, and let CI do the rest.

## Conventions

- Match the surrounding code. This repository favours explanatory comments that record
  *why* a non-obvious choice was made, especially in the security and cryptography module,
  the SQLite migration chain in `database/migrations.rs`, and workflow configuration. Preserve them,
  and add to them when the reasoning is not self-evident.
- The frontend is strict TypeScript and the codebase honours it: no `@ts-ignore`, no
  `@ts-expect-error`, and `as any` only where a dependency genuinely requires it. Keep it
  that way.
- Every frontend call into Rust goes through the typed facade in `src/lib/api.ts`. Do not
  call `invoke()` directly from a component; nothing does any more, and the dead file that
  used to is gone.
- The Rust side is split by domain rather than by layer. `database/` is one module per
  table group, each re-opening `impl Database`; `commands/` is one module per domain, and
  `lib.rs` keeps only the shared state, the helpers and `run()`. Settings is the same shape
  on the frontend: `components/settings/` is one component per tab. Put a new method or
  command in the module it belongs to rather than at the end of the largest file.
