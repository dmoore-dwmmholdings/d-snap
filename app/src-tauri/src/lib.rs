//! Tauri 2 shell for D-Snap. Commands are added by the Tauri backend chain.

/// Builds and runs the Tauri application.
///
/// Errors from the event loop are reported on stderr instead of panicking.
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("D-Snap failed to start: {err}");
        std::process::exit(1);
    }
}
