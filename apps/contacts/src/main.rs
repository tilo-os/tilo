//! Contacts — standard Tilo application entry point.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(contacts::ContactsShell);
