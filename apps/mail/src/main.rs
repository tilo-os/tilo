//! Mail — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody with the IMAP client, and the contact directory, come from
// their engine libraries, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_contact_directory_engine;

ui::main!(mail::MailShell);
