//! The Tauri command layer: one module per domain.
//!
//! Each command is a thin wrapper that takes the lock on the piece of
//! `AppState` it needs, calls into the module that does the work, and maps the
//! failure to an `AppError`. They are grouped here rather than in the modules
//! they call so that `run()` can see the whole surface the frontend has.

pub mod ai;
pub mod albums;
pub mod backup;
pub mod config;
pub mod diagnostics;
pub mod media;
pub mod search;
pub mod security;
pub mod telegram;
pub mod uploads;
