//! What a session reaches of the machine it draws on: the one answer every
//! service that acts on the machine reads.
//!
//! A Tilo login owns the machine under it: its VPN kill switch configures the
//! firewall, its Bluetooth and network services drive the radios, its power
//! service holds logind. A session that runs *inside* another desktop — a
//! nested session in a window, a test session beside a live one — owns none of
//! that: those daemons belong to the person sitting at the machine, and a
//! session they did not start must never change them. The answer is declared
//! once, by the environment (`TILO_MACHINE`), and inherited by every process
//! the session starts, so the shell, its services, the launcher and the
//! programs it runs all read the same one.
//!
//! Services do not decide this themselves and do not guess it from a display:
//! they ask here, and a [`Reach::Hosted`] session keeps its machine-acting
//! services off rather than pointing them at a host's daemons.

use std::ffi::OsString;

/// The variable a session declares itself with: `owned` for a session that
/// reaches the machine's own daemons, `hosted` for one that must not.
pub const VAR: &str = "TILO_MACHINE";

/// Where Tilo OS's system unit keeps the device daemon's sockets: its own runtime directory,
/// which every account may enter, each connection admitted by the account it comes from.
pub const DAEMON_SOCKETS: &str = "/run/tilo-runtime";
/// The device daemon's native endpoint, in its sockets' directory: what apps, the system's own
/// callers (the sign-in's session hooks) and `tilo-runtime --connect` reach.
pub const DAEMON_NATIVE: &str = "native.sock";
/// The variable naming a device daemon's profile when it is not the account's own: a test's, a VM
/// host's.
pub const DAEMON_PROFILE_VAR: &str = "TILO_RUNTIME_PROFILE";

/// The profile an account's own device daemon keeps its state in, under its home: its store, its
/// device's key and certificates, and the sockets of a profile named apart. Tilo OS's system unit
/// keeps its profile outside every home instead.
#[must_use]
pub fn account_daemon_profile() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    let suffix = "Library/Application Support/Tilo/runtime";
    #[cfg(target_os = "windows")]
    let suffix = "AppData/Local/Tilo/runtime";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let suffix = ".local/state/tilo/runtime";
    Some(super::home()?.join(suffix))
}

/// The unit Tilo OS runs the device daemon from boot with, as the system's own account.
pub const DAEMON_SYSTEM_UNIT: &str = "/usr/lib/systemd/system/tilo-runtime.service";

/// Whether this computer's daemon is the system's: Tilo OS runs it from boot as its own
/// account for every person of the computer, where other computers run it as the person.
#[must_use]
pub fn system_daemon() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new(DAEMON_SYSTEM_UNIT).is_file()
}

/// Where this computer's daemon keeps its sockets: the system unit's runtime directory under
/// Tilo OS; else, for the account `uid` runs it as, `$XDG_RUNTIME_DIR/tilo` (the one systemd
/// names by the account when the variable is not said, over ssh or from cron) on Linux, and the
/// account's temporary directory (`/var/folders/…/T/tilo`) on macOS. Neither made nor checked:
/// the daemon makes it, private to the account.
///
/// # Errors
/// macOS names no temporary directory for this account.
#[cfg(unix)]
pub fn daemon_sockets(uid: u32) -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    if system_daemon() {
        return Ok(PathBuf::from(DAEMON_SOCKETS));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = uid;
        Ok(darwin_temp_dir()?.join("tilo"))
    }
    #[cfg(not(target_os = "macos"))]
    Ok(crate::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|runtime| runtime.is_absolute() && runtime.is_dir())
        .or_else(|| Some(PathBuf::from(format!("/run/user/{uid}"))).filter(|run| run.is_dir()))
        .map_or_else(
            || std::env::temp_dir().join(format!("tilo-{uid}")),
            |runtime| runtime.join("tilo"),
        ))
}

/// The native endpoint of the daemon this account reaches ([`daemon_sockets`] of its own
/// account).
///
/// # Errors
/// As [`daemon_sockets`].
#[cfg(unix)]
pub fn daemon_native() -> Result<std::path::PathBuf, String> {
    Ok(daemon_sockets(rustix::process::geteuid().as_raw())?.join(DAEMON_NATIVE))
}

/// Where this computer's daemon names its pipes on Windows: the `sockets` folder of the account's
/// own daemon profile. A pipe lives in the system's pipe namespace, not in a folder; its name is
/// made from this path, and the folder holds the daemon's lock beside them. Neither made nor
/// checked: the daemon makes it, private to the account.
///
/// # Errors
/// The account names no home.
#[cfg(windows)]
pub fn daemon_sockets() -> Result<std::path::PathBuf, String> {
    account_daemon_profile()
        .map(|profile| profile.join("sockets"))
        .ok_or_else(|| "this account names no home folder (USERPROFILE)".to_owned())
}

/// The native endpoint of the daemon this account reaches ([`daemon_sockets`]).
///
/// # Errors
/// As [`daemon_sockets`].
#[cfg(windows)]
pub fn daemon_native() -> Result<std::path::PathBuf, String> {
    Ok(daemon_sockets()?.join(DAEMON_NATIVE))
}

/// This account's temporary directory on macOS (`/var/folders/…/T/`), as the system names it:
/// private to the account, and short whatever the home folder is.
#[cfg(target_os = "macos")]
fn darwin_temp_dir() -> Result<std::path::PathBuf, String> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut buf = vec![0_u8; 1024];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most that many, its
    // NUL included.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len == 0 || len > buf.len() {
        return Err(format!(
            "this account has no temporary directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    buf.truncate(len.saturating_sub(1));
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// Whether a session reaches the machine's own daemons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// This session is the login that owns the machine: its services drive the
    /// machine's daemons.
    Owned,
    /// This session runs inside another desktop, or is a test beside one: the
    /// machine's daemons are not its to change.
    Hosted,
}

impl Reach {
    /// The reach `TILO_MACHINE` names: `hosted` for a hosted session, else
    /// [`Owned`](Self::Owned). A session that says nothing owns the machine it
    /// was started on, so a login needs no declaration and only a session that
    /// must keep away from the machine has to say so.
    #[must_use]
    pub fn named(value: Option<&OsString>) -> Self {
        match value.and_then(|value| value.to_str()).map(str::trim) {
            Some("hosted") => Self::Hosted,
            _ => Self::Owned,
        }
    }

    /// Whether the machine's own daemons are this session's to drive.
    #[must_use]
    pub const fn is_owned(self) -> bool {
        matches!(self, Self::Owned)
    }
}

/// What this process's session reaches of the machine.
#[must_use]
pub fn reach() -> Reach {
    Reach::named(crate::var(VAR).as_ref())
}

/// Whether this session may drive the machine's own daemons: the one answer
/// the services that act on the machine read.
#[must_use]
pub fn owns_the_machine() -> bool {
    reach().is_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> OsString {
        OsString::from(text)
    }

    #[test]
    fn a_session_that_says_nothing_owns_its_machine() {
        assert_eq!(Reach::named(None), Reach::Owned);
        assert_eq!(Reach::named(Some(&value(""))), Reach::Owned);
        assert_eq!(Reach::named(Some(&value("owned"))), Reach::Owned);
        assert_eq!(Reach::named(Some(&value("something else"))), Reach::Owned);
    }

    #[test]
    fn a_hosted_session_reaches_no_machine_of_its_own() {
        assert_eq!(Reach::named(Some(&value("hosted"))), Reach::Hosted);
        assert_eq!(Reach::named(Some(&value(" hosted "))), Reach::Hosted);
        assert!(!Reach::Hosted.is_owned());
        assert!(Reach::Owned.is_owned());
    }
}
