//! Calendar — standard Tilo application entry point.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The time-zone database comes from its engine library, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_timezone_engine;

ui::main!(calendar::CalendarShell);
