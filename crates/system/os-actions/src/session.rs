//! The session lifecycle actions shell chrome may take, kept here so UI code cannot
//! construct an arbitrary command.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_power_mgmt::LogindManager;

const LOGOUT_REQUEST_ENV: &str = "TILO_SESSION_LOGOUT_REQUEST";
/// GDM's local display factory: the only display manager with a user-switch call an ordinary local user
/// may make.
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

/// Only GDM exposes that on the system bus; greetd runs one session per seat and has no such call.
pub fn user_switching_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        bus_name_owned(GDM_BUS_NAME).unwrap_or_else(|error| {
            tracing::debug!(target: "tilo::session", %error, "cannot ask the bus for a greeter");
            false
        })
    })
}

fn bus_name_owned(name: &str) -> Result<bool, String> {
    crate::block_on(async {
        let bus = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        let reply = bus
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &name,
            )
            .await
            .map_err(|e| format!("NameHasOwner {name}: {e}"))?;
        reply
            .body()
            .deserialize::<bool>()
            .map_err(|e| format!("NameHasOwner {name}: {e}"))
    })
}

fn open_greeter() -> Result<(), String> {
    crate::block_on(async {
        let bus = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        bus.call_method(
            Some(GDM_BUS_NAME),
            "/org/gnome/DisplayManager/LocalDisplayFactory",
            Some("org.gnome.DisplayManager.LocalDisplayFactory"),
            "CreateTransientDisplay",
            &(),
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("{GDM_BUS_NAME} refused to open a greeter: {e}"))
    })
}

/// Errors when logind cannot be reached, does not authorise the action, or the command fails.
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
        // A greeter is a display-manager session, not a logind session id: ask GDM for its greeter,
        // which it permits ordinary local users to do.
        SessionAction::SwitchUser if !user_switching_supported() => {
            Err("This computer's login screen cannot switch users".into())
        }
        SessionAction::SwitchUser => open_greeter(),
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

/// Opened once and kept: brightness keys auto-repeat, and a connection per call would cost a bus
/// handshake each.
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

/// Asks the display-manager wrapper for an orderly logout over its private channel. Synchronous on
/// purpose: it creates and writes one local file and publishes an in-process edge, and on a worker the
/// finishing would not wake an otherwise idle compositor.
pub fn request_logout() -> Result<(), String> {
    request_wrapper_logout_at(&logout_request_path()?)?;
    // The wrapper owns teardown but cannot see the request until this compositor exits, so an
    // in-process edge tells the frame loop; the request file remains the cross-process proof.
    LOGOUT_REQUESTED.store(true, Ordering::Release);
    tracing::info!(target: "tilo::session", "logout requested through session wrapper");
    Ok(())
}

/// The compositor calls this from its owning thread; the edge avoids a filesystem poll per frame.
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
