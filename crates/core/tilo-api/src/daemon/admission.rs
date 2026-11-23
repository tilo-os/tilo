//! Who a connection to this computer's daemon is, by the account the kernel says is at its other
//! end. Root and the daemon's own account act as the device. Under Tilo OS's system unit, an
//! account the sign-in said is a person's ([`Admission::signed_in`]) acts as that person, and any
//! other account is answered only what a device answers before anyone set it up. A person's own
//! service admits its owner and root alone.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

/// The administrator's account: the device's, and the only one that says who signed in.
pub const ROOT: u32 = 0;

/// What a connection acts as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// Root, or the daemon's own account: the device itself.
    Device,
    /// An account signed in as the person whose root key this is.
    Person([u8; 32]),
    /// Any other account: only what a device answers before it has a person.
    Stranger,
}

impl Admitted {
    /// Whether it acts as the device or a person, rather than only asking what the device is.
    #[must_use]
    pub const fn served(self) -> bool {
        !matches!(self, Self::Stranger)
    }
}

/// Who the daemon's connections are: its own account, whether it serves every account of the
/// computer, and which accounts are signed in as which person.
#[derive(Debug)]
pub struct Admission {
    own: u32,
    system: bool,
    seats: Mutex<HashMap<u32, [u8; 32]>>,
}

impl Admission {
    /// The admission of a daemon running as `own`; `system` when it is the system unit's.
    #[must_use]
    pub fn new(own: u32, system: bool) -> Self {
        Self {
            own,
            system,
            seats: Mutex::default(),
        }
    }

    /// Whether it is the system unit's daemon, serving every account of the computer.
    #[must_use]
    pub const fn system(&self) -> bool {
        self.system
    }

    /// What a connection from `uid` acts as.
    #[must_use]
    pub fn admit(&self, uid: u32) -> Admitted {
        if uid == ROOT || uid == self.own {
            return Admitted::Device;
        }
        if !self.system {
            return Admitted::Stranger;
        }
        self.person_of(uid)
            .map_or(Admitted::Stranger, Admitted::Person)
    }

    /// The person signed in as `uid`, when one is.
    #[must_use]
    pub fn person_of(&self, uid: u32) -> Option<[u8; 32]> {
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&uid)
            .copied()
    }

    /// Say that `uid` signed in as `person`, as asked by `by`.
    ///
    /// # Errors
    /// `by` is not root: only the sign-in says who signed in.
    pub fn signed_in(&self, by: u32, uid: u32, person: [u8; 32]) -> Result<(), String> {
        Self::by_root(by)?;
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid, person);
        Ok(())
    }

    /// Say that `uid`'s session ended, as asked by `by`: whether it was signed in.
    ///
    /// # Errors
    /// `by` is not root.
    pub fn left(&self, by: u32, uid: u32) -> Result<bool, String> {
        Self::by_root(by)?;
        Ok(self
            .seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&uid)
            .is_some())
    }

    fn by_root(by: u32) -> Result<(), String> {
        if by == ROOT {
            Ok(())
        } else {
            Err("only the computer's sign-in says who signed in on it".to_owned())
        }
    }
}

/// This daemon's admission, once it said who it is ([`install`]); every server of its sockets
/// admits by it.
static HERE: OnceLock<Admission> = OnceLock::new();

/// Make `admission` this process's, once: the daemon's start does.
pub fn install(admission: Admission) {
    let _ = HERE.set(admission);
}

/// This process's admission: the daemon's, or, in a process that is no daemon, its own
/// account's alone.
#[must_use]
pub fn here(own: impl FnOnce() -> u32) -> &'static Admission {
    HERE.get_or_init(|| Admission::new(own(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_daemon_admits_root_itself_and_signed_in_people() {
        let admission = Admission::new(990, true);
        assert_eq!(admission.admit(ROOT), Admitted::Device);
        assert_eq!(admission.admit(990), Admitted::Device);
        assert_eq!(admission.admit(1000), Admitted::Stranger);
        assert!(admission.signed_in(1000, 1000, [7; 32]).is_err());
        assert!(admission.signed_in(990, 1000, [7; 32]).is_err());
        admission.signed_in(ROOT, 1000, [7; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Person([7; 32]));
        assert!(admission.left(1000, 1000).is_err());
        assert!(admission.left(ROOT, 1000).unwrap());
        assert_eq!(admission.admit(1000), Admitted::Stranger);
    }

    #[test]
    fn a_persons_service_admits_them_and_root_alone() {
        let admission = Admission::new(1000, false);
        admission.signed_in(ROOT, 1001, [7; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Device);
        assert_eq!(admission.admit(ROOT), Admitted::Device);
        assert_eq!(admission.admit(1001), Admitted::Stranger);
    }
}
