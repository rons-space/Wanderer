//! The error type every command returns.
//!
//! Two problems this exists to solve. The frontend used to branch on error *text*
//! (`message.includes("Database not initialized")` in `App.tsx`), which breaks the
//! moment a message is reworded. And every failure reached the interface as
//! `e.to_string()`, so a rusqlite error put the failing SQL on screen and an
//! `std::io::Error` put an absolute path there, complete with the user's account name.
//!
//! So an `AppError` carries three things: a stable `code` the frontend can match on, a
//! `message` written for a person, and, for errors that came from a library rather than
//! from us, a `detail` that is logged and never serialized.

use serde::{Serialize, Serializer};
use std::fmt;

/// Stable, machine-readable classification. Serialized as the `code` field, in
/// camelCase, and matched by the frontend instead of the message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// Startup has not finished opening the library yet. The frontend retries.
    DatabaseNotInitialized,
    /// The library is encrypted and no master key is loaded.
    VaultLocked,
    /// The request named something that does not exist.
    NotFound,
    /// The request itself was wrong: a bad value, a key that may not be written.
    InvalidInput,
    /// The requested operation is not available in the current state.
    Unavailable,
    /// Something failed inside the database.
    Database,
    /// Something failed in the filesystem.
    Io,
    /// Something failed while talking to Telegram.
    Telegram,
    /// Anything else.
    Internal,
}

impl ErrorCode {
    /// What the user is told when the underlying error is not safe to show.
    fn generic_message(self) -> &'static str {
        match self {
            ErrorCode::DatabaseNotInitialized => "The library is still opening",
            ErrorCode::VaultLocked => "Unlock the library to continue",
            ErrorCode::NotFound => "That item could not be found",
            ErrorCode::InvalidInput => "That request was not valid",
            ErrorCode::Unavailable => "That feature is not available right now",
            ErrorCode::Database => "The library database could not complete the request",
            ErrorCode::Io => "A file could not be read or written",
            ErrorCode::Telegram => "Telegram could not complete the request",
            ErrorCode::Internal => "Something went wrong",
        }
    }
}

/// An error on its way to the frontend.
#[derive(Debug)]
pub struct AppError {
    code: ErrorCode,
    /// Shown to the user. Only ever a message this codebase wrote.
    message: String,
    /// The underlying library error. Logged, never serialized: this is where the SQL
    /// text and the absolute paths live.
    detail: Option<String>,
}

impl AppError {
    /// An error with a message written for the user.
    ///
    /// Use this for text this codebase controls. Anything coming out of a dependency
    /// belongs in `detail`, through one of the `From` impls.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// An error whose cause came from a library and is not safe to display.
    fn opaque(code: ErrorCode, detail: impl fmt::Display) -> Self {
        let detail = detail.to_string();
        // Logged here, at the point of conversion, because this is the last place the
        // real cause exists: the frontend gets the generic message instead.
        log::error!("{:?}: {}", code, detail);
        Self {
            code,
            message: code.generic_message().to_string(),
            detail: Some(detail),
        }
    }

    pub fn database_not_initialized() -> Self {
        Self::new(
            ErrorCode::DatabaseNotInitialized,
            "Database not initialized",
        )
    }

    pub fn vault_locked(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::VaultLocked, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    pub fn telegram(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Telegram, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

/// Serialized as `{ "code": ..., "message": ... }`.
///
/// `detail` is deliberately absent. Tauri sends whatever this produces straight to
/// JavaScript, so anything included here is on screen and in any bug report screenshot.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("message", &self.message)?;
        state.end()
    }
}

// Errors from our own code arrive as strings and are already written for a person:
// `"Encryption is not initialized for this library"`, and so on. They keep their text.
impl From<String> for AppError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        Self::internal(message)
    }
}

// Errors from dependencies name schemas, paths and network internals. They are logged
// and replaced.
impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        Self::opaque(ErrorCode::Database, err)
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::opaque(ErrorCode::Io, err)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::opaque(ErrorCode::Internal, format!("{:#}", err))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::opaque(ErrorCode::Internal, err)
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(err: tokio::task::JoinError) -> Self {
        Self::opaque(ErrorCode::Internal, err)
    }
}

impl From<tauri::Error> for AppError {
    fn from(err: tauri::Error) -> Self {
        Self::opaque(ErrorCode::Internal, err)
    }
}

impl From<Box<dyn std::error::Error>> for AppError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        Self::opaque(ErrorCode::Internal, err)
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for AppError {
    fn from(err: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::opaque(ErrorCode::Internal, err)
    }
}

/// Result type for application operations
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn serialized(err: &AppError) -> serde_json::Value {
        serde_json::to_value(err).expect("serialize")
    }

    #[test]
    fn the_frontend_receives_a_code_and_a_message() {
        let json = serialized(&AppError::database_not_initialized());
        assert_eq!(json["code"], "databaseNotInitialized");
        assert_eq!(json["message"], "Database not initialized");
    }

    /// The whole point of `detail`: a rusqlite error names tables and columns, and
    /// used to be handed to the interface verbatim.
    #[test]
    fn a_database_error_never_reaches_the_frontend_verbatim() {
        let err = AppError::from(rusqlite::Error::InvalidColumnName(
            "no such column: secret_table.secret_column".to_string(),
        ));

        let json = serialized(&err);
        assert_eq!(json["code"], "database");
        assert_eq!(
            json["message"],
            "The library database could not complete the request"
        );
        assert!(
            !json.to_string().contains("secret_column"),
            "the underlying error leaked into the payload: {}",
            json
        );
        assert!(
            err.detail().expect("detail kept").contains("secret_column"),
            "the cause has to survive for the log"
        );
    }

    /// An io error carries the path, which carries the account name.
    #[test]
    fn an_io_error_does_not_disclose_the_path() {
        let err = AppError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "/home/someone/Pictures/private.jpg not found",
        ));

        let json = serialized(&err);
        assert_eq!(json["code"], "io");
        assert!(!json.to_string().contains("/home/someone"));
    }

    /// Messages this codebase writes are meant for the user and are kept.
    #[test]
    fn our_own_messages_are_shown_as_written() {
        let json = serialized(&AppError::vault_locked("Unlock encryption to view media"));
        assert_eq!(json["code"], "vaultLocked");
        assert_eq!(json["message"], "Unlock encryption to view media");
    }

    #[test]
    fn every_code_has_a_generic_message() {
        for code in [
            ErrorCode::DatabaseNotInitialized,
            ErrorCode::VaultLocked,
            ErrorCode::NotFound,
            ErrorCode::InvalidInput,
            ErrorCode::Unavailable,
            ErrorCode::Database,
            ErrorCode::Io,
            ErrorCode::Telegram,
            ErrorCode::Internal,
        ] {
            assert!(!code.generic_message().is_empty());
        }
    }
}
