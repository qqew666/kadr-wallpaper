#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod server;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod state;
#[cfg(windows)]
mod windows_app;

#[cfg(windows)]
fn main() {
    if let Err(message) = windows_app::run() {
        windows_app::show_error(&message);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Кадр устанавливает живые обои в Windows 10 и 11.");
}
