#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The time-zone database comes from its engine library, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_timezone_engine;

fn main() {
    ui::framework::run::<clocks::ClocksShell>();
}
