//! video-player — thin binary entry point. All logic lives in the library.
//!
//! Every mode runs on the app framework: the live window (opening a file
//! given as `--open`, a path or a URL), and the `--shots`,
//! `--responsive-matrix`, `--native-audit` and `--headless` captures, which
//! stage their scenario through the same host.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    video_player::remember_launch_request();
    ui::launch::run_or_exit(&video_player::framework_launch_spec());
}
