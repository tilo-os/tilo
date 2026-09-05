//! Session-wide rootless Xwayland compatibility via `xwayland-satellite`.
//!
//! qOS remains a Wayland compositor and does not grow an in-process XWM. The
//! satellite is itself a Wayland client and maps each legacy X11 window to an
//! ordinary `xdg_toplevel`, so existing host applications participate in the
//! same layout. Failure is non-fatal: native Wayland software remains usable.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{
    FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore as _};

const DISPLAY_ATTEMPTS: usize = 64;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Keep the compatibility server out of the security-hardened qOS image.
pub(crate) const fn enabled_for(mode: super::InstallMode) -> bool {
    matches!(mode, super::InstallMode::HostDesktop)
}

pub(crate) struct SessionXwayland {
    child: Child,
    runtime: PathBuf,
    authority: PathBuf,
    reservation: PathBuf,
}

impl SessionXwayland {
    pub(crate) fn start(wayland_socket: &OsStr) -> Result<Self, String> {
        let binary = satellite_binary().ok_or_else(|| {
            "xwayland-satellite is not installed; X11-only host applications are unavailable"
                .to_string()
        })?;
        let runtime_root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| "XDG_RUNTIME_DIR is not an absolute path".to_string())?;
        let runtime = runtime_root.join("qos-session/xwayland");
        fs::create_dir_all(&runtime)
            .map_err(|error| format!("creating {}: {error}", runtime.display()))?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protecting {}: {error}", runtime.display()))?;

        let (display_number, reservation_file, reservation) = reserve_display(&runtime)?;
        let authority = runtime.join(format!("Xauthority-{display_number}"));
        if let Err(error) = write_xauthority(&authority, display_number) {
            drop(reservation_file);
            let _ = fs::remove_file(&reservation);
            return Err(error);
        }
        let x_display = format!(":{display_number}");
        let keyboard = super::backend::drm::input_config::KeyboardSettings::load();

        let mut command = Command::new(&binary);
        command
            .arg(&x_display)
            .args(["-auth", authority.to_string_lossy().as_ref()])
            .args(["-nolisten", "tcp"])
            .env("WAYLAND_DISPLAY", wayland_socket)
            // Xwayland compiles its own XKB map before it receives any input.
            // In a display-manager session there need not be PAM-provided
            // XKB_DEFAULT_* variables, so inheriting the process environment
            // silently gives every legacy host app US regardless of the qOS
            // seat keymap. Keep both sides on the same validated settings.
            .env("XKB_DEFAULT_LAYOUT", &keyboard.layouts)
            .env("XKB_DEFAULT_VARIANT", &keyboard.variants)
            .env(
                "XKB_DEFAULT_OPTIONS",
                keyboard.options.as_deref().unwrap_or(""),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // SAFETY: `pre_exec` runs after fork and before exec. `prctl` is one
        // async-signal-safe syscall and captures no heap-backed state. The
        // death signal prevents a compositor crash from leaving an X server
        // accepting connections after its Wayland parent is gone.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&authority);
                let _ = fs::remove_file(&reservation);
                return Err(format!("starting {}: {error}", binary.display()));
            }
        };
        drop(reservation_file);

        if let Err(error) = wait_until_ready(&mut child, display_number) {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
            let _ = fs::remove_file(&authority);
            let _ = fs::remove_file(&reservation);
            return Err(error);
        }

        crate::shell::host_apps::set_x11_session(Some(crate::shell::host_apps::X11Session {
            display: x_display.clone(),
            authority: authority.clone(),
        }));
        tracing::info!(display = %x_display, binary = %binary.display(), "rootless Xwayland compatibility bridge starting");
        Ok(Self {
            child,
            runtime,
            authority,
            reservation,
        })
    }
}

