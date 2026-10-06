//! Monitor — thin binary entry point. All logic lives in the library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<monitor::MonitorShell>();
}
