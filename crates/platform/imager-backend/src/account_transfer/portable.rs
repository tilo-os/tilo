//! Selected account transport over the canonical portable-account codec.
//!
//! The envelope format, domains and custody hand-off live in
//! `account_custody::portable`. This module adds what the installer needs on
//! top: a capability-rooted read and atomic publish of the artifact file, and
//! the `Connector` view over a destination configuration root. PreparedSetup
//! and installer writer integration remain caller work.
use std::io::{Read, Seek, Write};
use std::path::Path;

use account_custody::portable::{self, MAX_ARTIFACT, PortableError};
use accounts::AccountId;
use tilo_seal::SymmetricKey;

use super::{ConnectionError, Connector};

pub use account_custody::portable::{PasswordSealedAccounts, SealedAccounts};

fn transfer_error(error: PortableError) -> ConnectionError {
    match error {
        PortableError::InvalidConfiguration | PortableError::WrongUser => {
            ConnectionError::InvalidConfiguration
        }
        PortableError::CredentialUnavailable => ConnectionError::CredentialUnavailable,
        PortableError::AccountMissing => ConnectionError::AccountMissing,
        PortableError::StorageUnavailable => ConnectionError::StorageUnavailable,
        PortableError::AlreadyPresent => ConnectionError::AlreadyPresent,
    }
}

