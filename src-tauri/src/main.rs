// Pronto is a desktop GUI in every build profile. Keeping the Windows subsystem
// unconditional prevents dev/test launchers from exposing a console as well.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args().any(|argument| argument == "--diagnose") {
        pronto_lib::diagnose();
        return;
    }
    pronto_lib::run();
}
