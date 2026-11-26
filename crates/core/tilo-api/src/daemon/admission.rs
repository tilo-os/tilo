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

#[cfg(unix)]
use peer_facts::{Namespaces, PeerFacts};
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
    /// The account whose session the daemon admitted by the founding link its sign-in answered:
    /// it may make a person on this computer or join one, and reads what that setup draws, and
    /// nothing of a person's. Tied to the one process the link was redeemed from, never to the
    /// account at large: an app, an agent or any other program of the account is no founding
    /// link.
    Founding {
        /// The key the daemon made the account on this computer, which its founded person acts as
        /// once it certified it.
        key: [u8; 32],
    },
    /// Any other account: only what a device answers before it has a person.
    Stranger,
}

impl Admitted {
    /// Whether it is admitted to more than asking what the device is: the device's own account,
    /// a person, a key of a person, or the founding link a sign-in answered.
    #[must_use]
    pub const fn served(self) -> bool {
        !matches!(self, Self::Stranger)
    }
}

/// Who a connection to a session's own socket is from: what the kernel's account of the process
/// at its other end says, and nothing the process says about itself.
///
/// A session belongs to a login, so it admits that account's programs in its user and mount
/// namespaces. A process whose namespaces the kernel will not show made itself non-dumpable, as
/// one does that loads the device key: the session serves it an environment's socket and nothing
/// else, since it cannot tell it from a sandboxed app's owner. What keeps a sandboxed app's
/// program out of that socket is the mount: no sandbox binds the session's directory, so whatever
/// opens it is a program of the account outside every sandbox.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPeer {
    /// A program of the session's account, in the session's namespaces: the login's own.
    Login {
        /// The process that opened the connection.
        pid: u32,
    },
    /// A process of the session's account whose namespaces the kernel will not show: served an
    /// environment's socket and nothing else.
    Hidden {
        /// The process that opened the connection.
        pid: u32,
    },
    /// A program outside the session's user or mount namespace: a sandboxed app's program.
    Sandboxed,
    /// Another account's process.
    OtherAccount,
    /// The kernel did not name the process that opened the connection.
    Unidentified,
}

#[cfg(unix)]
impl SessionPeer {
    /// What `facts`, the kernel's account of the process at the other end of a connection, say the
    /// session of the account `session_uid` sees. Decided before the session reads a byte the
    /// peer sent.
    #[must_use]
    pub fn of(facts: &PeerFacts, session_uid: u32) -> Self {
        // No pid is no process this login can be serving, whoever its account is.
        let Some(pid) = facts.pid else {
            return Self::Unidentified;
        };
        if facts.uid != session_uid {
            return Self::OtherAccount;
        }
        match facts.namespaces {
            Namespaces::Shared => Self::Login { pid },
            Namespaces::Other => Self::Sandboxed,
            Namespaces::NotShown => Self::Hidden { pid },
        }
    }
}

/// Who the daemon's connections are: its own account, whether it serves every account of the
/// computer, and which accounts are signed in as which person.
#[derive(Debug)]
pub struct Admission {
    own: u32,
    system: bool,
    seats: Mutex<HashMap<u32, Seat>>,
    /// Each account that made a person here, by uid, and the profile that keeps them.
    accounts: Mutex<HashMap<u32, AccountPerson>>,
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
        cert: Option<Box<DeviceCert>>,
    },
    /// An account root signed in that has no person yet, whose key on this device `key` is: it
    /// may make its person, and then acts as that key.
    Founding { key: [u8; 32] },
}

/// A person an account made here, and whether it is the person the device itself serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountPerson {
    /// The person, by their root key.
    pub person: [u8; 32],
    /// Whether this is the account the device was set up from, which its own services serve.
    pub device: bool,
}

/// Where in its profile the daemon keeps each account's person.
pub const ACCOUNTS: &str = "accounts";

/// The file recording that `uid` made the person `person`, in the profile `profile`.
#[must_use]
pub fn person_record(profile: &std::path::Path, uid: u32) -> PathBuf {
    profile
        .join(ACCOUNTS)
        .join(uid.to_string())
        .join("person.json")
}

/// Where in its profile the daemon keeps each signed-in account's session.
pub const SESSIONS: &str = "sessions";

