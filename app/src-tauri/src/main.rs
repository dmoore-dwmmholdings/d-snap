//! D-Snap desktop entry point.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dsnap_app_lib::run();
}
