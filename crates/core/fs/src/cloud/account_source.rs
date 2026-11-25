//! Where the cloud service reads its file accounts and signs in to them.
//!
//! The service keeps no vault and holds no sign-in of its own. The daemon that runs the
//! workspace keeps the accounts and the vault, and answers this device's mount through the one
//! dispatch ([`tilo_actions::remote::person_accounts`]): the accounts the workspace keeps, each
//! with the settings its mount reads and never its secret, and, for one account whenever its
//! mount asks, what it signs in with now: a password, a grant's current access token, or an
//! iCloud session's headers. A grant's renewal is kept in the workspace's vault by the daemon,
//! never written here.
//!
//! How each account's mount syncs goes back the same way ([`person_accounts::REPORT_SYNC_VERB`]),
//! as this device's report in the workspace's accounts.
//!
//! The daemon answers only a connection of the person's own on this computer, which is what
//! this service's connection is.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accounts::{Account, AccountId, SyncHealth};
use cloud_source::{Credential, CredentialSource};
use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, LineClient, ReceiptStatus,
    RemoteActionTarget,
};
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

/// How long the watch waits before it asks the daemon again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// How long one action call waits for the daemon's answer.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// A file account, and the workspace whose accounts keep it.
#[derive(Clone, Debug)]
struct Held {
    workspace: String,
    account: Account,
    signed_in: bool,
}

/// One action of the daemon's dispatch, as the cloud service asks it.
pub trait Ask: Send + Sync {
    /// Invoke `target` in `workspace` (none: this device's own) and answer its result.
    ///
    /// # Errors
    /// The daemon does not answer ([`Asked::Offline`]), or refused the call.
    fn invoke(
        &self,
        workspace: Option<&str>,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, Asked>;
}

/// Why an ask did not answer.
#[derive(Debug)]
pub enum Asked {
    /// The daemon does not answer now, or is restarting: asked again later.
    Offline(String),
    /// The daemon refused the call, in words a person reads.
    Refused(String),
}

impl std::fmt::Display for Asked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline(reason) | Self::Refused(reason) => formatter.write_str(reason),
        }
    }
}

/// The daemon's dispatch over this computer's native endpoint.
pub struct Daemon {
    socket: PathBuf,
    line: Mutex<Option<LineClient>>,
}

impl Daemon {
    /// The daemon of this computer, at its native endpoint.
    ///
    /// # Errors
    /// This computer's daemon socket is not known.
    pub fn here() -> Result<Self, String> {
        Ok(Self::at(tilo_paths::machine::daemon_native()?))
    }

    /// The daemon listening at `socket`.
    #[must_use]
    pub fn at(socket: PathBuf) -> Self {
        Self {
            socket,
            line: Mutex::new(None),
        }
    }

    fn connected(&self) -> Result<LineClient, Asked> {
        if let Some(line) = self.line.lock_recover().as_ref() {
            return Ok(line.clone());
        }
        let line = LineClient::connect(&self.socket).map_err(|error| {
            Asked::Offline(format!(
                "this computer's daemon does not answer at {} ({error})",
                self.socket.display()
            ))
        })?;
        *self.line.lock_recover() = Some(line.clone());
        Ok(line)
    }
}

impl Ask for Daemon {
    fn invoke(
        &self,
        workspace: Option<&str>,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, Asked> {
        let line = self.connected()?;
        match asked(&line, workspace, target.clone(), arguments.clone()) {
            Err(Asked::Offline(_)) => {
                // The connection ended: the daemon restarted, or went away. Ask once more on a
                // new one, so a restart is not seen as a refusal.
                *self.line.lock_recover() = None;
                let line = self.connected()?;
                asked(&line, workspace, target, arguments)
            }
            answer => answer,
        }
    }
}

/// `target` asked on `line` as a connection of the person's own in `workspace`.
fn asked(
    line: &LineClient,
    workspace: Option<&str>,
    target: RemoteActionTarget,
    arguments: Map<String, Value>,
) -> Result<Value, Asked> {
    let caller = match workspace {
        Some(workspace) => Caller::local_person("").in_workspace(workspace),
        None => Caller::local_person(""),
    };
    let request = InvokeRequest::plain(target, arguments);
    let receipt = line
        .invoke(&caller, request)
        .wait_timeout(ANSWER_WAIT)
        .ok()
        .flatten()
        .ok_or_else(|| Asked::Offline("the daemon did not answer".to_owned()))?
        .map_err(|error| match error {
            ActionCallError::Unavailable { reason } => Asked::Offline(reason),
            other => Asked::Refused(other.own_words()),
        })?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Failed { error } => Err(Asked::Refused(error.message)),
        ReceiptStatus::Rejected { error } => Err(Asked::Refused(error.own_words())),
        ReceiptStatus::Cancelled { .. } => Err(Asked::Refused("The call was stopped".to_owned())),
        ReceiptStatus::Accepted => Err(Asked::Offline("the daemon did not answer yet".to_owned())),
    }
}

