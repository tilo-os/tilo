//! Recording an account in a home's accounts, its secret in the vault.
//!
//! The accounts are the workspace's `tilo.accounts` record ([`PersonAccounts`]) as the home
//! projects it: code here keeps the projection as it stands beside the destination and lands each
//! edit into it, so a reader of the home and the workspace's record agree. A secret is sealed in
//! the vault under the same root before the account that names it is landed, so a crash leaves a
//! sealed orphan, never an account without its secret.
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use accounts::person::PersonAccounts;
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
use migration_sources::vault::VaultError;
use tilo_resource::{Invoke, Replica};

pub use migration_sources::vault::PrivateSecret;

/// `config_root`'s accounts as the record stands there, or none where it keeps none.
#[must_use]
pub fn person_accounts(config_root: &Path) -> PersonAccounts {
    Accounts::read(config_root)
        .ok()
        .and_then(|accounts| accounts.state().ok())
        .unwrap_or_default()
}

/// Read `config_root`'s accounts as a replica a caller can land edits into and write back, for a
/// step the installer runs before the workspace's session starts. Not shared between threads.
///
/// # Errors
/// The projection does not read.
pub fn replica_in(config_root: &Path) -> Result<Replica<PersonAccounts>, ConnectionError> {
    let mut record = Replica::<PersonAccounts>::new();
    for account in Accounts::read(config_root)?.all()? {
        record
            .apply_confirmed(PersonAccounts::saving(&account))
            .map_err(|_| ConnectionError::StorageUnavailable)?;
    }
    Ok(record)
}

/// Write `record`'s accounts as `config_root`'s projection.
///
/// # Errors
/// The record does not read, or the projection does not write.
pub fn project(
    config_root: &Path,
    record: &mut Replica<PersonAccounts>,
) -> Result<(), ConnectionError> {
    record
        .state()
        .map_err(|_| ConnectionError::StorageUnavailable)?
        .project_in(config_root)
        .map(|_| ())
        .map_err(|_| ConnectionError::StorageUnavailable)
}

/// Details deliberately omit credentials and provider response bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionError {
    SignInUnavailable,
    SignInFailed,
    InvalidConfiguration,
    StorageUnavailable,
    AccountMissing,
    CredentialUnavailable,
    AlreadyPresent,
    DavAuthenticationRefused,
    DavEndpointUnavailable,
    DavFilesUnavailable,
}

impl From<VaultError> for ConnectionError {
    fn from(_: VaultError) -> Self {
        Self::StorageUnavailable
    }
}

/// The accounts of one home, as its projection holds them, with each change kept into it. The
/// secrets are the vault's, under the same root.
///
/// The projection is the [`AccountStore`] every reader of a home's accounts takes, so it carries
/// each account's `owner` as the file does: migration review and credentials refuse an account
/// another member contributed, and a projection that dropped that field would adopt it. The
/// workspace's [`PersonAccounts`] record keeps the same accounts without `owner`; [`Self::state`]
/// answers that shape for the callers that land edits into it.
#[derive(Clone, Default)]
pub struct Accounts {
    record: Arc<Mutex<AccountStore>>,
}

impl Accounts {
    /// No accounts yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The accounts of `config_root`'s projection. None when it keeps none.
    ///
    /// # Errors
    /// The projection does not read.
    pub fn read(config_root: &Path) -> Result<Self, ConnectionError> {
        Self::read_within(config_root, config_root)
    }

