//! Contacts — standard Tilo application entry point.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The contact directory comes from its engine library, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_contact_directory_engine;

ui::main!(contacts::ContactsShell);
