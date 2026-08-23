// Pronto is a desktop GUI in every build profile. Keeping the Windows subsystem
// unconditional prevents dev/test launchers from exposing a console as well.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    pronto_lib::run();
}
