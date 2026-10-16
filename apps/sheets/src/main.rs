#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Sheets — thin binary entry point. All logic lives in the library; the
//! screenshot scenarios are `SheetsShell`'s.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(sheets::SheetsShell);
