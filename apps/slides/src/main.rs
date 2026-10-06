#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Slides — thin binary entry point. All logic lives in the library; the
//! screenshot scenarios are `SlidesShell`'s.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(slides::SlidesShell);
