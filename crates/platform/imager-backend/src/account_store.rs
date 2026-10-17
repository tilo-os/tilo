//! Native account custody publication after the caller reviews destination metadata.
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
use std::path::Path;

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

/// A private grant whose password and token are cleared on every exit path.
pub struct PrivateSecret(pub Secret);
impl Drop for PrivateSecret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.password.zeroize();
        if let Some(token) = &mut self.0.token {
            token.zeroize();
        }
    }
}

/// Publish an imported grant, or verify that an existing grant is identical.
/// The caller supplies its pinned destination metadata reader; it is invoked
/// again before publication, just as for a newly connected account.
///
/// # Errors
/// Refuses conflicting accounts, missing credentials or unavailable storage.
pub fn persist_import(
    root: &Path,
    mut load_store: impl FnMut() -> Result<AccountStore, ConnectionError>,
    account: Account,
    secret: Secret,
) -> Result<AccountId, ConnectionError> {
    let secret = PrivateSecret(secret);
    let store = load_store()?;
    if let Some(existing) = store.get(&account.id) {
        if existing != &account {
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
        if stored.0 != secret.0 && !account_custody::copied_source_matches(&stored.0, &secret.0) {
            return Err(ConnectionError::AlreadyPresent);
        }
        return Ok(account.id);
    }
    persist_private(root, &mut load_store, account, &secret.0)
}

/// Publish a grant to the native vault before making its metadata visible.
/// The reader runs only after the secret is protected by zeroizing ownership.
///
/// # Errors
/// Refuses unreadable metadata, vault recovery failures or failed durable writes.
pub fn persist(
    root: &Path,
    mut load_store: impl FnMut() -> Result<AccountStore, ConnectionError>,
    account: Account,
    secret: Secret,
) -> Result<AccountId, ConnectionError> {
    let secret = PrivateSecret(secret);
    persist_private(root, &mut load_store, account, &secret.0)
}

fn persist_private(
    root: &Path,
    load_store: &mut impl FnMut() -> Result<AccountStore, ConnectionError>,
    account: Account,
    secret: &Secret,
) -> Result<AccountId, ConnectionError> {
    // Vault write must succeed before publishing the metadata. A crash
    // between writes leaves a sealed orphan, never a plaintext credential
    // or an apparently connected account without its grant.
    let mut store = load_store()?;
    let mut vault = SecretVault::open_in(root);
    if vault.recovery_error().is_some() {
        return Err(ConnectionError::StorageUnavailable);
    }
    vault.put(&account.id, secret);
    vault
        .try_save()
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    let id = account.id.clone();
    store.upsert(account);
    store
        .save_in(root)
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    Ok(id)
}
