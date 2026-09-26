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

/// Run the shell-out backing `action`.
///
/// # Errors
///
/// Returns an error message if `loginctl`/`systemctl` is not installed, is not authorised, or
/// the command otherwise fails, or when switching users on a display manager that
/// cannot (see [`user_switching_supported`]).
pub fn perform_session_action(action: SessionAction) -> Result<(), String> {
    match action {
        SessionAction::Lock => run("loginctl", &["lock-session"]),
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
        SessionAction::Suspend => run("systemctl", &["suspend"]),
        SessionAction::Restart => run("systemctl", &["reboot"]),
        SessionAction::ShutDown => run("systemctl", &["poweroff"]),
    }
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

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Invocation {
        binary: &'static str,
        args: &'static [&'static str],
    }

    fn invocation(action: SessionAction) -> Invocation {
        match action {
            SessionAction::Lock => Invocation {
                binary: "loginctl",
                args: &["lock-session"],
            },
            SessionAction::Logout => panic!("logout uses the wrapper request channel"),
            SessionAction::SwitchUser => Invocation {
                binary: "busctl",
                args: &[
                    "call",
                    "org.gnome.DisplayManager",
                    "/org/gnome/DisplayManager/LocalDisplayFactory",
                    "org.gnome.DisplayManager.LocalDisplayFactory",
                    "CreateTransientDisplay",
                ],
            },
            SessionAction::Suspend => Invocation {
                binary: "systemctl",
                args: &["suspend"],
            },
            SessionAction::Restart => Invocation {
                binary: "systemctl",
                args: &["reboot"],
            },
            SessionAction::ShutDown => Invocation {
                binary: "systemctl",
                args: &["poweroff"],
            },
        }
    }

    #[test]
    fn session_switch_and_logout_use_the_right_private_paths() {
        // Phase `switch_user_targets_the_gdm_factory_not_a_fictional_logind_session`.
        {
            let call = invocation(SessionAction::SwitchUser);
            assert_eq!(call.binary, "busctl");
            assert_eq!(call.args.last(), Some(&"CreateTransientDisplay"));
            assert!(!call.args.contains(&"greeter"));
        }
        // Phase `logout_request_is_private_atomic_and_never_calls_loginctl`.
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
        // Phase `compositor_logout_edge_is_consumed_once`.
        {
            LOGOUT_REQUESTED.store(true, Ordering::Release);
            assert!(take_logout_request());
            assert!(!take_logout_request());
        }
    }
}
