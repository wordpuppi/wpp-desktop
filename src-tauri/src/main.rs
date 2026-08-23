// Prevents an extra console window on Windows in release. macOS-first, but harmless.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wordpuppi_desktop_lib::run();
}
