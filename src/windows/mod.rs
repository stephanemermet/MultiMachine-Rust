mod capture;
mod clipboard;
mod data;
mod display;
mod drag;
mod injector;
mod input;
mod layout_dialog;
mod platform;
mod storage;
mod ui;
pub fn run() {
    if let Err(error) = ui::run() {
        storage::log("application.start_failed");
        unsafe {
            use ::windows::{Win32::UI::WindowsAndMessaging::*, core::*};
            MessageBoxW(
                None,
                PCWSTR(
                    platform::wide(&format!("MultiMachine Rust ne peut pas démarrer : {error}"))
                        .as_ptr(),
                ),
                w!("MultiMachine Rust"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
