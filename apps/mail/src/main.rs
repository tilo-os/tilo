//! Mail — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody with the IMAP client, the contact directory, and the
// message codecs with the SMTP client come from their engine libraries, not
// the runtime or the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_contact_directory_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_mail_engine;

ui::main!(mail::MailShell);
