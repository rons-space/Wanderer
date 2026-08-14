//! Logging that survives being packaged.
//!
//! `env_logger` writes to stderr, and a release build sets
//! `windows_subsystem = "windows"` precisely so that no console window appears,
//! so on the platform this app targets first every `log::` call went nowhere.
//! There was no way for a user to tell us what happened, and no way for us to
//! ask. This writes the same records to a file next to the app's data instead,
//! keeps stderr as well while developing, and exposes the path so the UI can
//! point at it.
//!
//! Two things it deliberately does not do. It does not send anything anywhere:
//! the file stays on the user's disk until they choose to share it. And it does
//! not log messages verbatim, because the messages in this codebase are full of
//! paths (see `paths::purge_scratch_dirs`, which logs whatever it failed to
//! delete) and a path contains the account name. `scrub` rewrites the home
//! directory out of every line before it is written.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use log::{LevelFilter, Log, Metadata, Record};

/// Rotate once the file passes this, keeping one previous generation. Two
/// megabytes is a few thousand lines: enough to cover the session that went
/// wrong, small enough to paste into an issue.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Where the log file lives, once logging has been initialized.
pub fn log_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

/// The directory holding the log file and its one rotated generation.
pub fn log_dir(app_data: &Path) -> PathBuf {
    app_data.join("logs")
}

/// Replace the user's home directory with `~` wherever it appears.
///
/// Not a general PII scrubber, and it cannot be one: a log line can contain
/// anything a caller decided to format into it. It removes the identifier that
/// is genuinely in almost every line that mentions a file, which is the account
/// name embedded in `/home/<name>` or `C:\Users\<name>`.
pub fn scrub(message: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return message.to_string();
    };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return message.to_string();
    }

    let mut out = message.replace(home.as_ref(), "~");
    // Windows paths reach logs both as typed and as escaped debug output
    // (`{:?}` on a `Path` doubles the separators), so the escaped spelling has
    // to be rewritten too or half the lines keep the account name.
    if home.contains('\\') {
        out = out.replace(&home.replace('\\', "\\\\"), "~");
    }
    out
}

struct FileLogger {
    file: Mutex<Option<File>>,
    path: PathBuf,
    home: Option<PathBuf>,
    /// Also write to stderr. True for debug builds, where a console exists.
    echo: bool,
}

impl FileLogger {
    fn write_record(&self, line: &str) {
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            // A panic in another logging call must not cascade into every
            // subsequent one; dropping the record is the lesser failure.
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(file) = guard.as_mut() {
            if file
                .metadata()
                .map(|m| m.len() > MAX_LOG_BYTES)
                .unwrap_or(false)
            {
                // Keep exactly one generation. Failing to rotate is not worth
                // losing the record over, so the write goes ahead either way.
                let rotated = self.path.with_extension("log.1");
                let _ = std::fs::rename(&self.path, rotated);
                match open_log_file(&self.path) {
                    Ok(fresh) => *guard = Some(fresh),
                    Err(_) => *guard = None,
                }
            }
        }

        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = format!(
            "{} {:<5} {} {}",
            timestamp(),
            record.level(),
            record.target(),
            scrub(&record.args().to_string(), self.home.as_deref()),
        );

        if self.echo {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
        self.write_record(&line);
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

fn open_log_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

/// Install the logger. Called once, before anything that logs.
///
/// A failure to open the file is not fatal: the process still starts, it just
/// logs to stderr only, which is what it did before this module existed.
pub fn init(app_data: &Path) {
    let path = log_dir(app_data).join("wanderer.log");
    let file = open_log_file(&path).ok();
    let opened = file.is_some();

    let logger = FileLogger {
        file: Mutex::new(file),
        path: path.clone(),
        home: dirs::home_dir(),
        echo: cfg!(debug_assertions),
    };

    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<LevelFilter>().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        });

    // `set_logger` rather than `set_boxed_logger`, which is behind the `log`
    // crate's `std` feature: the logger lives for the whole process anyway, so
    // leaking it costs nothing and keeps the dependency's default features.
    if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
        log::set_max_level(level);
        let _ = LOG_PATH.set(path.clone());
        if opened {
            log::info!("Logging to {}", path.display());
        } else {
            log::warn!("Could not open {}; logging to stderr only", path.display());
        }
    }
}

/// A crash or unhandled rejection reported by the webview.
///
/// The frontend's `console.error` is as invisible in a packaged build as
/// `println!` is, so the error boundary and the global handlers send what they
/// caught here to land in the same file as everything else.
pub fn record_frontend_error(context: &str, message: &str, stack: Option<&str>) {
    match stack {
        Some(stack) => log::error!("[webview:{context}] {message}\n{stack}"),
        None => log::error!("[webview:{context}] {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_directory_is_rewritten_out_of_messages() {
        let home = Path::new("/home/alice");
        assert_eq!(
            scrub(
                "Could not purge /home/alice/.local/share/wanderer",
                Some(home)
            ),
            "Could not purge ~/.local/share/wanderer"
        );
    }

    #[test]
    fn escaped_windows_paths_are_rewritten_too() {
        let home = Path::new("C:\\Users\\alice");
        // As a plain path, and as `{:?}` renders it.
        assert_eq!(
            scrub("opening C:\\Users\\alice\\photo.jpg", Some(home)),
            "opening ~\\photo.jpg"
        );
        assert_eq!(
            scrub("opening \"C:\\\\Users\\\\alice\\\\photo.jpg\"", Some(home)),
            "opening \"~\\\\photo.jpg\""
        );
    }

    #[test]
    fn messages_without_the_home_directory_are_untouched() {
        let home = Path::new("/home/alice");
        assert_eq!(
            scrub("upload failed: timeout", Some(home)),
            "upload failed: timeout"
        );
        assert_eq!(
            scrub("/home/bob/photo.jpg", Some(home)),
            "/home/bob/photo.jpg"
        );
    }

    #[test]
    fn an_unknown_home_directory_is_not_an_error() {
        assert_eq!(scrub("/home/alice/x", None), "/home/alice/x");
        assert_eq!(scrub("/home/alice/x", Some(Path::new(""))), "/home/alice/x");
    }

    #[test]
    fn the_log_directory_sits_under_the_app_data_directory() {
        let dir = log_dir(Path::new("/srv/wanderer-data"));
        assert_eq!(dir, PathBuf::from("/srv/wanderer-data/logs"));
    }

    /// Rotation is what stops a long-running install from filling the disk, and
    /// it has to keep the previous generation rather than truncating: the
    /// interesting part of a crash is usually just before the last line.
    #[test]
    fn writing_past_the_limit_rotates_and_keeps_one_generation() {
        let dir = std::env::temp_dir().join(format!("wanderer-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("wanderer.log");

        let logger = FileLogger {
            file: Mutex::new(Some(open_log_file(&path).unwrap())),
            path: path.clone(),
            home: None,
            echo: false,
        };

        let filler = "x".repeat(4096);
        while std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) <= MAX_LOG_BYTES {
            logger.write_record(&filler);
        }
        logger.write_record("after rotation");

        let rotated = path.with_extension("log.1");
        assert!(rotated.exists(), "previous generation was not kept");
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("after rotation"));
        assert!(
            current.len() < MAX_LOG_BYTES as usize,
            "the fresh file should start empty"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
