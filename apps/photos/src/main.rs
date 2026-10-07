#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Photos — thin binary entry point. All logic lives in the library; the
//! screenshot scenarios are `PhotosShell`'s.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(photos::PhotosShell);
