//! Disks' binary: the launcher serves the live window and the captures.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<disks::DisksShell>();
}
