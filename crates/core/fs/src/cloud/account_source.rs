//! Where the cloud service reads its file accounts and signs in to them.
//!
//! [`SessionAccounts`] asks the session ([`accounts::sign_ins`]): the accounts are the
//! workspaces' published accounts records, and an account that signs in with a password (`WebDAV`,
//! Nextcloud) signs in with the vault entry it names, released by the session in memory. No
//! account's secret is in a file on this device. An account that signs in by its provider's page
//! (Google Drive, `OneDrive`, Dropbox, iCloud Drive) has no sign-in the workspace's vault hands
//! out yet: it asks its person to sign in again.
//!
//! How each account's mount syncs goes back on the same socket ([`Ask::ReportSync`]): the
//! session writes it into the workspace's accounts, where every member's devices read it.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accounts::sign_ins::{self, Answer, Ask, Held};
use accounts::{Account, AccountId, SyncHealth};
use cloud_source::{Credential, CredentialSource};
use tilo_lock::MutexRecover as _;
use zeroize::Zeroizing;

/// The cloud service's file accounts and their sign-ins.
pub trait AccountSource: Send + Sync {
    /// The file accounts now.
    fn accounts(&self) -> Vec<Account>;
    /// Call `changed` each time the accounts may have changed, until the returned watch drops.
    fn watch(&self, changed: Box<dyn Fn() + Send + Sync>) -> io::Result<AccountsWatch>;
    /// What `account` signs in with.
    fn credential(&self, account: &Account) -> Arc<dyn CredentialSource>;
    /// Say how `account`'s sync stands on this device, as every member reads it.
    fn report(&self, account: &AccountId, health: SyncHealth);
    /// Whether its person signed in to `account` and no device reported a sync since.
    fn signed_in(&self, account: &AccountId) -> bool;
}

/// Keeps an [`AccountSource::watch`] going; dropping it stops it.
pub struct AccountsWatch {
    stop: Arc<AtomicBool>,
}

impl Drop for AccountsWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// How long the watch waits before it asks again after the session did not answer.
const RECONNECT: Duration = Duration::from_secs(1);

/// The accounts and sign-ins the session tells on its socket.
pub struct SessionAccounts {
    inner: Arc<Inner>,
}

struct Inner {
    socket: PathBuf,
    /// The last accounts the session told, and their revision.
    latest: Mutex<Option<(String, Vec<Held>)>>,
}

impl SessionAccounts {
    /// The session's accounts on `socket`.
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                socket,
                latest: Mutex::new(None),
            }),
        }
    }
}

impl Inner {
    fn ask(&self, ask: &Ask) -> io::Result<Answer> {
        sign_ins::Client::connect(&self.socket)?.ask(ask)
    }

    /// Ask for the accounts after `after`, and keep what is told. Whether they changed.
    fn fetch(&self, after: Option<String>) -> io::Result<bool> {
        match self.ask(&Ask::Accounts { after })? {
            Answer::Accounts { revision, accounts } => {
                let mut latest = self.latest.lock_recover();
                let changed = latest.as_ref().is_none_or(|(held, _)| *held != revision);
                if changed {
                    tracing::info!(
                        accounts = accounts.len(),
                        "the session told other file accounts"
                    );
                }
                *latest = Some((revision, accounts));
                Ok(changed)
            }
            Answer::Refused { error } => Err(io::Error::other(error)),
            Answer::SignIn { .. } | Answer::Reported => {
                Err(io::Error::other(accounts::sign_ins::NOT_ANSWERED))
            }
        }
    }

    fn revision(&self) -> Option<String> {
        self.latest
            .lock_recover()
            .as_ref()
            .map(|(revision, _)| revision.clone())
    }

    /// What the session last told of `account`.
    fn held<T>(&self, account: &AccountId, read: impl FnOnce(&Held) -> T) -> Option<T> {
        self.latest.lock_recover().as_ref().and_then(|(_, held)| {
            held.iter()
                .find(|held| held.account.id == *account)
                .map(read)
        })
    }

    fn workspace_of(&self, account: &AccountId) -> Option<String> {
        self.held(account, |held| held.workspace.clone())
    }
}

