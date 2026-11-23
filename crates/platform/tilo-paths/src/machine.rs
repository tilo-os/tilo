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
