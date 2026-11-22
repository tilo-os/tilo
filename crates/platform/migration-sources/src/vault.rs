//! Publishing an account's metadata only after its secret is sealed in the vault.

use std::path::Path;

use accounts::{Account, AccountId, Secret, SecretVault};

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

/// Seal `secret` in the vault for `account`, saying its id. The account is the workspace's
/// record's from then on: the caller keeps it there, so a crash between the two leaves a sealed
/// orphan, never a plaintext credential or an apparently connected account without its secret.
///
/// # Errors
/// The vault could not be read or written (`E: From<VaultError>`).
pub fn persist<E>(root: &Path, account: &Account, secret: Secret) -> Result<AccountId, E>
where
    E: From<VaultError>,
{
    let secret = PrivateSecret(secret);
    seal(root, &account.id, &secret.0)?;
    Ok(account.id.clone())
}
