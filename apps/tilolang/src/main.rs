//! The tilolang host: one Tilo app that runs every tilolang app package.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(definition = tilolang_app::definition);