    /// The accounts of `config_root`'s projection, refusing a store reached through a link
    /// anywhere below `boundary`. A source home's `tilo` folder is consented as part of a wider
    /// root, so a redirected folder must not be read.
    ///
    /// # Errors
    /// The projection does not read, or its path leaves `boundary`.
    pub fn read_within(boundary: &Path, config_root: &Path) -> Result<Self, ConnectionError> {
        let text =
            crate::account_sources::metadata(boundary, &accounts::store_path_in(config_root))
                .map_err(|()| ConnectionError::StorageUnavailable)?;
        let Some(text) = text else {
            return Ok(Self::empty());
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| ConnectionError::StorageUnavailable)?;
        let store = AccountStore::new();
        let accounts = Self {
            record: Arc::new(Mutex::new(store)),
        };
        for account in value
            .get("accounts")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let account: Account = serde_json::from_value(account.clone())
                .map_err(|_| ConnectionError::StorageUnavailable)?;
            accounts.keep(&account)?;
        }
        Ok(accounts)
    }

    fn lock(&self) -> MutexGuard<'_, AccountStore> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accounts as the workspace's record holds them.
    ///
    /// # Errors
    /// An account names no id or no kind.
    pub fn state(&self) -> Result<PersonAccounts, ConnectionError> {
        let store = self.lock();
        let mut record = PersonAccounts::default();
        for account in store.all() {
            record
                .keep(account)
                .map_err(|_| ConnectionError::StorageUnavailable)?;
        }
        Ok(record)
    }

    /// Keep `account`, added or in place of the one with its id.
    ///
    /// # Errors
    /// The account names no id or no kind.
    pub fn keep(&self, account: &Account) -> Result<(), ConnectionError> {
        if account.id.as_str().trim().is_empty() || account.type_id.as_str().trim().is_empty() {
            return Err(ConnectionError::StorageUnavailable);
        }
        self.lock().upsert(account.clone());
        Ok(())
    }

    /// Remove the account `id`. Whether the record held it.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn remove(&self, id: &str) -> Result<bool, ConnectionError> {
        Ok(self.lock().remove(&AccountId::new(id)))
    }

    /// The account `id`, when the record keeps it.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn get(&self, id: &str) -> Result<Option<Account>, ConnectionError> {
        Ok(self.lock().get(&AccountId::new(id)).cloned())
    }

    /// Every account, in the order the record keeps them.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn all(&self) -> Result<Vec<Account>, ConnectionError> {
        Ok(self.lock().all().to_vec())
    }

    /// Whether the record holds no account.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn is_empty(&self) -> Result<bool, ConnectionError> {
        Ok(self.lock().is_empty())
    }

    /// Write the accounts as `config_root`'s projection, for the readers that still read it.
    ///
    /// # Errors
    /// The projection does not write.
    pub fn project(&self, config_root: &Path) -> Result<(), ConnectionError> {
        self.lock()
            .save_in(config_root)
            .map_err(|_| ConnectionError::StorageUnavailable)
    }
}

/// Publish an imported grant, or verify that an existing grant is identical. The caller supplies
/// the record as it stands, and lands the edits it answers into it.
///
/// # Errors
/// An account of the same id is already there with other details or another secret, or the vault
/// could not be written.
pub fn persist_import(
    root: &Path,
    record: &PersonAccounts,
    account: Account,
    secret: Secret,
) -> Result<Vec<Invoke<PersonAccounts>>, ConnectionError> {
    let secret = PrivateSecret(secret);
    if let Some(existing) = record.account(account.id.as_str()) {
        if existing != account {
            return Err(ConnectionError::AlreadyPresent);
        }
        let vault = SecretVault::open_in(root);
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::StorageUnavailable);
        }
        let stored = PrivateSecret(
            vault
                .get(&account.id)
                .ok_or(ConnectionError::CredentialUnavailable)?,
        );
        if stored.0 != secret.0 && !account_credentials::copied_source_matches(&stored.0, &secret.0)
        {
            return Err(ConnectionError::AlreadyPresent);
        }
        return Ok(vec![PersonAccounts::saving(&account)]);
    }
    seal_and_edit(root, &account, &secret.0)
}

/// Seal a grant's secret in the vault before making its account visible in the record. The caller
/// lands the edits it answers.
///
/// # Errors
/// The vault could not be read or written.
pub fn persist(
    root: &Path,
    record: &PersonAccounts,
    account: Account,
    secret: Secret,
) -> Result<Vec<Invoke<PersonAccounts>>, ConnectionError> {
    if record.account(account.id.as_str()).is_some() {
        return Err(ConnectionError::AlreadyPresent);
    }
    let secret = PrivateSecret(secret);
    seal_and_edit(root, &account, &secret.0)
}

fn seal_and_edit(
    root: &Path,
    account: &Account,
    secret: &Secret,
) -> Result<Vec<Invoke<PersonAccounts>>, ConnectionError> {
    // The vault write must succeed before the account is landed. A crash between the two leaves
    // a sealed orphan, never a plaintext credential or an account without its grant.
    migration_sources::vault::seal(root, &account.id, secret)?;
    Ok(vec![PersonAccounts::saving(account)])
}