/// The file recording that `uid` is signed in as `person` while its session runs, in `profile`.
#[must_use]
pub fn session_record(profile: &std::path::Path, uid: u32) -> PathBuf {
    profile.join(SESSIONS).join(format!("{uid}.json"))
}

/// The sessions `profile` says are still running: each account's person, from the records root's
/// sign-ins wrote. A daemon restarting mid-session reads them, so the session's shell reaches its
/// person again.
///
/// # Errors
/// A record is there and cannot be read, or is not such a record.
fn recorded_sessions(profile: &std::path::Path) -> io::Result<HashMap<u32, Seat>> {
    let mut sessions = HashMap::new();
    let dir = profile.join(SESSIONS);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(sessions),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(uid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let path = entry.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let record: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} names no session's person: {error}", path.display()),
            )
        })?;
        let person = record
            .get("person")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} names no person", path.display()),
                )
            })?;
        sessions.insert(uid, Seat::Person(person_key(&path, person)?));
    }
    Ok(sessions)
}

/// The accounts that made a person, from the records in `profile`: none while there are none.
/// The record each one written before accounts were kept separately is read too.
///
/// # Errors
/// A record is there and cannot be read, or is not such a record.
pub fn recorded_accounts(profile: &std::path::Path) -> io::Result<HashMap<u32, AccountPerson>> {
    let mut accounts = HashMap::new();
    let dir = profile.join(ACCOUNTS);
    // The one record a profile kept before each account's person was kept apart.
    let legacy = dir.join("person.json");
    match std::fs::read(&legacy) {
        Ok(bytes) => {
            let by_uid: HashMap<u32, String> = serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} names no accounts: {error}", legacy.display()),
                )
            })?;
            for (uid, person) in by_uid {
                accounts.insert(
                    uid,
                    AccountPerson {
                        person: person_key(&legacy, &person)?,
                        device: true,
                    },
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(accounts),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(uid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let path = entry.path().join("person.json");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let record: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} names no account's person: {error}", path.display()),
            )
        })?;
        let person = record
            .get("person")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} names no person", path.display()),
                )
            })?;
        let device = record.get("device").and_then(serde_json::Value::as_bool) == Some(true)
            || !accounts.values().any(|held: &AccountPerson| held.device);
        accounts.insert(
            uid,
            AccountPerson {
                person: person_key(&path, person)?,
                device,
            },
        );
    }
    Ok(accounts)
}

