// Not referenced anywhere yet: all 74 commands return `Result<T, String>`, and the
// frontend string-matches the messages. T32 (issue #40) wires this up and removes that
// matching; until then the module is the contract it will be wired to.
#![allow(dead_code)]

use serde::Serialize;
use thiserror::Error;

/// Application-wide error types that serialize cleanly to JSON for frontend consumption.
#[derive(Debug, Error, Serialize)]
#[serde(tag = "type", content = "message")]
pub enum AppError {
    #[error("Database not initialized")]
    DatabaseNotInitialized,

    #[error("AI feature unavailable: {0}")]
    AiUnavailable(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Telegram error: {0}")]
    Telegram(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::Database(err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err.to_string())
    }
}

/// Result type for application operations
pub type AppResult<T> = Result<T, AppError>;
