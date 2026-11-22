//! The file accounts of the workspaces this device holds, and their sign-ins, as the session
//! tells them to this login's own cloud service.
//!
//! The cloud service (`fs cloud-service`) runs outside any sandbox, as the person, and keeps a
//! mount for each file account. It reads the accounts from the workspaces' published accounts
//! records, not from a file on the device, and signs in with the vault entry each account names
//! (`passwordItem`), asked for on this socket. The session answers a sign-in the way it answers
//! any app's engine (`BrowserServiceSessionCommand::ReleaseAccountSecret`): for an enabled account
//! of the workspace that names the entry, at the servers it names, decided by the person's grant
//! on Passwords, with a use written into the entry first. A released secret stays in the cloud
//! service's memory and goes to its mount on the mount's standard input.
//!
//! The socket lives in a directory of its own below the runtime directory, which only the
//! person's own processes reach; the launcher hands it to no app.
//!
//! An app's origin connector, the sandboxed process the session starts for a tilolang app whose
//! types hold an outside service's resources, speaks the same lines on a connection the session
//! hands it when it starts ([`Client::over`]). The session scopes that connection to the
//! connector's workspace and app: it lists every account of that workspace, refuses a sign-in of
//! another, and the use a sign-in writes names the app.
//!
//! One request and one answer per line, as JSON.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Account;

/// The directory below the runtime directory that holds the socket.
pub const RUNTIME_DIR_NAME: &str = "tilo-account-sign-ins";
const SOCKET_NAME: &str = "socket";

/// What a client is told when the session no longer answers its sign-in, or answered another
/// ask than its own.
pub const NOT_ANSWERED: &str = "The session no longer answers this account's sign-in";

/// The longest a line of either side may be: the accounts of every workspace held here.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// How long the session holds an ask for accounts that changed before it answers with the same
/// ones, so the asker sees the connection is alive.
pub const LONGEST_WAIT: Duration = Duration::from_secs(50);

/// The socket below the runtime directory `runtime_dir`.
#[must_use]
pub fn socket_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(RUNTIME_DIR_NAME).join(SOCKET_NAME)
}

/// The socket below this login's runtime directory.
#[must_use]
pub fn socket() -> Option<PathBuf> {
    Some(socket_in(&tilo_paths::xdg::runtime_dir()?))
}

/// What the cloud service asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// The file accounts of every workspace this device holds. With `after`, the revision the
    /// asker holds: the answer waits until the accounts are another revision, or until
    /// [`LONGEST_WAIT`] passed.
    Accounts { after: Option<String> },
    /// The sign-in kept in the vault entry `item` (`vault.item:<id>`), which an account of
    /// `workspace` names.
    SignIn { workspace: String, item: String },
}

/// A file account and the workspace whose accounts record it is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    pub workspace: String,
    pub account: Account,
}

/// What the session answers.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// The file accounts, and the revision they are.
    Accounts {
        revision: String,
        accounts: Vec<Held>,
    },
    /// The secret an entry keeps.
    SignIn { secret: Zeroizing<String> },
    /// What the ask was refused with, in words a person reads.
    Refused { error: String },
}

impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accounts { revision, accounts } => f
                .debug_struct("Accounts")
                .field("revision", revision)
                .field("accounts", &accounts.len())
                .finish(),
            Self::SignIn { .. } => f.debug_struct("SignIn").finish_non_exhaustive(),
            Self::Refused { error } => f.debug_struct("Refused").field("error", error).finish(),
        }
    }
}

/// Whether `account` is one the cloud service mounts or imports: the accounts the session lists.
#[must_use]
pub fn is_file_account(account: &Account) -> bool {
    use crate::cloud_types::{DROPBOX, GOOGLE_DRIVE, ICLOUD_DRIVE, NEXTCLOUD, ONEDRIVE, WEBDAV};
    [
        GOOGLE_DRIVE,
        ONEDRIVE,
        DROPBOX,
        ICLOUD_DRIVE,
        NEXTCLOUD,
        WEBDAV,
    ]
    .contains(&account.type_id.as_str())
}