/// The person a record names, from the hex it keeps.
fn person_key(path: &std::path::Path, person: &str) -> io::Result<[u8; 32]> {
    hex::decode(person)
        .ok()
        .and_then(|person| <[u8; 32]>::try_from(person).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} names a person that is no key", path.display()),
            )
        })
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

    /// The same admission, keeping each account's person in `profile`, and admitting those it
    /// already names.
    ///
    /// # Errors
    /// A record is there and cannot be read, or is not such a record.
    pub fn keeping_accounts_in(mut self, profile: PathBuf) -> io::Result<Self> {
        self.accounts = Mutex::new(recorded_accounts(&profile)?);
        self.seats = Mutex::new(recorded_sessions(&profile)?);
        self.kept = Some(profile);
        Ok(self)
    }

    /// The person the account `uid` made here, when it made one.
    #[must_use]
    pub fn recorded_person(&self, uid: u32) -> Option<AccountPerson> {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&uid)
            .copied()
    }

    /// Whether the account `uid` made a person here.
    #[must_use]
    pub fn has_person(&self, uid: u32) -> bool {
        self.recorded_person(uid).is_some()
    }

    /// The account signed in as `person`, when one is: whose root this computer keeps for them.
    #[must_use]
    #[expect(
        clippy::needless_collect,
        reason = "the accounts' lock is let go before the seats' is taken, so the two never wait on each other"
    )]
    pub fn signed_in_account_for(&self, person: [u8; 32]) -> Option<u32> {
        // The accounts are read out before the seats, as every other path takes them in that
        // order, so the two never wait on each other.
        let held: Vec<(u32, [u8; 32])> = self
            .accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(uid, held)| (*uid, held.person))
            .collect();
        held.into_iter()
            .find(|(uid, held)| *held == person && self.signed_in_now(*uid))
            .map(|(uid, _)| uid)
    }

    /// The account the device itself was set up from, whose person its own services serve.
    #[must_use]
    pub fn device_person_uid(&self) -> Option<u32> {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(_, held)| held.device)
            .map(|(uid, _)| *uid)
    }

    /// Record that the account `uid` made the person `person`: under the system unit it acts as
    /// a key of theirs from then on, across the daemon's restarts. A person's own service is
    /// theirs already and records nothing.
    ///
    /// The first person a computer makes is the one the device itself serves; every account
    /// after it keeps a person of its own, never that one.
    ///
    /// # Errors
    /// The record could not be kept, or this account made another person already.
    pub fn founded_by(&self, uid: u32, person: [u8; 32]) -> Result<(), String> {
        let device = self
            .accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .all(|held| !held.device);
        self.founded_by_as(uid, person, device)
    }

    /// [`Self::founded_by`], saying whether this is the person the device itself serves: the
    /// account a certificate already names it for is, and no other.
    ///
    /// # Errors
    /// As [`Self::founded_by`].
    pub fn founded_by_as(&self, uid: u32, person: [u8; 32], device: bool) -> Result<(), String> {
        if !self.system || uid == ROOT || uid == self.own {
            return Ok(());
        }
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(held) = accounts.get(&uid)
            && held.person != person
        {
            return Err("this account made another person on this computer already".to_owned());
        }
        accounts.insert(uid, AccountPerson { person, device });
        drop(accounts);
        if let Some(profile) = &self.kept {
            let path = person_record(profile, uid);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| {
                    format!("this computer could not keep which account is whose person: {error}")
                })?;
            }
            let record = serde_json::json!({ "person": hex::encode(person), "device": device });
            let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
            tilo_files::native::atomic::write_private(&path, &bytes).map_err(|error| {
                format!("this computer could not keep which account is whose person: {error}")
            })?;
        }
        Ok(())
    }

    /// Record that the account `uid` joined the person `person` this computer now serves,
    /// replacing the person it made here before: the person made for the setup is retired, and
    /// the account acts as the joined one.
    ///
    /// # Errors
    /// The record could not be kept.
    pub fn adopted(&self, uid: u32, person: [u8; 32]) -> Result<(), String> {
        if !self.system || uid == ROOT || uid == self.own {
            return Ok(());
        }
        {
            let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
            for held in accounts.values_mut() {
                if held.device && held.person != person {
                    held.device = false;
                }
            }
            accounts.insert(
                uid,
                AccountPerson {
                    person,
                    device: true,
                },
            );
        }
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid, Seat::Person(person));
        if let Some(profile) = &self.kept {
            let path = person_record(profile, uid);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| {
                    format!("this computer could not keep which account is whose person: {error}")
                })?;
            }
            let record = serde_json::json!({ "person": hex::encode(person), "device": true });
            let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
            tilo_files::native::atomic::write_private(&path, &bytes).map_err(|error| {
                format!("this computer could not keep which account is whose person: {error}")
            })?;
        }
        Ok(())
    }

    /// The account `uid` signed in with no person yet, whose key here `key` is: it may make one,
    /// and then acts as that key.
    ///
    /// # Errors
    /// `by` is not root.
    pub fn signed_in_founding(&self, by: u32, uid: u32, key: [u8; 32]) -> Result<(), String> {
        Self::by_root(by)?;
        self.seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid, Seat::Founding { key });
        Ok(())
    }

    /// The account `uid` made the person `person` after signing in with no person yet: it acts
    /// as the key of theirs this device made for it, once their certificate for it comes.
    ///
    /// # Errors
    /// `uid` signed in with no key of its own here.
    pub fn founded(&self, uid: u32, person: [u8; 32]) -> Result<(), String> {
        let mut seats = self.seats.lock().unwrap_or_else(PoisonError::into_inner);
        let founding = match seats.get(&uid) {
            Some(Seat::Founding { key }) => Some(*key),
            _ => None,
        };
        if let Some(key) = founding {
            seats.insert(
                uid,
                Seat::Key {
                    person,
                    key,
                    cert: None,
                },
            );
        }
        drop(seats);
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
        // A person's root is opened only while their account is signed in, so an account no
        // sign-in named now is answered the device's own actions alone.
        match self.seat(uid) {
            Some(Seat::Person(person)) => Admitted::Person(person),
            Some(Seat::Key {
                person,
                key,
                cert: Some(_),
            }) => Admitted::Key { person, key },
            Some(Seat::Key { cert: None, .. } | Seat::Founding { .. }) | None => Admitted::Stranger,
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
            Some(Seat::Key { cert, .. }) => cert.map(|cert| *cert),
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

    /// The key `uid` may act as once it makes its person, while it has made none yet.
    #[must_use]
    pub fn founding(&self, uid: u32) -> Option<[u8; 32]> {
        match self.seat(uid) {
            Some(Seat::Founding { key }) => Some(key),
            _ => None,
        }
    }

    /// Whether this account is signed in, with a person here or none yet.
    #[must_use]
    pub fn signed_in_now(&self, uid: u32) -> bool {
        self.seat(uid).is_some()
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
        // The session outlives this daemon process: one restarted while the person is still
        // signed in admits them, as root said they are, so its shell reaches them again.
        if let Some(profile) = &self.kept {
            let path = session_record(profile, uid);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| {
                    format!("this computer could not keep who is signed in: {error}")
                })?;
            }
            let bytes = serde_json::json!({ "person": hex::encode(person) }).to_string();
            tilo_files::native::atomic::write_private(&path, bytes.as_bytes()).map_err(
                |error| format!("this computer could not keep who is signed in: {error}"),
            )?;
        }
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
        *held = Some(Box::new(cert));
        drop(seats);
        Ok(())
    }

    /// Say that `uid`'s session ended, as asked by `by`: whether it was signed in.
    ///
    /// # Errors
    /// `by` is not root.
    pub fn left(&self, by: u32, uid: u32) -> Result<bool, String> {
        Self::by_root(by)?;
        let was = self
            .seats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&uid)
            .is_some();
        if let Some(profile) = &self.kept {
            let _ = std::fs::remove_file(session_record(profile, uid));
        }
        Ok(was)
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

