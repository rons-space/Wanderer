// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Logging is installed inside `run`, once Tauri can tell us where the app
    // data directory is. Anything logged before that point has nowhere to go in
    // a packaged build, which is why there is nothing else in this function.
    tauri_app_lib::run()
}
