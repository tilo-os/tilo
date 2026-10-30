//! Narrow session lifecycle actions used by shell chrome.
//!
//! Keeping these here means UI code cannot construct an arbitrary command.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_power_mgmt::LogindManager;

use crate::process::run;

const LOGOUT_REQUEST_ENV: &str = "TILO_SESSION_LOGOUT_REQUEST";
/// The only display manager with a user-switch call Tilo can make: GDM's
/// local display factory, which it permits to ordinary local users.
const GDM_BUS_NAME: &str = "org.gnome.DisplayManager";
static LOGOUT_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    Logout,
    SwitchUser,
    Suspend,
    Restart,
    ShutDown,
}

/// Whether the running display manager can open a greeter beside this
/// session. Only GDM exposes that on the system bus; greetd (the Tilo image's
/// login manager) runs one session per seat and has no such call. Probed once
/// per process: the display manager does not change under a running session.
pub fn user_switching_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| run("busctl", &["--system", "status", GDM_BUS_NAME]).is_ok())
}

/// Perform `action`.
///
/// # Errors
///
/// Returns an error message if logind cannot be reached, does not authorise the action, or
/// the command otherwise fails, or when switching users on a display manager that
/// cannot (see [`user_switching_supported`]).
pub fn perform_session_action(action: SessionAction) -> Result<(), String> {
    match action {
        SessionAction::Lock => crate::block_on(async {
            logind()
                .await?
                .lock_session()
                .await
                .map_err(|e| format!("logind Lock: {e}"))
        }),
        SessionAction::Logout => request_logout(),
        // A greeter is a display-manager session, not a magic logind session
        // id. Ask the active display manager to create/activate its greeter.
        // GDM deliberately permits this exact method to ordinary local users
        // in `/usr/share/dbus-1/system.d/gdm.conf`.
        SessionAction::SwitchUser if !user_switching_supported() => {
            Err("This computer's login screen cannot switch users".into())
        }
        SessionAction::SwitchUser => run(
            "busctl",
            &[
                "call",
                GDM_BUS_NAME,
                "/org/gnome/DisplayManager/LocalDisplayFactory",
                "org.gnome.DisplayManager.LocalDisplayFactory",
                "CreateTransientDisplay",
            ],
        ),
        // Interactive, as `systemctl` asks: polkit may prompt the person.
        SessionAction::Suspend => crate::block_on(async {
            logind()
                .await?
                .suspend(true)
                .await
                .map_err(|e| format!("logind Suspend: {e}"))
        }),
        SessionAction::Restart => crate::block_on(async {
            logind()
                .await?
                .reboot(true)
                .await
                .map_err(|e| format!("logind Reboot: {e}"))
        }),
        SessionAction::ShutDown => crate::block_on(async {
            logind()
                .await?
                .power_off(true)
                .await
                .map_err(|e| format!("logind PowerOff: {e}"))
        }),
    }
}

/// The process's handle on logind, opened once and kept: brightness keys
/// auto-repeat, and a connection per call would cost a bus handshake each.
pub(crate) async fn logind() -> Result<LogindManager, String> {
    static LOGIND: std::sync::Mutex<Option<LogindManager>> = std::sync::Mutex::new(None);
    if let Some(logind) = LOGIND.lock().ok().and_then(|slot| slot.clone()) {
        return Ok(logind);
    }
    let logind = LogindManager::connect()
        .await
        .map_err(|e| format!("cannot reach logind on the system bus: {e}"))?;
    if let Ok(mut slot) = LOGIND.lock() {
        *slot = Some(logind.clone());
    }
    Ok(logind)
}

fn logout_request_path() -> Result<PathBuf, String> {
    let path = std::env::var_os(LOGOUT_REQUEST_ENV).ok_or_else(|| {
        "Tilo session wrapper did not provide a logout request channel".to_string()
    })?;
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("Tilo session logout request path is not absolute".into());
    }
    Ok(path)
}

fn request_wrapper_logout_at(path: &Path) -> Result<(), String> {
    let mut request = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("creating Tilo logout request: {error}"))?;
    request
        .write_all(b"logout\n")
        .map_err(|error| format!("writing Tilo logout request: {error}"))
}

/// Request an orderly compositor-owned logout through the display-manager
/// wrapper's private channel.
///
/// This is intentionally synchronous: it only creates and writes one private
/// local file, then publishes an in-process edge. Running it on a worker can
/// strand an otherwise-idle compositor because completing the worker does not
/// itself wake the compositor event loop.
///
/// # Errors
///
/// Returns a description when the request path cannot be resolved or the
/// request file cannot be written.
pub fn request_logout() -> Result<(), String> {
    request_wrapper_logout_at(&logout_request_path()?)?;
    // The wrapper owns teardown, but it cannot observe the request until this
    // compositor exits. Publish an in-process edge for the native frame loop;
    // the request file remains the cross-process proof that this was an
    // intentional logout rather than a compositor crash.
    LOGOUT_REQUESTED.store(true, Ordering::Release);
    tracing::info!(target: "tilo::session", "logout requested through session wrapper");
    Ok(())
}

/// Consume the process-local logout edge written by [`perform_session_action`].
///
/// The compositor calls this from its owning thread. Keeping the edge here
/// avoids a per-frame filesystem poll while still letting the wrapper perform
/// orderly target and activation-environment cleanup after the shell exits.
#[must_use]
pub fn take_logout_request() -> bool {
    LOGOUT_REQUESTED.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_uses_the_private_request_path() {
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("logout-request");
            request_wrapper_logout_at(&path).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "logout\n");
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(request_wrapper_logout_at(&path).is_err());
        }
        {
            LOGOUT_REQUESTED.store(true, Ordering::Release);
            assert!(take_logout_request());
            assert!(!take_logout_request());
        }
    }
}
