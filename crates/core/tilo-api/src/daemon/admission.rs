//! Who a connection to this computer's daemon is, by the account the kernel says is at its other
//! end. Root and the daemon's own account act as the device. Under Tilo OS's system unit, the
//! account that made the device's person ([`Admission::founded_by`]), which the daemon records,
//! acts as that person, and so does an account the sign-in said is them
//! ([`Admission::signed_in`]).
//! Another person signed in on the computer acts as a key of their own on this device, which
//! their root (or their identity's issuer) certified ([`Admission::certified`]): the account is
//! admitted as that key, and only once it holds a certificate for it. Any other account is
//! answered only what a device answers before anyone set it up. A person's own service admits its
//! owner and root alone.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use tilo_user_identity::DeviceCert;

/// The administrator's account: the device's, and the only one that says who signed in.
pub const ROOT: u32 = 0;

/// What a connection acts as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// Root, or the daemon's own account: the device itself.
    Device,
    /// An account signed in as the person whose root key this is: the person the device serves.
    Person([u8; 32]),
    /// An account signed in as another person, acting as the key of theirs on this device that
    /// their certificate names.
    Key {
        /// The person, by their root key.
        person: [u8; 32],
        /// The key the account acts as.
        key: [u8; 32],
    },
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
    seats: Mutex<HashMap<u32, Seat>>,
    /// The accounts that are the device's person, by uid, and the file that keeps them.
    accounts: Mutex<HashMap<u32, [u8; 32]>>,
    kept: Option<PathBuf>,
}

/// Who an account signed in as.
#[derive(Clone, Debug)]
enum Seat {
    /// The person the device serves.
    Person([u8; 32]),
    /// Another person, whose key on this device `key` is, once their certificate for it came.
    Key {
        person: [u8; 32],
        key: [u8; 32],
        cert: Option<DeviceCert>,
    },
}

/// Where in its profile the daemon keeps which accounts are the device's person.
pub const PERSON_ACCOUNTS: &str = "accounts/person.json";

/// The accounts the record at `path` names as the device's person, by uid: none while there is
/// no record.
///
/// # Errors
/// The file is there and cannot be read, or is not such a record.
pub fn person_accounts(path: &std::path::Path) -> io::Result<HashMap<u32, [u8; 32]>> {
    let accounts: HashMap<u32, String> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} names no accounts: {error}", path.display()),
            )
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => return Err(error),
    };
    accounts
        .into_iter()
        .map(|(uid, person)| {
            hex::decode(&person)
                .ok()
                .and_then(|person| <[u8; 32]>::try_from(person).ok())
                .map(|person| (uid, person))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} names account {uid} as no person", path.display()),
                    )
                })
        })
        .collect()
}

impl Admission {
    /// The admission of a daemon running as `own`; `system` when it is the system unit's.
    #[must_use]
    pub fn new(own: u32, system: bool) -> Self {
        Self {
            own,
            system,
            seats: Mutex::default(),
            accounts: Mutex::default(),
            kept: None,
        }
    }

    /// The same admission, keeping which accounts are the device's person in `path`, and
    /// admitting those it already names.
    ///
    /// # Errors
    /// The file is there and cannot be read, or is not such a record.
    pub fn keeping_accounts_in(mut self, path: PathBuf) -> io::Result<Self> {
        self.accounts = Mutex::new(person_accounts(&path)?);
        self.kept = Some(path);
        Ok(self)
    }

