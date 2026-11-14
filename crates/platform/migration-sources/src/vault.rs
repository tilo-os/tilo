//! Publishing an account's metadata only after its secret is sealed in the vault.

use std::path::Path;

use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};

/// The vault or the account store could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultError {
    Storage,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the account vault is unavailable")
    }
}

impl std::error::Error for VaultError {}

/// A secret whose password and token are wiped on every exit path.
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

/// Seal `secret` under `id` in the vault at `root`.
///
/// The vault write must succeed before the metadata is published: a crash
/// between the two leaves a sealed orphan, never a plaintext credential or an
/// apparently connected account without its secret.
///
/// # Errors
/// The vault could not be read or written, or reports a recovery error.
pub fn seal(root: &Path, id: &AccountId, secret: &Secret) -> Result<(), VaultError> {
    let mut vault = SecretVault::open_in(root);
    if vault.recovery_error().is_some() {
        return Err(VaultError::Storage);
    }
    vault.put(id, secret);
    vault.try_save().map_err(|_| VaultError::Storage)
}

/// Seal `secret` in the vault, then publish `account` to the store `load_store`
/// reads. The reader runs only after the secret is protected by zeroizing
/// ownership, so a failure to read the store leaves no secret behind.
///
/// # Errors
/// The store could not be read (`E`), or the vault or store could not be
/// written (`E: From<VaultError>`).
pub fn persist<E>(
    root: &Path,
    mut load_store: impl FnMut() -> Result<AccountStore, E>,
    account: Account,
    secret: Secret,
) -> Result<AccountId, E>
where
    E: From<VaultError>,
{
    let secret = PrivateSecret(secret);
    let mut store = load_store()?;
    seal(root, &account.id, &secret.0)?;
    let id = account.id.clone();
    store.upsert(account);
    store.save_in(root).map_err(|_| VaultError::Storage)?;
    Ok(id)
}
