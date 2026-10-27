#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
mod windows;
fn main() {
    #[cfg(windows)]
    windows::run();
    #[cfg(not(windows))]
    eprintln!(
        "MultiMachine Rust est une application Windows. Le moteur et les tests fonctionnent sous Linux."
    );
}