    /// Record that the account `uid` made the device's person `person`: under the system unit
    /// it acts as them from now on, across the daemon's restarts. A person's own service is
    /// theirs already and records nothing.
    ///
    /// # Errors
    /// The record could not be kept, or another account is the device's person already.
    pub fn founded_by(&self, uid: u32, person: [u8; 32]) -> Result<(), String> {
        if !self.system || uid == ROOT || uid == self.own {
            return Ok(());
        }
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        if accounts.values().any(|held| *held != person) {
            return Err("another person's account is this computer's already".to_owned());
        }
        accounts.insert(uid, person);
        if let Some(path) = &self.kept {
            let record: HashMap<String, String> = accounts
                .iter()
                .map(|(uid, person)| (uid.to_string(), hex::encode(person)))
                .collect();
            let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
            tilo_files::native::atomic::write_private(path, &bytes).map_err(|error| {
                format!("this computer could not keep which account is its person: {error}")
            })?;
        }
        Ok(())
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
        match self.seat(uid) {
            Some(Seat::Person(person)) => Admitted::Person(person),
            Some(Seat::Key {
                person,
                key,
                cert: Some(_),
            }) => Admitted::Key { person, key },
            Some(Seat::Key { cert: None, .. }) => Admitted::Stranger,
            None => self
                .accounts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&uid)
                .map_or(Admitted::Stranger, |person| Admitted::Person(*person)),
        }
    }

    /// The person signed in as `uid`, when one is: the device's, or another whose key's
    /// certificate came.
    #[must_use]
    pub fn person_of(&self, uid: u32) -> Option<[u8; 32]> {
        match self.admit(uid) {
            Admitted::Person(person) | Admitted::Key { person, .. } => Some(person),
            _ => None,
        }
    }

    /// The certificate of the key `uid` acts as, when it signed in as another person than the
    /// device's and their certificate came.
    #[must_use]
    pub fn key_of(&self, uid: u32) -> Option<DeviceCert> {
        match self.seat(uid) {
            Some(Seat::Key { cert, .. }) => cert,
            _ => None,
        }
    }

    /// The key `uid` is to act as, while it waits for its person's certificate for it: the
    /// person, and the key.
    #[must_use]
    pub fn awaiting(&self, uid: u32) -> Option<([u8; 32], [u8; 32])> {
        match self.seat(uid) {
            Some(Seat::Key {
                person,
                key,
                cert: None,
            }) => Some((person, key)),
            _ => None,
        }
    }

    fn seat(&self, uid: u32) -> Option<Seat> {
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&uid)
            .cloned()
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
            .insert(uid, Seat::Person(person));
        Ok(())
    }

    /// Say that `uid` signed in as `person`, another person than the device's, who acts here as
    /// the key `key`, as asked by `by`. The account is admitted once `person` certified the key
    /// ([`Self::certified`]).
    ///
    /// # Errors
    /// `by` is not root.
    pub fn signed_in_as_key(
        &self,
        by: u32,
        uid: u32,
        person: [u8; 32],
        key: [u8; 32],
    ) -> Result<(), String> {
        Self::by_root(by)?;
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                uid,
                Seat::Key {
                    person,
                    key,
                    cert: None,
                },
            );
        Ok(())
    }

    /// Take `cert` as the certificate of the key `uid` acts as: it must be valid now, name that
    /// key, and be the person's own (signed by their root, or their identity's issuer). The
    /// account acts as the key from now on, until it left.
    ///
    /// # Errors
    /// `uid` waits for no certificate, or `cert` is not one for its key by its person.
    pub fn certified(&self, uid: u32, cert: DeviceCert) -> Result<(), String> {
        let mut seats = self.seats.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(Seat::Key {
            person,
            key,
            cert: held,
        }) = seats.get_mut(&uid)
        else {
            return Err(format!(
                "account {uid} did not sign in as a person with a key of their own here"
            ));
        };
        if cert.device_public_key != *key {
            return Err("the certificate is for another key than the account's".to_owned());
        }
        if cert.user_public_key != *person {
            return Err("the certificate is another person's than the account's".to_owned());
        }
        tilo_user_identity::verify_device_cert(&cert)
            .map_err(|error| format!("the certificate does not hold: {error}"))?;
        *held = Some(cert);
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
    fn the_account_that_made_the_person_is_them_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("accounts").join("person.json");
        let admission = Admission::new(990, true)
            .keeping_accounts_in(kept.clone())
            .unwrap();
        assert_eq!(admission.admit(1000), Admitted::Stranger);
        admission.founded_by(1000, [7; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Person([7; 32]));
        assert_eq!(admission.admit(1001), Admitted::Stranger);
        assert!(
            admission.founded_by(1001, [8; 32]).is_err(),
            "a second person never takes the computer"
        );
        let restarted = Admission::new(990, true).keeping_accounts_in(kept).unwrap();
        assert_eq!(restarted.admit(1000), Admitted::Person([7; 32]));
        assert_eq!(restarted.admit(1001), Admitted::Stranger);
        // Leaving a session ends a sign-in, not which account the person is.
        assert!(!restarted.left(ROOT, 1000).unwrap());
        assert_eq!(restarted.admit(1000), Admitted::Person([7; 32]));
    }

    #[test]
    fn a_persons_service_admits_them_and_root_alone() {
        let admission = Admission::new(1000, false);
        admission.signed_in(ROOT, 1001, [7; 32]).unwrap();
        admission
            .signed_in_as_key(ROOT, 1002, [8; 32], [9; 32])
            .unwrap();
        assert_eq!(admission.admit(1002), Admitted::Stranger);
        assert_eq!(admission.admit(1000), Admitted::Device);
        assert_eq!(admission.admit(ROOT), Admitted::Device);
        assert_eq!(admission.admit(1001), Admitted::Stranger);
    }
}
