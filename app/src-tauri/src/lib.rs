//! Tauri 2 shell for D-Snap (DSNA-66): commands over the core facade, progress and change
//! events.

pub mod commands;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use commands::AppState;
use dsnap_bridge::backend::{Backend, Emit};

/// Registers the commands and lists their names, from one list so they cannot drift.
macro_rules! commands {
    ($($name:ident),* $(,)?) => {
        /// Every registered command name.
        const REGISTERED: &[&str] = &[$(stringify!($name)),*];
        // The bridge tests its list against `commands.ts`; this keeps the two in step.
        const _: () = assert!(
            dsnap_bridge::same_names(REGISTERED, dsnap_bridge::COMMAND_NAMES),
            "registered commands differ from dsnap_bridge::COMMAND_NAMES"
        );

        fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$(commands::$name),*]
        }
    };
}

commands!(
    list_projects,
    add_project,
    rename_project,
    remove_project,
    relocate_project,
    reveal_in_explorer,
    pick_folder,
    get_data_dir,
    get_project_settings,
    set_project_settings,
    get_global_settings,
    set_global_settings,
    snapshot,
    status,
    cancel_operation,
    list_versions,
    set_label,
    set_pinned,
    delete_version,
    changes,
    file_diff,
    read_blob_as_data_url,
    restore_plan,
    restore_project,
    restore_file,
    revert_hunk,
);

/// Builds and runs the Tauri application.
///
/// Errors from the event loop are reported on stderr instead of panicking. A data folder
/// that cannot be opened does not stop the app: every command returns that error, so the
/// UI can show it.
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let emit: Emit = Arc::new(move |name, payload| {
                let _ = handle.emit(name, payload);
            });
            app.manage(AppState(Arc::new(Backend::open(None, emit))));
            Ok(())
        })
        .invoke_handler(handler())
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("D-Snap failed to start: {err}");
        std::process::exit(1);
    }
}
