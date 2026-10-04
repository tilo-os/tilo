//! Settings — standard Tilo application entry point.
//!
//! The live window and every `--shots` capture run on the app framework
//! (`SettingsShell`); named captures are its `scenarios`. Appearance changes
//! reach the window through the shared settings file the theme layers watch.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<settings::SettingsShell>();
}