/// The accounts and sign-ins the daemon of this computer tells.
pub struct SessionAccounts {
    inner: Arc<Inner>,
}

struct Inner {
    ask: Arc<dyn Ask>,
    /// The last accounts the daemon told, and the revision they are.
    latest: Mutex<Option<(u64, Vec<Held>)>>,
}

impl SessionAccounts {
    /// The accounts the daemon at this computer's native endpoint tells.
    ///
    /// # Errors
    /// This computer's daemon socket is not known.
    pub fn new() -> Result<Self, String> {
        Ok(Self::over(Arc::new(Daemon::here()?)))
    }

    /// The accounts the daemon behind `ask` tells.
    #[must_use]
    pub fn over(ask: Arc<dyn Ask>) -> Self {
        Self {
            inner: Arc::new(Inner {
                ask,
                latest: Mutex::new(None),
            }),
        }
    }
}

impl Inner {
    /// Ask for the accounts and keep what is told. Whether they changed since `after`.
    fn fetch(&self) -> Result<bool, Asked> {
        let list = self.ask.invoke(
            None,
            account_target("account/workspaces", "list"),
            Map::new(),
        )?;
        let mut accounts = Vec::new();
        for workspace in workspaces_of(&list) {
            let listed = match self.ask.invoke(
                Some(&workspace),
                remote(person_accounts::RESOURCE, person_accounts::CLOUD_ACCOUNTS),
                Map::new(),
            ) {
                Ok(listed) => listed,
                // A workspace this device does not hold now, or does not let it read: the
                // others still mount.
                Err(Asked::Refused(_)) => continue,
                Err(offline) => return Err(offline),
            };
            accounts.extend(held_in(&workspace, &listed));
        }
        let revision = revision_of(&accounts);
        let mut latest = self.latest.lock_recover();
        let changed = latest.as_ref().is_none_or(|(held, _)| *held != revision);
        if changed {
            tracing::info!(
                accounts = accounts.len(),
                "the daemon told other file accounts"
            );
        }
        *latest = Some((revision, accounts));
        Ok(changed)
    }

