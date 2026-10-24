//! Who may ask: the compositor of the person at the seat, and nobody else.
//!
//! Changing a machine's routes, DNS and firewall is the shell's to ask, on
//! the person's behalf: no app, script or other account may, even of the
//! same person. The shell is recognised as the NTFS mount service recognises
//! it ([`seat_peer`]).

pub use seat_peer::{PeerFacts, Seat};

use super::Caller;

/// Decide whether `peer` may ask anything at all.
///
/// # Errors
///
/// Why it is turned away.
pub fn decide(seat: &Seat, peer: &PeerFacts) -> Result<Caller, String> {
    if seat.is_seat_shell(peer) {
        Ok(Caller { uid: peer.uid })
    } else {
        Err("only the desktop of the person at this computer may change its routes".into())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn only_the_seats_shell_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seat0"), "ACTIVE_UID=1000\n").unwrap();
        let seat = Seat {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat: dir.path().join("seat0"),
            users: dir.path().join("users"),
        };
        let shell = PeerFacts {
            uid: 1000,
            exe: Some(PathBuf::from("/usr/bin/shell")),
            host_namespaces: true,
        };
        assert_eq!(decide(&seat, &shell), Ok(Caller { uid: 1000 }));
        let app = PeerFacts {
            host_namespaces: false,
            ..shell.clone()
        };
        assert!(decide(&seat, &app).is_err(), "a sandboxed app");
        let other = PeerFacts {
            uid: 1001,
            ..shell.clone()
        };
        assert!(decide(&seat, &other).is_err(), "another account's shell");
        let script = PeerFacts {
            exe: Some(PathBuf::from("/usr/bin/bash")),
            ..shell
        };
        assert!(decide(&seat, &script).is_err(), "the person's own script");
    }
}
