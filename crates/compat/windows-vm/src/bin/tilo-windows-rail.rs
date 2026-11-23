//! Run `FreeRDP`'s X11 RAIL client on the X11 display the launch's app keeper serves.
//!
//! Tilo does not implement an XWM in its compositor. The keeper serves each launch a display of
//! its own through `xwayland-satellite`, a deliberately small Wayland client/XWM which maps each
//! X11 window to an `xdg_toplevel`; that lets `FreeRDP` RAIL windows participate in the ordinary
//! Tilo layout instead of placing a nested Windows desktop in one window of its own. The keeper
//! and its display stay while the desktop restarts in place, so `FreeRDP` stays connected and the
//! program inside Windows keeps its windows.
//!
//! The RDP password arrives at a descriptor the private `--tilo-password-fd=N` argument names,
//! never on a command line, in the environment or in a file: it becomes `FreeRDP`'s standard
//! input (`/from-stdin`) and nothing else this starts inherits it.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The X server the keeper serves a launch's display with.
const SATELLITE: &str = "/usr/bin/xwayland-satellite";
const FREERDP: &str = "/usr/bin/xfreerdp";
const BWRAP: &str = "/usr/bin/bwrap";
const READ_ONLY_DRIVE_PREFIX: &str = "--tilo-read-only-drive=";
const PASSWORD_FD_PREFIX: &str = "--tilo-password-fd=";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("Tilo native Windows surface is unavailable: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, String> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    verify_runtime()?;
    if arguments.as_slice() == [OsStr::new("--probe")] {
        return Ok(0);
    }
    if arguments.is_empty() {
        return Err("no FreeRDP request was supplied".to_string());
    }
    let Private {
        free_rdp,
        read_only_drives,
        password,
    } = split_private_arguments(arguments)?;
    // The keeper serves the display and its cookie; the program never makes either.
    if env::var_os("DISPLAY").is_none() || env::var_os("XAUTHORITY").is_none() {
        return Err("the launch's keeper serves no X11 display".to_string());
    }
    let status = free_rdp_command(&free_rdp, &read_only_drives)
        .env("_JAVA_AWT_WM_NONREPARENTING", "1")
        .stdin(Stdio::from(password))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("could not start xfreerdp: {error}"))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

fn verify_runtime() -> Result<(), String> {
    for binary in [SATELLITE, FREERDP, BWRAP] {
        if !Path::new(binary).is_file() {
            return Err(format!(
                "required runtime component {binary} is not installed"
            ));
        }
    }
    Ok(())
}

/// What the rail takes for itself out of its arguments: the rest is `FreeRDP`'s.
#[derive(Debug)]
struct Private {
    free_rdp: Vec<OsString>,
    read_only_drives: Vec<PathBuf>,
    /// The descriptor the RDP password is read from, close-on-exec: it reaches `FreeRDP` only as
    /// its standard input.
    password: OwnedFd,
}

fn split_private_arguments(arguments: Vec<OsString>) -> Result<Private, String> {
    let mut free_rdp = Vec::new();
    let mut read_only_drives = Vec::new();
    let mut password = None;
    for argument in arguments {
        let text = argument.to_str();
        if let Some(value) = text.and_then(|value| value.strip_prefix(READ_ONLY_DRIVE_PREFIX)) {
            let path = PathBuf::from(value);
            validate_portal_drive(&path)?;
            read_only_drives.push(path);
        } else if let Some(value) = text.and_then(|value| value.strip_prefix(PASSWORD_FD_PREFIX)) {
            if password.is_some() {
                return Err("two RDP password descriptors were supplied".to_string());
            }
            password = Some(take_descriptor(value)?);
        } else {
            free_rdp.push(argument);
        }
    }
    if free_rdp.is_empty() {
        return Err("no FreeRDP request was supplied".to_string());
    }
    Ok(Private {
        free_rdp,
        read_only_drives,
        password: password.ok_or("no RDP password descriptor was supplied")?,
    })
}

