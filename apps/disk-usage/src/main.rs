#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<disk_usage::DiskUsageShell>();
}
