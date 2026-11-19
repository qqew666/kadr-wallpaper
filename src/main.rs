#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod server;
#[cfg(windows)]
mod startup;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod state;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod updates;
#[cfg(windows)]
mod windows_app;

#[cfg(windows)]
fn main() {
    if let Some(result) = updates::run_installer_if_requested() {
        if let Err(message) = result {
            windows_app::show_error(&message);
        }
        return;
    }
    if let Err(message) = windows_app::run() {
        windows_app::show_error(&message);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Кадр устанавливает живые обои в Windows 10 и 11.");
}
