//! Monitor's entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<monitor::MonitorShell>();
}
