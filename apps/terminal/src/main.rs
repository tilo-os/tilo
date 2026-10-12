//! Terminal — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(terminal::TerminalShell);
