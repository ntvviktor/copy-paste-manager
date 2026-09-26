// Keep the console window from appearing alongside the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    clipvim_lib::run()
}