impl Connector {
    /// Produce a portable selected-account artifact with a fresh content key
    /// wrapped under the person's explicit transport password.
    pub fn seal_selected_accounts_with_password(
        &self,
        selected: &[AccountId],
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<PasswordSealedAccounts, ConnectionError> {
        portable::seal_selected_accounts_with_password(
            &self.root,
            selected,
            password,
            setup_binding,
        )
        .map_err(transfer_error)
    }

    /// Import the accounts a password-wrapped artifact carries.
    pub fn import_password_sealed_accounts(
        &self,
        artifact: &PasswordSealedAccounts,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        portable::import_password_sealed_accounts(&self.root, artifact, password, setup_binding)
            .map_err(transfer_error)
    }

    /// Import an artifact file only when its complete account ID set exactly
    /// matches the caller's authenticated setup manifest. Rejects missing,
    /// extra or duplicate requested IDs before touching target custody.
    pub fn import_selected_accounts_matching(
        &self,
        path: &Path,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        let artifact = read_artifact(path).map_err(|_| ConnectionError::StorageUnavailable)?;
        self.import_password_sealed_accounts_matching(&artifact, password, setup_binding, expected)
    }

    /// In-memory authenticated setup consumer; no credential bytes escape.
    pub fn import_password_sealed_accounts_matching(
        &self,
        artifact: &PasswordSealedAccounts,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        portable::import_password_sealed_accounts_matching(
            &self.root,
            artifact,
            password,
            setup_binding,
            expected,
        )
        .map_err(transfer_error)
    }

    /// Publish a new private artifact atomically; an existing name is refused.
    /// Only ciphertext is ever written. The source config tree is excluded.
    pub fn export_selected_accounts(
        &self,
        selected: &[AccountId],
        path: &Path,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<(), ConnectionError> {
        let artifact =
            self.seal_selected_accounts_with_password(selected, password, setup_binding)?;
        publish_artifact(&self.root, path, &artifact)
            .map_err(|_| ConnectionError::StorageUnavailable)
    }

    pub fn import_selected_accounts(
        &self,
        path: &Path,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        let artifact = read_artifact(path).map_err(|_| ConnectionError::StorageUnavailable)?;
        self.import_password_sealed_accounts(&artifact, password, setup_binding)
    }

    /// Explicit selected local accounts only. Read credentials from canonical
    /// custody, then seal them under a fresh caller-owned transport key. Caller
    /// must wrap that key in authenticated PreparedSetup custody and keep its
    /// setup binding; neither source vault files nor source keys are portable.
    pub fn seal_selected_accounts(
        &self,
        selected: &[AccountId],
        key: &SymmetricKey,
        setup_binding: &[u8],
    ) -> Result<SealedAccounts, ConnectionError> {
        portable::seal_selected_accounts(&self.root, selected, key, setup_binding)
            .map_err(transfer_error)
    }

    /// Authenticate the complete selected bundle before any destination write,
    /// then reseal each credential under destination canonical vault custody.
    /// Exact retries succeed; conflicting metadata or credentials fail without
    /// replacing existing accounts. Caller owns/validates the destination home.
    pub fn import_sealed_accounts(
        &self,
        sealed: &SealedAccounts,
        key: &SymmetricKey,
        setup_binding: &[u8],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        portable::import_sealed_accounts(&self.root, sealed, key, setup_binding)
            .map_err(transfer_error)
    }
}

/// Inspect a manifest's exact string-valued selection after authenticating its
/// sealed artifact. No account identifiers or credentials escape.
///
/// # Errors
/// Refuses unreadable artifacts, incorrect passwords, bindings or selections.
pub fn inspect_prepared_consumer(
    path: &Path,
    password: &[u8],
    setup_binding: &[u8],
    expected: &[String],
) -> Result<bool, ConnectionError> {
    let artifact = read_artifact(path).map_err(|_| ConnectionError::StorageUnavailable)?;
    let ids: Vec<AccountId> = expected.iter().cloned().map(AccountId::new).collect();
    artifact
        .requires_protected_consumer(password, setup_binding, &ids)
        .map_err(transfer_error)
}

/// Read one explicitly granted private native file with bounded/no-follow
/// access. Readers can also use from_bytes for an enclosing setup package.
fn read_artifact(path: &Path) -> std::io::Result<PasswordSealedAccounts> {
    let bytes = crate::destination_security::read_private_file(path, MAX_ARTIFACT)?;
    PasswordSealedAccounts::from_bytes(&bytes)
        .map_err(|_| std::io::Error::other("Malformed account artifact"))
}

fn publish_artifact(
    source_root: &Path,
    path: &Path,
    artifact: &PasswordSealedAccounts,
) -> std::io::Result<()> {
    use crate::{destination_security, migration_directory::PinnedDirectory, migration_durability};
    use cap_fs_ext::OpenOptionsFollowExt;
    #[cfg(unix)]
    use cap_primitives::fs::OpenOptionsExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    migration_durability::supported()?;
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| std::io::Error::other("Artifact parent missing"))?,
    )?;
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    let source = PinnedDirectory::open_absolute(source_root)?;
    if parent.contains_identity(&source.identity) || source.contains_identity(&parent.identity) {
        return Err(std::io::Error::other(
            "Artifact must be outside source configuration",
        ));
    }
    let name = Path::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("Artifact name missing"))?,
    );
    let temporary =
        std::path::PathBuf::from(format!(".tilo-account-{}.sealed", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = cap_primitives::fs::open(&parent.file, &temporary, &options)?;
    let result = (|| {
        destination_security::check_private_file(&file)?;
        file.write_all(artifact.as_bytes())?;
        migration_durability::sync_file(&file)?;
        file.rewind()?;
        let mut readback = Vec::new();
        (&mut file)
            .take(MAX_ARTIFACT as u64 + 1)
            .read_to_end(&mut readback)?;
        if readback != artifact.as_bytes() {
            return Err(std::io::Error::other("Artifact readback failed"));
        }
        parent.validate_destination()?;
        source.validate()?;
        cap_primitives::fs::hard_link(&parent.file, &temporary, &parent.file, name)?;
        migration_durability::sync_directory(&parent.file)?;
        Ok(())
    })();
    let cleanup = cap_primitives::fs::remove_file(&parent.file, &temporary);
    cleanup?;
    migration_durability::sync_directory(&parent.file)?;
    result
}

#[cfg(test)]
mod tests;
