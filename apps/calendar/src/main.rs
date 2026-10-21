//! Calendar — standard Tilo application entry point.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The time-zone database, the CalDAV client and the contact directory come
// from their engine libraries, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_contact_directory_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_timezone_engine;

ui::main!(calendar::CalendarShell);
