//! Remote connections — standard Tilo application entry point: the live
//! window and the framework's `--shots`, `--headless` and `--native-audit`
//! modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<remote::RemoteShell>();
}
