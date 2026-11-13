//! Remote's entry point: the live window, the capture and headless modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<remote::RemoteShell>();
}
