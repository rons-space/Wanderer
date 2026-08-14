//! Commands the frontend uses to report its own failures.

use crate::*;

/// Take a webview crash into the same log file as everything else.
///
/// `console.error` in a packaged build is as invisible as `println!` is, so the
/// error boundary and the global handlers call this instead. `context` says
/// where it came from (which view, or which global handler); the rest is
/// whatever the browser gave us.
#[tauri::command]
pub fn report_frontend_error(context: String, message: String, stack: Option<String>) {
    logging::record_frontend_error(&context, &message, stack.as_deref());
}

/// Where the log file is, so the UI can tell the user what to attach.
///
/// Returns the path rather than the contents: the file is the user's, and
/// nothing about reporting a bug should require the app to read it back into
/// the webview.
#[tauri::command]
pub fn get_log_path() -> Option<String> {
    logging::log_path().map(|p| p.display().to_string())
}
