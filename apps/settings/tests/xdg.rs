// One binary for the suites that redirect the process-global XDG config root,
// kept out of `it` so they never share a process with the UI interaction suite.
// `#[path]` keeps the files in place.

#[path = "accounts_mail_loop.rs"]
mod accounts_mail_loop;
#[path = "cloud_storage.rs"]
mod cloud_storage;
#[path = "cloud_ux.rs"]
mod cloud_ux;
#[path = "prefs_persistence.rs"]
mod prefs_persistence;