/// Take the inherited descriptor `number` names, close-on-exec from now on.
fn take_descriptor(number: &str) -> Result<OwnedFd, String> {
    let fd: RawFd = number
        .parse()
        .ok()
        .filter(|fd| *fd > 2)
        .ok_or("the RDP password descriptor is not a descriptor number")?;
    // SAFETY: F_SETFD on a number this process was handed changes only that descriptor's flags,
    // and fails without effect when it is not open.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(format!(
            "the RDP password descriptor is not open: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the descriptor is open, was handed to this process for this use alone, and nothing
    // else here owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn validate_portal_drive(path: &Path) -> Result<(), String> {
    let lease_root = windows_vm::portal_lease_root();
    if !path.is_absolute() || !path.starts_with(&lease_root) {
        return Err("read-only drive is outside the Tilo portal lease root".to_string());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect portal drive: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("read-only portal drive is not a real directory".to_string());
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("could not resolve portal drive: {error}"))?;
    if canonical != path || !canonical.starts_with(&lease_root) {
        return Err("read-only portal drive escaped its lease".to_string());
    }
    Ok(())
}

fn free_rdp_command(arguments: &[OsString], read_only_drives: &[PathBuf]) -> Command {
    let mut command = if read_only_drives.is_empty() {
        Command::new(FREERDP)
    } else {
        let mut sandbox = Command::new(BWRAP);
        // Preserve the normal RAIL runtime while using the rootless mount
        // namespace as the security boundary for portal read-only decisions.
        sandbox.args(["--die-with-parent", "--new-session", "--bind", "/", "/"]);
        for drive in read_only_drives {
            sandbox.arg("--ro-bind").arg(drive).arg(drive);
        }
        sandbox.arg("--").arg(FREERDP);
        sandbox
    };
    command.args(arguments);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The password descriptor is the rail's own: it leaves `FreeRDP`'s arguments, and no program
    /// the rail starts inherits it but through its standard input.
    #[test]
    fn rail_launches_keep_secrets_and_private_paths_out_of_argv() {
        {
            let (reader, _writer) = std::io::pipe().unwrap();
            let number = std::os::fd::IntoRawFd::into_raw_fd(OwnedFd::from(reader));
            // SAFETY: clears the flag on the descriptor this test owns, as a handed one arrives.
            assert_eq!(unsafe { libc::fcntl(number, libc::F_SETFD, 0) }, 0);
            let private = split_private_arguments(vec![
                "/v:guest".into(),
                format!("{PASSWORD_FD_PREFIX}{number}").into(),
                "/from-stdin".into(),
            ])
            .unwrap();
            assert_eq!(private.free_rdp, ["/v:guest", "/from-stdin"]);
            assert_eq!(std::os::fd::AsRawFd::as_raw_fd(&private.password), number);
            // SAFETY: reads the flags of a descriptor the rail owns.
            let flags = unsafe { libc::fcntl(number, libc::F_GETFD) };
            assert_eq!(
                flags & libc::FD_CLOEXEC,
                libc::FD_CLOEXEC,
                "nothing the rail starts inherits it at its own number"
            );
            assert!(
                split_private_arguments(vec!["/v:guest".into(), "/from-stdin".into()]).is_err(),
                "a launch without its password does not start"
            );
            assert!(
                split_private_arguments(vec![
                    "/v:guest".into(),
                    format!("{PASSWORD_FD_PREFIX}1").into()
                ])
                .is_err(),
                "standard streams are not a password descriptor"
            );
        }
        {
            let drive = windows_vm::portal_lease_root().join("rail-test/TILO01");
            let command = free_rdp_command(&["/v:guest".into()], std::slice::from_ref(&drive));
            assert_eq!(command.get_program(), OsStr::new(BWRAP));
            let arguments = command.get_args().collect::<Vec<_>>();
            assert!(arguments.windows(3).any(|window| {
                window
                    == [
                        OsStr::new("--ro-bind"),
                        drive.as_os_str(),
                        drive.as_os_str(),
                    ]
            }));
            assert!(!arguments.iter().any(|argument| {
                argument
                    .to_string_lossy()
                    .starts_with(READ_ONLY_DRIVE_PREFIX)
            }));
        }
        {
            assert!(
                split_private_arguments(vec![
                    "--tilo-read-only-drive=/home/user".into(),
                    "/v:guest".into(),
                ])
                .is_err()
            );
        }
    }
}