/// This process's admission when its daemon installed one: none in a process that is no daemon,
/// which keeping accounts is not asked of.
#[must_use]
pub fn installed() -> Option<&'static Admission> {
    HERE.get()
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
    fn each_account_keeps_its_own_person_across_restarts() {
        let profile = tempfile::tempdir().unwrap();
        let admission = Admission::new(990, true)
            .keeping_accounts_in(profile.path().to_path_buf())
            .unwrap();
        assert_eq!(admission.admit(1000), Admitted::Stranger);
        admission.founded_by(1000, [7; 32]).unwrap();
        // The root is opened only while the account is signed in.
        assert_eq!(admission.admit(1000), Admitted::Stranger);
        admission.signed_in(ROOT, 1000, [7; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Person([7; 32]));
        // Another account makes a person of its own, never the first account's.
        admission.founded_by(1001, [8; 32]).unwrap();
        assert_eq!(admission.recorded_person(1001).unwrap().person, [8; 32]);
        assert!(!admission.recorded_person(1001).unwrap().device);
        assert!(
            admission.founded_by(1001, [9; 32]).is_err(),
            "an account makes one person on a computer"
        );
        let restarted = Admission::new(990, true)
            .keeping_accounts_in(profile.path().to_path_buf())
            .unwrap();
        assert_eq!(restarted.recorded_person(1000).unwrap().person, [7; 32]);
        assert_eq!(restarted.recorded_person(1001).unwrap().person, [8; 32]);
        assert_eq!(restarted.device_person_uid(), Some(1000));
        assert_eq!(restarted.admit(1001), Admitted::Stranger);
        // Leaving a session ends a sign-in, not which account made which person.
        assert!(!restarted.left(ROOT, 1000).unwrap());
        assert_eq!(restarted.recorded_person(1000).unwrap().person, [7; 32]);
    }

    #[test]
    fn an_account_with_no_person_signs_in_to_found_one() {
        let admission = Admission::new(990, true);
        admission.signed_in_founding(ROOT, 1001, [9; 32]).unwrap();
        assert_eq!(admission.admit(1001), Admitted::Stranger);
        assert_eq!(admission.founding(1001), Some([9; 32]));
        admission.founded(1001, [8; 32]).unwrap();
        assert_eq!(admission.awaiting(1001), Some(([8; 32], [9; 32])));
        assert_eq!(admission.founding(1001), None);
    }

    #[test]
    fn an_account_with_no_person_may_make_one() {
        let profile = tempfile::tempdir().unwrap();
        let admission = Admission::new(990, true)
            .keeping_accounts_in(profile.path().to_path_buf())
            .unwrap();
        assert!(!admission.has_person(1000));
        admission.founded_by(1000, [7; 32]).unwrap();
        assert!(admission.has_person(1000));
        assert!(!admission.has_person(1001));
    }

    #[test]
    fn an_account_that_joins_a_person_retires_the_one_it_made_here() {
        let profile = tempfile::tempdir().unwrap();
        let admission = Admission::new(990, true)
            .keeping_accounts_in(profile.path().to_path_buf())
            .unwrap();
        admission.founded_by(1000, [7; 32]).unwrap();
        admission.signed_in(ROOT, 1000, [7; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Person([7; 32]));
        assert!(admission.recorded_person(1000).unwrap().device);
        // The computer joined the person the account already had: the person it made here for
        // its setup is retired, and the account acts as the one it joined.
        admission.adopted(1000, [8; 32]).unwrap();
        assert_eq!(admission.admit(1000), Admitted::Person([8; 32]));
        let held = admission.recorded_person(1000).expect("the joined person");
        assert_eq!(held.person, [8; 32]);
        assert!(
            held.device,
            "the joined person is the one this device serves"
        );
        // It survives a restart as the account's person.
        let restarted = Admission::new(990, true)
            .keeping_accounts_in(profile.path().to_path_buf())
            .unwrap();
        assert_eq!(restarted.recorded_person(1000).unwrap().person, [8; 32]);
        assert!(restarted.recorded_person(1000).unwrap().device);
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

    /// Who a session's own socket admits, from the kernel's account of the peer.
    #[cfg(unix)]
    mod session_peers {
        use std::path::PathBuf;

        use super::*;

        const OWN: u32 = 1000;

        fn peer(uid: u32, pid: Option<u32>, namespaces: Namespaces) -> PeerFacts {
            PeerFacts {
                uid,
                pid,
                exe: Some(PathBuf::from("/usr/bin/tilo-session")),
                namespaces,
            }
        }

        /// A process of this account the kernel hides comes in by the entrance that asks only for
        /// an environment's socket; a process it can look at in another mount namespace is a
        /// sandboxed program and is refused as one, and another account's process is refused
        /// before anything of it is considered.
        #[test]
        fn a_hidden_process_of_this_account_comes_in_for_an_environments_socket_alone() {
            assert_eq!(
                SessionPeer::of(&peer(OWN, Some(7), Namespaces::NotShown), OWN),
                SessionPeer::Hidden { pid: 7 },
                "hidden, and in the login's account: let in for an environment's socket"
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN, Some(7), Namespaces::Other), OWN),
                SessionPeer::Sandboxed,
                "a program in another mount namespace, which the session can look at, is sandboxed"
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN + 1, Some(7), Namespaces::NotShown), OWN),
                SessionPeer::OtherAccount,
                "another account's process, hidden or not, is not let in"
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN, Some(7), Namespaces::Shared), OWN),
                SessionPeer::Login { pid: 7 },
                "a program of this login sharing its namespaces is an ordinary one"
            );
        }

        #[test]
        fn the_peer_must_be_this_accounts_own_process_in_its_namespaces() {
            assert_eq!(
                SessionPeer::of(&peer(OWN, Some(1), Namespaces::Shared), OWN),
                SessionPeer::Login { pid: 1 }
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN + 1, Some(1), Namespaces::Shared), OWN),
                SessionPeer::OtherAccount,
                "another account"
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN, Some(1), Namespaces::Other), OWN),
                SessionPeer::Sandboxed,
                "a program inside a sandbox"
            );
            assert_eq!(
                SessionPeer::of(&peer(OWN, None, Namespaces::NotShown), OWN),
                SessionPeer::Unidentified,
                "the kernel named no process"
            );
        }
    }
}