fn wait_until_ready(child: &mut Child, display: u16) -> Result<(), String> {
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "xwayland-satellite exited before opening display :{display} ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("checking xwayland-satellite: {error}")),
        }
        if x_socket_ready(&socket) {
            return Ok(());
        }
        if started.elapsed() >= STARTUP_TIMEOUT {
            return Err(format!(
                "xwayland-satellite did not open display :{display} within {} seconds",
                STARTUP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Refuse a symlink, regular file, or another user's socket at the globally
/// shared X11 path. The display number is reserved inside this user's runtime
/// directory, but `/tmp/.X11-unix` is intentionally shared by every login.
fn x_socket_ready(path: &Path) -> bool {
    // SAFETY: `geteuid` is a side-effect-free process credential query.
    let current_uid = unsafe { libc::geteuid() };
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.uid() == current_uid)
}

impl Drop for SessionXwayland {
    fn drop(&mut self) {
        crate::shell::host_apps::set_x11_session(None);
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.authority);
        let _ = fs::remove_file(&self.reservation);
        let _ = fs::remove_dir(&self.runtime);
    }
}

fn satellite_binary() -> Option<PathBuf> {
    let fixed = PathBuf::from("/usr/bin/xwayland-satellite");
    if fixed.is_file() {
        return Some(fixed);
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("xwayland-satellite"))
            .find(|candidate| candidate.is_file())
    })
}

fn reserve_display(runtime: &Path) -> Result<(u16, File, PathBuf), String> {
    let mut random = [0_u8; 2];
    OsRng.fill_bytes(&mut random);
    let first = 1000_u16.saturating_add(u16::from_be_bytes(random) % 20_000);
    for offset in 0..DISPLAY_ATTEMPTS {
        let display = first.wrapping_add(u16::try_from(offset).unwrap_or(0));
        if Path::new(&format!("/tmp/.X11-unix/X{display}")).exists()
            || Path::new(&format!("/tmp/.X{display}-lock")).exists()
        {
            continue;
        }
        let reservation = runtime.join(format!("display-{display}.lock"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&reservation)
        {
            Ok(file) => return Ok((display, file, reservation)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("reserving X display: {error}")),
        }
    }
    Err("no unused X display number is available".to_string())
}

fn write_xauthority(path: &Path, display: u16) -> Result<(), String> {
    let mut cookie = [0_u8; 16];
    OsRng.fill_bytes(&mut cookie);
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
        .map_err(|error| format!("creating Xauthority: {error}"))?;
    file.write_all(&contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("writing Xauthority: {error}"))
}

fn push_field(buffer: &mut Vec<u8>, field: &[u8]) {
    push_u16(buffer, u16::try_from(field.len()).unwrap_or(u16::MAX));
    buffer.extend_from_slice(field);
}

fn push_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn compatibility_is_confined_to_add_on_desktop_sessions() {
        assert!(enabled_for(super::super::InstallMode::HostDesktop));
        assert!(!enabled_for(super::super::InstallMode::FullSystem));
    }

    #[test]
    fn keyboard_settings_retain_layout_variant_and_options_for_xwayland() {
        let section = serde_json::json!({
            "keyboard_layout": "de:nodeadkeys",
            "keyboard_layouts": ["de:nodeadkeys"],
            "compose_key": "rwin"
        })
        .as_object()
        .unwrap()
        .clone();
        let settings =
            crate::compositor::backend::drm::input_config::KeyboardSettings::from_section(&section);
        assert_eq!(settings.layouts, "de");
        assert_eq!(settings.variants, "nodeadkeys");
        assert_eq!(settings.options.as_deref(), Some("compose:rwin"));
    }

    #[test]
    fn authority_is_private_and_contains_no_text_cookie() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Xauthority");
        write_xauthority(&path, 1234).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let bytes = fs::read(path).unwrap();
        assert!(bytes.windows(4).any(|window| window == b"1234"));
        assert!(
            bytes
                .windows(b"MIT-MAGIC-COOKIE-1".len())
                .any(|window| window == b"MIT-MAGIC-COOKIE-1")
        );
    }

    #[test]
    fn readiness_requires_an_owned_unix_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("X1234");
        fs::write(&path, b"not a socket").unwrap();
        assert!(!x_socket_ready(&path));
        fs::remove_file(&path).unwrap();

        let listener = UnixListener::bind(&path).unwrap();
        assert!(x_socket_ready(&path));
        drop(listener);
    }
}
