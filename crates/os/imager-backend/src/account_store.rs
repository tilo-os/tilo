//! A home's accounts: the workspace's `tilo.accounts` record ([`AccountList`]) as a home stages it
//! before its workspace exists. Code here keeps the staged record beside the destination and lands
//! each edit into it, so a reader of the home and the workspace's record agree.
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use accounts::person::{self, AccountList};
use accounts::{Account, AccountId};
use tilo_migrate::vault::VaultError;
use tilo_resource::Replica;

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

/// The accounts of one home, as the record it stages holds them, with each change kept into it.
/// The secrets are the vault's, under the same root.
#[derive(Clone, Default)]
pub struct Accounts {
    record: Arc<Mutex<AccountList>>,
}

impl Accounts {
    /// No accounts yet.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The accounts of `config_root`'s staged record. None when it keeps none.
    ///
    /// # Errors
    /// The staged record does not read.
    pub fn read(config_root: &Path) -> Result<Self, ConnectionError> {
        Self::read_within(config_root, config_root)
    }

    /// The accounts of `config_root`'s staged record, refusing a record reached through a link
    /// anywhere below `boundary`. A source home's `tilo` folder is consented as part of a wider
    /// root, so a redirected folder must not be read.
    ///
    /// # Errors
    /// The staged record does not read, or its path leaves `boundary`.
    pub fn read_within(boundary: &Path, config_root: &Path) -> Result<Self, ConnectionError> {
        let bytes = tilo_migrate::engine::accounts::metadata_bytes(
            boundary,
            &person::staged_path_in(config_root),
        )
        .map_err(|()| ConnectionError::StorageUnavailable)?;
        let Some(bytes) = bytes else {
            return Ok(Self::empty());
        };
        let record = Replica::<AccountList>::load(&bytes)
            .map_err(|_| ConnectionError::StorageUnavailable)?
            .state()
            .map_err(|_| ConnectionError::StorageUnavailable)?;
        Ok(Self {
            record: Arc::new(Mutex::new(record)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, AccountList> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accounts as the workspace's record holds them.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn state(&self) -> Result<AccountList, ConnectionError> {
        Ok(self.lock().clone())
    }

    /// Keep `account`, added or in place of the one with its id.
    ///
    /// # Errors
    /// The account names no id or no kind.
    pub fn keep(&self, account: &Account) -> Result<(), ConnectionError> {
        if account.id.as_str().trim().is_empty() || account.type_id.as_str().trim().is_empty() {
            return Err(ConnectionError::StorageUnavailable);
        }
        self.lock()
            .keep(account)
            .map_err(|_| ConnectionError::StorageUnavailable)
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
        Ok(self.lock().account(id))
    }

    /// Every account, in the order the record keeps them.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn all(&self) -> Result<Vec<Account>, ConnectionError> {
        Ok(self.lock().all())
    }

    /// Whether the record holds no account.
    ///
    /// # Errors
    /// Never today; kept so callers read a record the same way.
    pub fn is_empty(&self) -> Result<bool, ConnectionError> {
        Ok(self.lock().is_empty())
    }

    /// Write the accounts as `config_root`'s staged record, for the readers that read it.
    ///
    /// # Errors
    /// The staged record does not write.
    pub fn project(&self, config_root: &Path) -> Result<(), ConnectionError> {
        self.lock()
            .stage_in(config_root)
            .map_err(|_| ConnectionError::StorageUnavailable)
    }
}
