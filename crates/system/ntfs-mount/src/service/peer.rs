//! Who is on the other end of a connection.
//!
//! Every caller must be the person at this machine's seat. Among their
//! processes, only the compositor may open drives: it is the one that draws
//! the consent prompt, which no app can draw or fake. The compositor is
//! recognised by the program it runs, read through a pidfd so a recycled
//! process id cannot impersonate it, and by sharing this service's user and
//! mount namespaces, which no sandboxed app does. `kernel.yama.ptrace_scope`
//! is 3 on Tilo, so no process can be steered from outside once running.

use std::path::PathBuf;

use super::Caller;

pub use seat_peer::PeerFacts;
#[cfg(target_os = "linux")]
pub use seat_peer::facts;

/// Where the facts about callers come from.
#[derive(Clone, Debug)]
pub struct PeerPolicy {
    /// The compositor's executable.
    pub trusted_ui: PathBuf,
    /// logind's record of the seat (`ACTIVE_UID=`).
    pub seat: PathBuf,
    /// logind's per-user records, present while the user has a session.
    pub users: PathBuf,
}

impl Default for PeerPolicy {
    fn default() -> Self {
        let seat = seat_peer::Seat::default();
        Self {
            trusted_ui: seat.trusted_ui,
            seat: seat.seat,
            users: seat.users,
        }
    }
}

impl PeerPolicy {
    fn seat(&self) -> seat_peer::Seat {
        seat_peer::Seat {
            trusted_ui: self.trusted_ui.clone(),
            seat: self.seat.clone(),
            users: self.users.clone(),
        }
    }

    /// The uid logind reports at the seat.
    pub fn active_uid(&self) -> Option<u32> {
        seat_peer::active_uid(&self.seat)
    }

    pub fn has_session(&self, uid: u32) -> bool {
        seat_peer::has_session(&self.users, uid)
    }

    /// Decide who `peer` is, or why it is turned away.
    pub fn decide(&self, peer: &PeerFacts) -> Result<Caller, String> {
        if self.active_uid() != Some(peer.uid) {
            return Err("only the person at this computer may use its drives".into());
        }
        Ok(if self.seat().is_trusted_ui(peer) {
            Caller::Shell { uid: peer.uid }
        } else {
            Caller::Person { uid: peer.uid }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn policy(dir: &Path, active: Option<u32>) -> PeerPolicy {
        let seat = dir.join("seat0");
        if let Some(uid) = active {
            fs::write(&seat, format!("IS_SEAT0=1\nACTIVE=c2\nACTIVE_UID={uid}\n")).unwrap();
        }
        PeerPolicy {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat,
            users: dir.join("users"),
        }
    }

    fn shell(uid: u32) -> PeerFacts {
        PeerFacts {
            uid,
            exe: Some(PathBuf::from("/usr/bin/shell")),
            host_namespaces: true,
        }
    }

    #[test]
    fn only_the_seats_compositor_is_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(dir.path(), Some(1000));
        assert_eq!(policy.decide(&shell(1000)), Ok(Caller::Shell { uid: 1000 }));
        let sandboxed = PeerFacts {
            host_namespaces: false,
            ..shell(1000)
        };
        assert_eq!(policy.decide(&sandboxed), Ok(Caller::Person { uid: 1000 }));
        let other_program = PeerFacts {
            exe: Some(PathBuf::from("/usr/bin/python3")),
            ..shell(1000)
        };
        assert_eq!(
            policy.decide(&other_program),
            Ok(Caller::Person { uid: 1000 })
        );
        let exited = PeerFacts {
            exe: None,
            ..shell(1000)
        };
        assert_eq!(policy.decide(&exited), Ok(Caller::Person { uid: 1000 }));
    }

    #[test]
    fn nobody_but_the_person_at_the_seat_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(policy(dir.path(), Some(1000)).decide(&shell(1001)).is_err());
        let empty = tempfile::tempdir().unwrap();
        assert!(policy(empty.path(), None).decide(&shell(1000)).is_err());
    }

    #[test]
    fn sessions_are_read_from_logind() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy(dir.path(), Some(1000));
        assert!(!policy.has_session(1000));
        fs::create_dir_all(dir.path().join("users")).unwrap();
        fs::write(dir.path().join("users/1000"), "STATE=active\n").unwrap();
        assert!(policy.has_session(1000));
    }
}
