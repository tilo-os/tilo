//! `tilo-account-service serve --socket <path>`
//!
//! The accounts service sandboxed apps reach over their launcher-issued
//! descriptor (see `account_custody::service`). Started by
//! `tilo-account-service.socket` when an app that declares the service is
//! launched; takes the systemd listener when there is one, else binds
//! `<path>` itself. Nothing secret is written to stdout or stderr.

use std::os::fd::FromRawFd as _;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use account_custody::Custodian;

fn usage() -> ExitCode {
    eprintln!("usage: tilo-account-service serve --socket <path>");
    ExitCode::from(64)
}

#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to spawned children; cleared once, before any thread is spawned"
)]
fn systemd_listener(expected: &Path) -> Result<Option<UnixListener>, String> {
    const SYSTEMD_LISTEN_FD: i32 = 3;
    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    if std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
        return Err("systemd must pass exactly one accounts listener".into());
    }
    // SAFETY: fcntl only checks that fd 3 is open.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } == -1 {
        return Err("systemd accounts listener fd is closed".into());
    }
    // SAFETY: systemd's LISTEN_PID/FDS contract transfers fd 3 to this
    // process exactly once.
    let listener = unsafe { UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    let actual = listener.local_addr().map_err(|error| error.to_string())?;
    if actual.as_pathname() != Some(expected) {
        return Err("systemd accounts listener has an unexpected path".into());
    }
    // SAFETY: fcntl on the descriptor this process now owns.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err("cannot mark the accounts listener close-on-exec".into());
    }
    // SAFETY: activation is consumed once, before any thread exists.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
}

fn bind(socket: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(socket) {
        if !metadata.file_type().is_socket() {
            return Err("refusing to replace a non-socket accounts path".into());
        }
        std::fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let listener = UnixListener::bind(socket).map_err(|error| error.to_string())?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    Ok(listener)
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("serve")) {
        return usage();
    }
    let socket = match (args.next(), args.next()) {
        (Some(flag), Some(path)) if flag == "--socket" => PathBuf::from(path),
        _ => return usage(),
    };
    let listener = match systemd_listener(&socket).and_then(|taken| match taken {
        Some(listener) => Ok(listener),
        None => bind(&socket),
    }) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("tilo-account-service: {error}");
            return ExitCode::FAILURE;
        }
    };
    let custodian = Custodian::live();
    match account_custody::service::serve(&custodian, &listener) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-account-service: {error}");
            ExitCode::FAILURE
        }
    }
}