/// Whether `account` signs in with a password its vault entry keeps, released on this socket,
/// rather than a grant its provider's page gave this device.
#[must_use]
pub fn signs_in_with_entry(account: &Account) -> bool {
    use crate::cloud_types::{NEXTCLOUD, WEBDAV};
    [NEXTCLOUD, WEBDAV].contains(&account.type_id.as_str())
}

/// The revision of `accounts`: equal for equal lists, in the order given.
#[must_use]
pub fn revision_of(accounts: &[Held]) -> String {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(&serde_json::to_vec(accounts).unwrap_or_default());
    format!("{:016x}", hasher.finish())
}

/// A connection to the session's socket.
pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connect to the socket at `path`.
    ///
    /// # Errors
    /// The session serves no socket there.
    pub fn connect(path: &Path) -> std::io::Result<Self> {
        Self::over(tilo_files::native::socket::connect(path)?)
    }

    /// A client over `stream`, a connection the session handed this process rather than one it
    /// dialled: an origin connector's, which the session scopes to its app and workspace.
    ///
    /// # Errors
    /// The connection's timeouts cannot be set.
    pub fn over(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(LONGEST_WAIT.saturating_add(Duration::from_secs(20))))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Ask `ask` and read its answer.
    ///
    /// # Errors
    /// The connection broke, or the answer was not one.
    pub fn ask(&mut self, ask: &Ask) -> std::io::Result<Answer> {
        let mut line = serde_json::to_vec(ask).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.reader.get_mut().write_all(&line)?;
        read_line(&mut self.reader)?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
            .and_then(|line| serde_json::from_slice(&line).map_err(std::io::Error::other))
    }
}

/// The next line of `reader`, without its newline; `None` at the end. Its bytes are wiped when
/// dropped: an answer line carries a secret.
///
/// # Errors
/// The read failed, or the line is longer than [`MAX_LINE`].
pub fn read_line(reader: &mut impl BufRead) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
    let mut line = Zeroizing::new(Vec::new());
    let read = reader
        .by_ref()
        .take(
            u64::try_from(MAX_LINE)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a line of the account sign-ins socket is too long",
        ));
    }
    line.pop();
    Ok(Some(line))
}

/// `answer` as the line the session writes.
///
/// # Errors
/// The answer does not serialize.
pub fn answer_line(answer: &Answer) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut line = Zeroizing::new(serde_json::to_vec(answer).map_err(std::io::Error::other)?);
    line.push(b'\n');
    Ok(line)
}

/// A stand-in for the session on its socket, for the cloud service's tests: the accounts are the
/// ones kept with [`Fixture::keep_account`], and the sign-ins are the passwords kept with
/// [`Fixture::keep`].
#[cfg(any(test, feature = "sign-ins-fixture"))]
pub mod fixture {
    use super::{Answer, Ask, Held};
    use crate::Account;
    use std::collections::{BTreeMap, HashMap};
    use std::io::{BufReader, Write as _};
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use zeroize::Zeroizing;

    /// The workspace the fixture's accounts are in.
    pub const WORKSPACE: &str = "00000000-0000-4000-8000-000000000001";

    /// The vault entry the fixture names as the sign-in of account `id`.
    #[must_use]
    pub fn item_for(id: &str) -> String {
        let hex: String = hex::encode(id.as_bytes())
            .chars()
            .cycle()
            .take(32)
            .collect();
        format!("vault.item:{hex}")
    }

    #[derive(Default)]
    struct Kept {
        accounts: BTreeMap<String, Account>,
        passwords: HashMap<String, String>,
        released: Vec<String>,
    }

    /// The fixture, serving until it drops.
    pub struct Fixture {
        socket: PathBuf,
        kept: Arc<Mutex<Kept>>,
        stop: Arc<AtomicBool>,
    }

