//! Run `FreeRDP`'s X11 RAIL client through one private rootless Xwayland bridge.
//!
//! Tilo does not implement an XWM in its compositor. `xwayland-satellite` is a
//! deliberately small Wayland client/XWM which maps each X11 window to an
//! `xdg_toplevel`; that lets `FreeRDP` RAIL windows participate in the ordinary
//! Tilo layout instead of placing a nested Windows desktop in one host window.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng as _;

const SATELLITE: &str = "/usr/bin/xwayland-satellite";
const FREERDP: &str = "/usr/bin/xfreerdp";
const BWRAP: &str = "/usr/bin/bwrap";
const READ_ONLY_DRIVE_PREFIX: &str = "--tilo-read-only-drive=";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const DISPLAY_ATTEMPTS: usize = 64;

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
    let (arguments, read_only_drives) = split_private_arguments(arguments)?;

    let runtime = private_runtime_root()?;
    let (display_number, reservation) = reserve_display(&runtime)?;
    let launch_root = PrivateLaunchRoot::new(private_launch_root(&runtime)?);
    let authority = launch_root.path().join("Xauthority");
    write_xauthority(&authority, display_number)?;

    let display = format!(":{display_number}");
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display_number}"));
    let mut satellite = Command::new(SATELLITE)
        .arg(&display)
        .args(["-auth", authority.to_string_lossy().as_ref()])
        .args(["-nolisten", "tcp", "-terminate"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("could not start xwayland-satellite: {error}"))?;

    let result = (|| {
        wait_for_xwayland(&mut satellite, &socket)?;
        let mut command = free_rdp_command(&arguments, &read_only_drives);
        let status = command
            .env("DISPLAY", &display)
            .env("XAUTHORITY", &authority)
            // Harmless for FreeRDP and required by Java child windows that a
            // published application may create in the RAIL session.
            .env("_JAVA_AWT_WM_NONREPARENTING", "1")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| format!("could not start xfreerdp: {error}"))?;
        Ok(status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1))
    })();

    stop_child(&mut satellite);
    drop(reservation);
    result
}

fn verify_runtime() -> Result<(), String> {
    for binary in [SATELLITE, FREERDP, BWRAP] {
        if !Path::new(binary).is_file() {
            return Err(format!(
                "required runtime component {binary} is not installed"
            ));
        }
    }
    let wayland = env::var_os("WAYLAND_DISPLAY")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "WAYLAND_DISPLAY is not set".to_string())?;
    let runtime = runtime_directory()?;
    let wayland = Path::new(&wayland);
    let socket = if wayland.is_absolute() {
        wayland.to_path_buf()
    } else {
        runtime.join(wayland)
    };
    if !socket.exists() {
        return Err("the Tilo Wayland socket is not available".to_string());
    }
    Ok(())
}

fn split_private_arguments(
    arguments: Vec<std::ffi::OsString>,
) -> Result<(Vec<std::ffi::OsString>, Vec<PathBuf>), String> {
    let mut free_rdp = Vec::new();
    let mut read_only = Vec::new();
    for argument in arguments {
        if let Some(value) = argument
            .to_str()
            .and_then(|value| value.strip_prefix(READ_ONLY_DRIVE_PREFIX))
        {
            let path = PathBuf::from(value);
            validate_portal_drive(&path)?;
            read_only.push(path);
        } else {
            free_rdp.push(argument);
        }
    }
    if free_rdp.is_empty() {
        return Err("no FreeRDP request was supplied".to_string());
    }
    Ok((free_rdp, read_only))
}

fn validate_portal_drive(path: &Path) -> Result<(), String> {
    // Keep this helper's trust root identical to the descriptor store's,
    // including the headless fallback.  A helper that validates a different
    // root would turn a valid opaque portal lease into an ambient path check.
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

fn free_rdp_command(arguments: &[std::ffi::OsString], read_only_drives: &[PathBuf]) -> Command {
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

fn runtime_directory() -> Result<PathBuf, String> {
    tilo_paths::xdg::runtime_dir()
        .ok_or_else(|| "XDG_RUNTIME_DIR is not an absolute path".to_string())
}

fn private_runtime_root() -> Result<PathBuf, String> {
    let root = runtime_directory()?.join("tilo-windows-vm/rail");
    fs::create_dir_all(&root)
        .map_err(|error| format!("could not create the private RAIL runtime: {error}"))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not protect the private RAIL runtime: {error}"))?;
    Ok(root)
}

fn private_launch_root(runtime: &Path) -> Result<PathBuf, String> {
    for _ in 0..DISPLAY_ATTEMPTS {
        let mut nonce = [0_u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
        let candidate = runtime.join(format!("launch-{}", hex::encode(nonce)));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).map_err(
                    |error| format!("could not protect the private launch directory: {error}"),
                )?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "could not create a private launch directory: {error}"
                ));
            }
        }
    }
    Err("could not allocate a private launch directory".to_string())
}

