//! Who may ask: the compositor of the person logged in, and nobody else.

pub use peer_facts::{LogindSeat, Namespaces, PeerFacts};

use super::Caller;

pub fn decide(seat: &LogindSeat, peer: &PeerFacts) -> Result<Caller, String> {
    if seat.is_active_session_shell(peer) {
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
    fn only_the_active_sessions_shell_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seat0"), "ACTIVE_UID=1000\n").unwrap();
        let seat = LogindSeat {
            trusted_ui: PathBuf::from("/usr/bin/tilo-session"),
            seat: dir.path().join("seat0"),
            users: dir.path().join("users"),
        };
        let shell = PeerFacts {
            uid: 1000,
            pid: Some(1),
            exe: Some(PathBuf::from("/usr/bin/tilo-session")),
            namespaces: Namespaces::Shared,
        };
        assert_eq!(decide(&seat, &shell), Ok(Caller { uid: 1000 }));
        let app = PeerFacts {
            namespaces: Namespaces::Other,
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