    /// What the daemon last told of `account`.
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

/// The target of `action` of `resource` under the accounts provider.
fn remote(resource: &str, action: &str) -> RemoteActionTarget {
    target(person_accounts::PROVIDER, resource, action)
}

/// The target of `action` of `resource` under the person's account provider.
fn account_target(resource: &str, action: &str) -> RemoteActionTarget {
    target("tilo.account", resource, action)
}

fn target(provider: &str, resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: provider.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// The workspaces the account's list names, as the daemon calls them.
fn workspaces_of(listed: &Value) -> Vec<String> {
    listed
        .get("workspaces")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |workspaces| {
            workspaces
                .iter()
                .filter(|workspace| {
                    workspace
                        .get("kept_here")
                        .and_then(Value::as_bool)
                        .unwrap_or_default()
                })
                .filter_map(|workspace| {
                    workspace
                        .get("id")
                        .or_else(|| workspace.get("workspace"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
}

/// The file accounts `listed` holds for `workspace`.
fn held_in(workspace: &str, listed: &Value) -> Vec<Held> {
    listed
        .get("accounts")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |accounts| {
            accounts
                .iter()
                .filter_map(|listed| {
                    let account: Account =
                        serde_json::from_value(listed.get("account")?.clone()).ok()?;
                    super::providers::is_file_account(&account).then(|| Held {
                        workspace: workspace.to_owned(),
                        signed_in: listed
                            .get("signedIn")
                            .and_then(Value::as_bool)
                            .unwrap_or_default(),
                        account,
                    })
                })
                .collect()
        })
}

/// The revision of `held`, as what changed when they are another.
fn revision_of(held: &[Held]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for held in held {
        held.workspace.hash(&mut hasher);
        held.account.id.as_str().hash(&mut hasher);
        held.account.type_id.as_str().hash(&mut hasher);
        held.account.display_name.hash(&mut hasher);
        held.account.enabled.hash(&mut hasher);
        held.account.config.to_string().hash(&mut hasher);
        held.signed_in.hash(&mut hasher);
    }
    hasher.finish()
}

impl AccountSource for SessionAccounts {
    fn accounts(&self) -> Vec<Account> {
        // Read anew: a removal must not wait for the watch to tell it.
        if let Err(error) = self.inner.fetch() {
            tracing::info!(?error, "the daemon does not tell the file accounts now");
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
        reason = "the watch asks the daemon on a thread of its own"
    )]
    fn watch(&self, changed: Box<dyn Fn() + Send + Sync>) -> io::Result<AccountsWatch> {
        let stop = Arc::new(AtomicBool::new(false));
        let inner = Arc::clone(&self.inner);
        let stopped = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("cloud-accounts".into())
            .spawn(move || {
                while !stopped.load(Ordering::SeqCst) {
                    match inner.fetch() {
                        Ok(true) if !stopped.load(Ordering::SeqCst) => changed(),
                        Ok(_) => {}
                        Err(error) => {
                            tracing::info!(?error, "the daemon's file accounts are not told yet");
                            std::thread::sleep(ASK_AGAIN);
                        }
                    }
                    std::thread::sleep(ASK_AGAIN);
                }
            })?;
        Ok(AccountsWatch { stop })
    }

    fn credential(&self, account: &Account) -> Arc<dyn CredentialSource> {
        let workspace = self.inner.workspace_of(&account.id);
        Arc::new(ProviderCredential {
            inner: Arc::clone(&self.inner),
            account: account.id.clone(),
            workspace,
            rejected: AtomicBool::new(false),
        })
    }

    fn report(&self, account: &AccountId, health: SyncHealth) {
        let Some(workspace) = self.inner.workspace_of(account) else {
            return;
        };
        let mut arguments = Map::new();
        arguments.insert("account".into(), Value::String(account.as_str().to_owned()));
        arguments.insert("health".into(), Value::String(health.word().to_owned()));
        arguments.insert("message".into(), Value::String(health.summary().to_owned()));
        let ask = remote(person_accounts::RESOURCE, person_accounts::REPORT_SYNC_VERB);
        match self.inner.ask.invoke(Some(&workspace), ask, arguments) {
            Ok(_) => {}
            Err(error) => {
                tracing::info!(?error, "the daemon took no sync report of a file account")
            }
        }
    }

    fn signed_in(&self, account: &AccountId) -> bool {
        self.inner
            .held(account, |held| held.signed_in)
            .unwrap_or_default()
    }
}

/// An account's sign-in, asked of the daemon each time the mount needs one, and kept in this
/// process's memory only until then.
struct ProviderCredential {
    inner: Arc<Inner>,
    account: AccountId,
    workspace: Option<String>,
    /// The mount refused the last sign-in: the daemon renews the grant once more.
    rejected: AtomicBool,
}

impl CredentialSource for ProviderCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
        let rejected = self.rejected.swap(false, Ordering::SeqCst);
        let asked = self.ask(rejected);
        match asked {
            Ok(value) => credential_of(&value),
            Err(Asked::Offline(error)) => {
                tracing::info!(
                    ?error,
                    "the daemon does not answer a file account's sign-in"
                );
                Err(cloud_source::Error::Offline)
            }
            Err(Asked::Refused(error)) => {
                tracing::info!(?error, "the daemon released no sign-in for a file account");
                Err(cloud_source::Error::Unauthorized)
            }
        }
    }

    fn rejected(&self) {
        self.rejected.store(true, Ordering::SeqCst);
    }
}

impl ProviderCredential {
    fn ask(&self, rejected: bool) -> Result<Value, Asked> {
        let Some(workspace) = self.workspace.clone() else {
            return Err(Asked::Refused(
                "That account is not one this device mounts".to_owned(),
            ));
        };
        let mut arguments = Map::new();
        arguments.insert(
            "account".into(),
            Value::String(self.account.as_str().to_owned()),
        );
        if rejected {
            arguments.insert("rejected".into(), Value::Bool(true));
        }
        self.inner.ask.invoke(
            Some(&workspace),
            remote(person_accounts::RESOURCE, person_accounts::CLOUD_CREDENTIAL),
            arguments,
        )
    }
}

/// The credential the daemon answered, as a mount reads it.
fn credential_of(answered: &Value) -> cloud_source::Result<Credential> {
    let kind = answered
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(cloud_source::Error::Unauthorized)?;
    let string = |name: &str| -> String {
        answered
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let expires = answered
        .get("expiresAtMs")
        .and_then(Value::as_u64)
        .and_then(|millis| std::time::UNIX_EPOCH.checked_add(Duration::from_millis(millis)));
    match kind {
        "basic" => Ok(Credential::Basic {
            username: string("username"),
            password: Zeroizing::new(string("secret")),
        }),
        "bearer" => Ok(Credential::Bearer {
            token: Zeroizing::new(string("secret")),
            expires,
        }),
        "session" => Ok(Credential::Session {
            headers: answered
                .get("headers")
                .and_then(Value::as_array)
                .map_or_else(Vec::new, |headers| {
                    headers
                        .iter()
                        .filter_map(|header| {
                            Some((
                                header.get("name")?.as_str()?.to_owned(),
                                Zeroizing::new(header.get("value")?.as_str()?.to_owned()),
                            ))
                        })
                        .collect()
                }),
            expires,
        }),
        _ => Err(cloud_source::Error::Unauthorized),
    }
}