fn reserve_display(runtime: &Path) -> Result<(u16, DisplayReservation), String> {
    let mut random = [0_u8; 2];
    UnwrapErr(SysRng).fill_bytes(&mut random);
    let first = 1000_u16.saturating_add(u16::from_be_bytes(random) % 20_000);
    for offset in 0..DISPLAY_ATTEMPTS {
        let display = first.wrapping_add(u16::try_from(offset).unwrap_or(0));
        let x_socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
        let x_lock = PathBuf::from(format!("/tmp/.X{display}-lock"));
        if x_socket.exists() || x_lock.exists() {
            continue;
        }
        let path = runtime.join(format!("display-{display}.lock"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((display, DisplayReservation { path, _file: file })),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("could not reserve an X display: {error}")),
        }
    }
    Err("no private X display number is available".to_string())
}

/// Write one `FamilyWild` MIT-MAGIC-COOKIE-1 record without putting the cookie
/// in argv. Xauthority uses network byte order and length-prefixed fields.
fn write_xauthority(path: &Path, display: u16) -> Result<(), String> {
    let mut cookie = [0_u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut cookie);
    let mut contents = Vec::with_capacity(64);
    push_u16(&mut contents, u16::MAX); // FamilyWild
    push_field(&mut contents, &[]);
    push_field(&mut contents, display.to_string().as_bytes());
    push_field(&mut contents, b"MIT-MAGIC-COOKIE-1");
    push_field(&mut contents, &cookie);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("could not create private X authority: {error}"))?;
    file.write_all(&contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write private X authority: {error}"))
}

fn push_field(buffer: &mut Vec<u8>, field: &[u8]) {
    push_u16(buffer, u16::try_from(field.len()).unwrap_or(u16::MAX));
    buffer.extend_from_slice(field);
}

fn push_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_be_bytes());
}

fn wait_for_xwayland(child: &mut Child, socket: &Path) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(STARTUP_TIMEOUT)
        .ok_or_else(|| "could not calculate the Xwayland startup deadline".to_string())?;
    loop {
        if socket.exists() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("could not inspect xwayland-satellite: {error}"))?
        {
            return Err(format!(
                "xwayland-satellite exited during startup ({status})"
            ));
        }
        if Instant::now() >= deadline {
            return Err("xwayland-satellite did not create its display in time".to_string());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn stop_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn remove_private_launch_root(root: &Path) {
    let _ = fs::remove_file(root.join("Xauthority"));
    let _ = fs::remove_dir(root);
}

struct PrivateLaunchRoot(PathBuf);

impl PrivateLaunchRoot {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateLaunchRoot {
    fn drop(&mut self) {
        remove_private_launch_root(&self.0);
    }
}

struct DisplayReservation {
    path: PathBuf,
    _file: File,
}

impl Drop for DisplayReservation {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_launches_keep_cookies_and_private_paths_out_of_argv() {
        // Phase `xauthority_record_never_places_cookie_in_command_arguments`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("authority");
            write_xauthority(&path, 1234).unwrap();
            let bytes = fs::read(path).unwrap();
            assert_eq!(&bytes[0..2], &u16::MAX.to_be_bytes());
            assert!(bytes.windows(4).any(|window| window == b"1234"));
            assert!(
                bytes
                    .windows(b"MIT-MAGIC-COOKIE-1".len())
                    .any(|window| window == b"MIT-MAGIC-COOKIE-1")
            );
            assert_eq!(bytes.len(), 48);
        }
        // Phase `launch_directory_cleanup_is_narrow`.
        {
            let directory = tempfile::tempdir().unwrap();
            let launch = directory.path().join("launch-test");
            fs::create_dir(&launch).unwrap();
            fs::write(launch.join("Xauthority"), b"test").unwrap();
            remove_private_launch_root(&launch);
            assert!(!launch.exists());
            assert!(directory.path().exists());
        }
        // Phase `private_read_only_argument_never_reaches_freerdp`.
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
        // Phase `caller_cannot_name_an_ambient_read_only_path`.
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
