//! `nested-session-bus ARGS…` runs the dbus-daemon of the environment this executable was built
//! in with ARGS, as `dbus-daemon ARGS…` would.
//!
//! A nested session starts a private session bus per shell (dist/dev/session-bus.sh). Tilo OS
//! hosts run their user bus on dbus-broker and carry no dbus-daemon, and a build that runs on
//! another machine brings back only what cargo built: this executable is how the build
//! environment's dbus-daemon (the build image's, of the same Fedora release as Tilo OS, linking
//! only libraries every Tilo host has) reaches the session. It writes the embedded daemon to an
//! anonymous memory file and executes that, so nothing lands on disk.

use std::ffi::{CString, OsString};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::process::ExitCode;

static DAEMON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dbus-daemon"));

/// Arguments and environment entries come from the kernel as C strings: none holds a NUL.
fn cstrings(items: impl Iterator<Item = OsString>) -> Vec<CString> {
    items
        .filter_map(|s| CString::new(s.into_vec()).ok())
        .collect()
}

fn main() -> ExitCode {
    if DAEMON.is_empty() {
        eprintln!(
            "nested-session-bus: built where no dbus-daemon was installed; \
             build it where one is (the build image has one)"
        );
        return ExitCode::from(127);
    }
    // SAFETY: a valid C string; the descriptor is owned right away.
    let fd = unsafe { libc::memfd_create(c"dbus-daemon".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        eprintln!(
            "nested-session-bus: memfd_create: {}",
            std::io::Error::last_os_error()
        );
        return ExitCode::from(126);
    }
    // SAFETY: memfd_create returned a new descriptor nobody else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    if let Err(e) = file.write_all(DAEMON) {
        eprintln!("nested-session-bus: writing the daemon: {e}");
        return ExitCode::from(126);
    }
    let mut given = std::env::args_os();
    given.next();
    let argv = cstrings(std::iter::once(OsString::from("dbus-daemon")).chain(given));
    let envp = cstrings(std::env::vars_os().map(|(k, v)| {
        let mut kv = k;
        kv.push("=");
        kv.push(v);
        kv
    }));
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|e| e.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());
    // SAFETY: NULL-terminated arrays of valid C strings that outlive the call; on success it
    // does not return.
    unsafe { libc::fexecve(file.as_raw_fd(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) };
    eprintln!(
        "nested-session-bus: fexecve: {}",
        std::io::Error::last_os_error()
    );
    ExitCode::from(126)
}