impl AccountSource for SessionAccounts {
    fn accounts(&self) -> Vec<Account> {
        // Read anew: a removal must not wait for the watch to tell it.
        if let Err(error) = self.inner.fetch(None) {
            tracing::info!(%error, "the session does not tell the file accounts now");
        }
        self.inner
            .latest
            .lock_recover()
            .as_ref()
            .map(|(_, held)| held.iter().map(|held| held.account.clone()).collect())
            .unwrap_or_default()
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the watch waits on the session's socket on a thread of its own"
    )]
    fn watch(&self, changed: Box<dyn Fn() + Send + Sync>) -> io::Result<AccountsWatch> {
        let stop = Arc::new(AtomicBool::new(false));
        let inner = Arc::clone(&self.inner);
        let stopped = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("cloud-accounts".into())
            .spawn(move || {
                while !stopped.load(Ordering::SeqCst) {
                    match inner.fetch(inner.revision()) {
                        Ok(true) if !stopped.load(Ordering::SeqCst) => changed(),
                        Ok(_) => {}
                        Err(error) => {
                            tracing::info!(%error, "the session's file accounts are not told yet");
                            std::thread::sleep(RECONNECT);
                        }
                    }
                }
            })?;
        Ok(AccountsWatch { stop })
    }

    fn credential(&self, account: &Account) -> Arc<dyn CredentialSource> {
        if sign_ins::signs_in_with_entry(account) {
            return Arc::new(EntrySignIn {
                inner: Arc::clone(&self.inner),
                account: account.id.clone(),
                item: accounts::email::password_item(&account.config).map(ToOwned::to_owned),
                username: account
                    .config
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                held: Mutex::new(None),
                rejected: Mutex::new(None),
            });
        }
        if account_credentials::is_cloud_type(account.type_id.as_str()) {
            // A sign-in-by-provider-page account's grant is this device's, not the
            // workspace's: the drive renews and reads it through the same store
            // `tilo migrate` signs in with.
            return Arc::new(account_credentials::DriveSignIn::new(
                account_credentials::CredentialStore::for_root(None).with_apple_icloud(),
                account.clone(),
            ));
        }
        Arc::new(NoCredential)
    }

    fn report(&self, account: &AccountId, health: SyncHealth) {
        let Some(workspace) = self.inner.workspace_of(account) else {
            return;
        };
        let ask = Ask::ReportSync {
            workspace,
            account: account.as_str().to_owned(),
            health,
            signed_in: false,
            message: health.summary().to_owned(),
        };
        match self.inner.ask(&ask) {
            Ok(Answer::Reported) => {}
            Ok(Answer::Refused { error }) => {
                tracing::info!(%error, "the session took no sync report of a file account");
            }
            Ok(_) => tracing::info!("the session answered a sync report with something else"),
            Err(error) => tracing::info!(%error, "the session does not take sync reports now"),
        }
    }

    fn signed_in(&self, account: &AccountId) -> bool {
        self.inner
            .held(account, |held| held.signed_in)
            .unwrap_or_default()
    }
}

/// An account's password, released by the session from the vault entry the account names and
/// kept in this process's memory.
struct EntrySignIn {
    inner: Arc<Inner>,
    account: AccountId,
    item: Option<String>,
    username: String,
    held: Mutex<Option<Zeroizing<String>>>,
    /// The password the server refused: released again unchanged, the account needs signing in.
    rejected: Mutex<Option<Zeroizing<String>>>,
}

impl CredentialSource for EntrySignIn {
    fn credential(&self) -> cloud_source::Result<Credential> {
        if let Some(password) = self.held.lock_recover().as_ref() {
            return Ok(Credential::Basic {
                username: self.username.clone(),
                password: password.clone(),
            });
        }
        let Some(item) = &self.item else {
            return Err(cloud_source::Error::Unauthorized);
        };
        let Some(workspace) = self.inner.workspace_of(&self.account) else {
            return Err(cloud_source::Error::Unauthorized);
        };
        let answer = self.inner.ask(&Ask::SignIn {
            workspace,
            item: item.clone(),
        });
        match answer {
            Ok(Answer::SignIn { secret }) => {
                if self.rejected.lock_recover().as_ref() == Some(&secret) {
                    return Err(cloud_source::Error::Unauthorized);
                }
                *self.held.lock_recover() = Some(secret.clone());
                Ok(Credential::Basic {
                    username: self.username.clone(),
                    password: secret,
                })
            }
            Ok(Answer::Refused { error }) => {
                tracing::info!(%error, "the session released no sign-in for a file account");
                Err(cloud_source::Error::Unauthorized)
            }
            Ok(Answer::Accounts { .. } | Answer::Reported) => {
                Err(cloud_source::Error::Unauthorized)
            }
            // The session is not up yet, or restarting: tried again later.
            Err(_) => Err(cloud_source::Error::Offline),
        }
    }

    fn rejected(&self) {
        let refused = self.held.lock_recover().take();
        if refused.is_some() {
            *self.rejected.lock_recover() = refused;
        }
    }
}

/// An account with nowhere to find its credential.
pub(crate) struct NoCredential;

impl CredentialSource for NoCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
        Err(cloud_source::Error::Unauthorized)
    }

    fn rejected(&self) {}
}
