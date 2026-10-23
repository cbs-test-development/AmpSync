// Release builds are GUI-subsystem apps on Windows: no console window is created alongside the
// app (closing that console would otherwise kill the app). Debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ampsync_desktop_lib::run()
}
