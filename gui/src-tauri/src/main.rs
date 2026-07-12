// Prevents an extra console window on Windows in release. macOS-only app, but
// the attribute is harmless and kept for parity with the Tauri template.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    easytier_mac_gui_lib::run()
}
