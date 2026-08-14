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

There is no CI in this repository yet. `sync-dev-to-main.yml` is the only workflow, and it
moves branch refs rather than building or testing anything. Nothing type-checks, lints,
builds or tests on push, so a pull request being green means only that nothing ran.

Until that changes, the checks that exist are local and are worth running on what you
touched:

- `npm run build` (`tsc && vite build`) type-checks and bundles the frontend.
- `npm test` runs the Vitest suite.
- `cargo test` and `cargo fmt --check` in `src-tauri/`.

Do not try to build or run the whole desktop application to validate a change. A full
`cargo build` compiles ONNX Runtime, bundled SQLite and the image stack from scratch, which
costs far more than the change is usually worth. Prefer the narrowest check that actually
covers the edit, and when CI exists, push and let it do the work rather than reproducing
it locally.

## Conventions

- Match the surrounding code. This repository favours explanatory comments that record
  *why* a non-obvious choice was made, especially in the security and cryptography module,
  the SQLite migration chain in `database.rs`, and workflow configuration. Preserve them,
  and add to them when the reasoning is not self-evident.
- The frontend is strict TypeScript and the codebase honours it: no `@ts-ignore`, no
  `@ts-expect-error`, and `as any` only where a dependency genuinely requires it. Keep it
  that way.
- Every frontend call into Rust goes through the typed facade in `src/lib/api.ts`. Do not
  call `invoke()` directly from a component. The one file that does, `LoginView.tsx`, is
  dead code that is never imported, so treat it as a counterexample rather than a
  pattern.
