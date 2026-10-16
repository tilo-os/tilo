//! video-player — thin binary entry point. All logic lives in the library.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<video_player::VideoPlayerShell>();
}
