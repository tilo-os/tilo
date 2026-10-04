#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Clocks: world clocks, alarms, timers and a stopwatch on the app framework.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<clocks::ClocksShell>();
}
