#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<clocks::ClocksShell>();
}