    impl Fixture {
        /// Serve the accounts kept with it on a socket in `dir`.
        ///
        /// # Panics
        /// The socket cannot be bound.
        #[must_use]
        #[allow(
            clippy::disallowed_methods,
            clippy::panic,
            reason = "a test's stand-in session answers on threads of its own, and a test without \
                      one stops there"
        )]
        pub fn serve(dir: &Path) -> Self {
            let socket = dir.join("sign-ins.sock");
            let listener = match tilo_files::native::socket::bind(&socket) {
                Ok(listener) => listener,
                Err(error) => panic!("bind the stand-in session at {}: {error}", socket.display()),
            };
            let kept: Arc<Mutex<Kept>> = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (serving, stopped) = (Arc::clone(&kept), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let (kept, stopped) = (Arc::clone(&serving), Arc::clone(&stopped));
                    std::thread::spawn(move || answer(stream, &kept, &stopped));
                }
            });
            Self { socket, kept, stop }
        }

        #[must_use]
        pub fn socket(&self) -> PathBuf {
            self.socket.clone()
        }

        /// Keep `account`, added or in place of the one with its id.
        pub fn keep_account(&self, account: Account) {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .accounts
                .insert(account.id.to_string(), account);
        }

        /// Keep the account `id` no more.
        pub fn remove_account(&self, id: &str) {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .accounts
                .remove(id);
        }

        /// Keep `password` as the sign-in of account `id`.
        pub fn keep(&self, id: &str, password: &str) {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .passwords
                .insert(item_for(id), password.to_owned());
        }

        /// Keep no sign-in for account `id`.
        pub fn forget(&self, id: &str) {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .passwords
                .remove(&item_for(id));
        }

        /// The entries released so far, in order.
        #[must_use]
        pub fn released(&self) -> Vec<String> {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .released
                .clone()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = tilo_files::native::socket::connect(&self.socket);
        }
    }

    fn listed(kept: &Mutex<Kept>) -> Vec<Held> {
        kept.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounts
            .values()
            .filter(|account| super::is_file_account(account))
            .map(|account| {
                let mut account = account.clone();
                if super::signs_in_with_entry(&account)
                    && crate::email::password_item(&account.config).is_none()
                {
                    let item = item_for(account.id.as_str());
                    crate::email::set_password_item(&mut account.config, Some(&item));
                }
                Held {
                    workspace: WORKSPACE.into(),
                    account,
                }
            })
            .collect()
    }

    fn answer(stream: std::os::unix::net::UnixStream, kept: &Mutex<Kept>, stop: &AtomicBool) {
        let Ok(mut write) = stream.try_clone() else {
            return;
        };
        let mut read = BufReader::new(stream);
        while let Ok(Some(line)) = super::read_line(&mut read) {
            let Ok(ask) = serde_json::from_slice::<Ask>(&line) else {
                return;
            };
            let answer = match ask {
                Ask::Accounts { after } => {
                    let mut waited = Duration::ZERO;
                    loop {
                        let accounts = listed(kept);
                        let revision = super::revision_of(&accounts);
                        if after.as_deref() != Some(revision.as_str())
                            || waited >= Duration::from_secs(2)
                            || stop.load(Ordering::SeqCst)
                        {
                            break Answer::Accounts { revision, accounts };
                        }
                        std::thread::sleep(Duration::from_millis(20));
                        waited = waited.saturating_add(Duration::from_millis(20));
                    }
                }
                Ask::SignIn { item, .. } => {
                    let mut kept = kept
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let secret = kept.passwords.get(&item).cloned();
                    if secret.is_some() {
                        kept.released.push(item);
                    }
                    drop(kept);
                    match secret {
                        Some(secret) => Answer::SignIn {
                            secret: Zeroizing::new(secret),
                        },
                        None => Answer::Refused {
                            error: "This workspace's vault keeps no such entry".into(),
                        },
                    }
                }
            };
            let Ok(line) = super::answer_line(&answer) else {
                return;
            };
            if write.write_all(&line).is_err() {
                return;
            }
        }
    }
}
