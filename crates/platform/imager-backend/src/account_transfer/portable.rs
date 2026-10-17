//! Selected account transport using canonical symmetric or password-wrapped
//! envelopes. Password artifacts wrap a fresh content key without exporting a
//! source vault key. PreparedSetup/installer writer integration is caller work.
use std::collections::BTreeSet;
use std::io::{Read, Seek, Write};
use std::path::Path;

use accounts::{Account, AccountId, Secret, SecretVault};
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, PasswordParams, SymmetricKey, envelope};
use zeroize::{Zeroize, Zeroizing};

use super::connect::{PrivateSecret, load_store};
use super::{ConnectionError, Connector};

const MAX_ACCOUNTS: usize = 64;
const MAX_PLAINTEXT: usize = 8 * 1024 * 1024;
const MAX_SEALED: usize = MAX_PLAINTEXT + 256;
const DOMAIN: &[u8] = b"tilo-prepared-accounts/v1\0";

/// Ciphertext only. No key, source custody, plaintext credential or public
/// resource-state serialization lives in this object.
pub struct SealedAccounts(Vec<u8>);
impl std::fmt::Debug for SealedAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedAccounts")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}
impl SealedAccounts {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Parse only a bounded sealed artifact. Authentication occurs on import.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectionError> {
        if bytes.len() > MAX_SEALED {
            return Err(ConnectionError::InvalidConfiguration);
        }
        envelope::Envelope::parse(bytes).map_err(|_| ConnectionError::CredentialUnavailable)?;
        Ok(Self(bytes.to_vec()))
    }
}

const ARTIFACT_MAGIC: &[u8; 8] = b"TILOACC1";
const WRAP_DOMAIN: &[u8] = b"tilo-prepared-account-key/v1\0";
const MAX_WRAPPED: usize = 256;
const MAX_ARTIFACT: usize = 16 + MAX_WRAPPED + MAX_SEALED;

/// A portable password-wrapped artifact: sealed selected accounts and a fresh
/// wrapped transport key. The password and unwrapped key are never serialized.
pub struct PasswordSealedAccounts(Vec<u8>);
impl std::fmt::Debug for PasswordSealedAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordSealedAccounts")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}
impl PasswordSealedAccounts {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectionError> {
        parse_artifact(bytes)?;
        Ok(Self(bytes.to_vec()))
    }
    /// Read one explicitly granted private native file with bounded/no-follow
    /// access. Readers can also use from_bytes for an enclosing setup package.
    pub fn read(path: &Path) -> Result<Self, ConnectionError> {
        read_artifact(path).map_err(|_| ConnectionError::StorageUnavailable)
    }
    fn open(
        &self,
        password: &[u8],
        setup: &[u8],
    ) -> Result<(SealedAccounts, SymmetricKey), ConnectionError> {
        validate_password(password)?;
        let context = wrap_binding(setup)?;
        let (wrapped, selected) = parse_artifact(&self.0)?;
        let key = envelope::open_with_password(
            password,
            Context::new(Label::MigrationPackage, &context),
            wrapped,
        )
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let key: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        Ok((
            SealedAccounts::from_bytes(selected)?,
            SymmetricKey::from_bytes(key),
        ))
    }
}
fn validate_password(password: &[u8]) -> Result<(), ConnectionError> {
    if password.len() > 4096
        || std::str::from_utf8(password).map_or(true, |text| text.chars().count() < 15)
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(())
}
fn wrap_binding(setup: &[u8]) -> Result<Vec<u8>, ConnectionError> {
    binding(setup)?;
    let mut context = WRAP_DOMAIN.to_vec();
    context.extend_from_slice(setup);
    Ok(context)
}
fn parse_artifact(bytes: &[u8]) -> Result<(&[u8], &[u8]), ConnectionError> {
    if bytes.len() > MAX_ARTIFACT || bytes.get(..8) != Some(ARTIFACT_MAGIC) {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let length = |offset: usize| -> Result<usize, ConnectionError> {
        let bytes: [u8; 4] = bytes
            .get(offset..offset + 4)
            .ok_or(ConnectionError::InvalidConfiguration)?
            .try_into()
            .map_err(|_| ConnectionError::InvalidConfiguration)?;
        Ok(u32::from_le_bytes(bytes) as usize)
    };
    let wrapped_len = length(8)?;
    if wrapped_len > MAX_WRAPPED {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let wrapped = bytes
        .get(12..12 + wrapped_len)
        .ok_or(ConnectionError::InvalidConfiguration)?;
    let selected_len = length(12 + wrapped_len)?;
    let start = 16 + wrapped_len;
    if selected_len > MAX_SEALED || bytes.len() != start + selected_len {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let parsed =
        envelope::Envelope::parse(wrapped).map_err(|_| ConnectionError::CredentialUnavailable)?;
    // Reject untrusted expensive KDF costs before deriving any key. Version 1
    // writes this one canonical policy, just like the migration-package codec.
    if !matches!(parsed.source, envelope::KeySource::Password { params, salt } if params == PasswordParams::DEFAULT && salt.len() == envelope::SALT_LEN)
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let selected = &bytes[start..];
    if !matches!(
        envelope::Envelope::parse(selected)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .source,
        envelope::KeySource::Key(_)
    ) {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok((wrapped, selected))
}
fn read_artifact(path: &Path) -> std::io::Result<PasswordSealedAccounts> {
    use crate::{destination_security, migration_directory::PinnedDirectory};
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| std::io::Error::other("Artifact parent missing"))?,
    )?;
    destination_security::check_private_file(&parent.file)?;
    parent.validate_destination()?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(
        &parent.file,
        Path::new(
            path.file_name()
                .ok_or_else(|| std::io::Error::other("Artifact name missing"))?,
        ),
        &options,
    )?;
    destination_security::check_private_file(&file)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_ARTIFACT as u64 {
        return Err(std::io::Error::other(
            "Artifact exceeds bounds or is not a file",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_ARTIFACT as u64 + 1)
        .read_to_end(&mut bytes)?;
    parent.validate()?;
    PasswordSealedAccounts::from_bytes(&bytes)
        .map_err(|_| std::io::Error::other("Malformed account artifact"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    account: Account,
    secret: Secret,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.secret.password.zeroize();
        if let Some(token) = &mut self.secret.token {
            token.zeroize();
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    version: u8,
    entries: Vec<Entry>,
}
fn binding(setup: &[u8]) -> Result<Vec<u8>, ConnectionError> {
    if setup.is_empty() || setup.len() > 4096 {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut context = DOMAIN.to_vec();
    context.extend_from_slice(setup);
    Ok(context)
}
struct BoundedPlain<'a>(&'a mut Vec<u8>);
impl std::io::Write for BoundedPlain<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_PLAINTEXT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Selected account transport exceeds bounds",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn valid_entries(entries: &[Entry]) -> Result<(), ConnectionError> {
    if entries.is_empty() || entries.len() > MAX_ACCOUNTS {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut ids = BTreeSet::new();
    for entry in entries {
        if entry.account.owner.is_some()
            || entry.account.id.as_str().is_empty()
            || entry.account.id.as_str().len() > 256
            || entry.secret.is_empty()
            || entry.secret.password.len() > MAX_PLAINTEXT
            || entry
                .secret
                .token
                .as_ref()
                .is_some_and(|token| token.len() > MAX_PLAINTEXT)
            || !ids.insert(entry.account.id.as_str())
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
    }
    Ok(())
}
impl Connector {
    /// Produce an actual portable selected-account artifact with a fresh
    /// content key wrapped under the person's explicit transport password.
    pub fn seal_selected_accounts_with_password(
        &self,
        selected: &[AccountId],
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<PasswordSealedAccounts, ConnectionError> {
        validate_password(password)?;
        let context = wrap_binding(setup_binding)?;
        let key = SymmetricKey::generate().map_err(|_| ConnectionError::CredentialUnavailable)?;
        let selected = self.seal_selected_accounts(selected, &key, setup_binding)?;
        let wrapped = envelope::seal_with_password(
            password,
            PasswordParams::DEFAULT,
            Context::new(Label::MigrationPackage, &context),
            key.expose_secret(),
        )
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let mut bytes = Vec::with_capacity(16 + wrapped.len() + selected.0.len());
        bytes.extend_from_slice(ARTIFACT_MAGIC);
        bytes.extend_from_slice(
            &u32::try_from(wrapped.len())
                .map_err(|_| ConnectionError::InvalidConfiguration)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&wrapped);
        bytes.extend_from_slice(
            &u32::try_from(selected.0.len())
                .map_err(|_| ConnectionError::InvalidConfiguration)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(selected.as_bytes());
        PasswordSealedAccounts::from_bytes(&bytes)
    }
    pub fn import_password_sealed_accounts(
        &self,
        artifact: &PasswordSealedAccounts,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        let (selected, key) = artifact.open(password, setup_binding)?;
        self.import_sealed_accounts(&selected, &key, setup_binding)
    }

    /// Import an authenticated artifact only when its complete account ID set
    /// exactly matches the caller's authenticated setup manifest. Rejects
    /// missing, extra or duplicate requested IDs before touching target custody.
    /// The caller owns/validates the target user's private configuration root.
    pub fn import_selected_accounts_matching(
        &self,
        path: &Path,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        let artifact = PasswordSealedAccounts::read(path)?;
        let bundle = matching_bundle(&artifact, password, setup_binding, expected)?;
        self.import_bundle(bundle)
    }

    /// In-memory authenticated setup consumer; no credential bytes escape.
    pub fn import_password_sealed_accounts_matching(
        &self,
        artifact: &PasswordSealedAccounts,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<Vec<AccountId>, ConnectionError> {
        self.import_bundle(matching_bundle(
            artifact,
            password,
            setup_binding,
            expected,
        )?)
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
        self.import_password_sealed_accounts(
            &PasswordSealedAccounts::read(path)?,
            password,
            setup_binding,
        )
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
        let context = binding(setup_binding)?;
        if selected.is_empty() || selected.len() > MAX_ACCOUNTS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let store = load_store(&self.root)?;
        let vault = SecretVault::open_in(&self.root);
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let mut entries = Vec::with_capacity(selected.len());
        for id in selected {
            let account = store
                .get(id)
                .filter(|account| account.owner.is_none())
                .cloned()
                .ok_or(ConnectionError::AccountMissing)?;
            let secret = vault
                .get(id)
                .ok_or(ConnectionError::CredentialUnavailable)?;
            entries.push(Entry { account, secret });
        }
        valid_entries(&entries)?;
        let mut plain = Zeroizing::new(Vec::new());
        serde_json::to_writer(
            &mut BoundedPlain(&mut plain),
            &Bundle {
                version: 1,
                entries,
            },
        )
        .map_err(|_| ConnectionError::InvalidConfiguration)?;
        if plain.len() > MAX_PLAINTEXT {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let sealed = envelope::seal(key, Context::new(Label::MigrationPackage, &context), &plain)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        Ok(SealedAccounts(sealed))
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
        self.import_bundle(open_bundle(sealed, key, setup_binding)?)
    }

    fn import_bundle(&self, bundle: Bundle) -> Result<Vec<AccountId>, ConnectionError> {
        let store = load_store(&self.root)?;
        let vault = SecretVault::open_in(&self.root);
        if vault.recovery_error().is_some() {
            return Err(ConnectionError::StorageUnavailable);
        }
        // Preflight every entry before importing the first. A rejected later
        // account must not leave a partially accepted selected bundle behind.
        for entry in &bundle.entries {
            let existing = store.get(&entry.account.id);
            if existing.is_some_and(|account| account != &entry.account) {
                return Err(ConnectionError::AlreadyPresent);
            }
            let stored = vault.get(&entry.account.id).map(PrivateSecret);
            if stored
                .as_ref()
                .is_some_and(|stored| stored.0 != entry.secret)
            {
                return Err(ConnectionError::AlreadyPresent);
            }
            if existing.is_some() && stored.is_none() {
                return Err(ConnectionError::CredentialUnavailable);
            }
        }
        let mut imported = Vec::with_capacity(bundle.entries.len());
        for entry in &bundle.entries {
            imported.push(self.persist_import(entry.account.clone(), entry.secret.clone())?);
        }
        Ok(imported)
    }
}

fn open_bundle(
    sealed: &SealedAccounts,
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<Bundle, ConnectionError> {
    let context = binding(setup_binding)?;
    if sealed.0.len() > MAX_SEALED {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let plain = envelope::open(
        key,
        Context::new(Label::MigrationPackage, &context),
        &sealed.0,
    )
    .map_err(|_| ConnectionError::CredentialUnavailable)?;
    if plain.len() > MAX_PLAINTEXT {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let bundle: Bundle =
        serde_json::from_slice(&plain).map_err(|_| ConnectionError::InvalidConfiguration)?;
    if bundle.version != 1 {
        return Err(ConnectionError::InvalidConfiguration);
    }
    valid_entries(&bundle.entries)?;
    Ok(bundle)
}

fn matching_bundle(
    artifact: &PasswordSealedAccounts,
    password: &[u8],
    setup_binding: &[u8],
    expected: &[AccountId],
) -> Result<Bundle, ConnectionError> {
    if expected.is_empty() || expected.len() > MAX_ACCOUNTS {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let requested: BTreeSet<_> = expected.iter().map(AccountId::as_str).collect();
    if requested.len() != expected.len() {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let (selected, key) = artifact.open(password, setup_binding)?;
    let bundle = open_bundle(&selected, &key, setup_binding)?;
    let present: BTreeSet<_> = bundle
        .entries
        .iter()
        .map(|entry| entry.account.id.as_str())
        .collect();
    if requested != present {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(bundle)
}

impl PasswordSealedAccounts {
    /// Inspect a manifest's exact string-valued selection after authenticating
    /// its sealed artifact. No account identifiers or credentials escape.
    ///
    /// # Errors
    /// Refuses unreadable artifacts, incorrect passwords, bindings or selections.
    pub fn inspect_prepared_consumer(
        path: &Path,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[String],
    ) -> Result<bool, ConnectionError> {
        let artifact = Self::read(path)?;
        let ids: Vec<AccountId> = expected.iter().cloned().map(AccountId::new).collect();
        artifact.requires_protected_consumer(password, setup_binding, &ids)
    }

    /// Inspect the exact authenticated selection's consumer requirement.
    /// # Errors
    /// Refuses incorrect passwords, binding, selection or invalid sealed custody.
    pub fn requires_protected_consumer(
        &self,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<bool, ConnectionError> {
        let bundle = matching_bundle(self, password, setup_binding, expected)?;
        Ok(bundle
            .entries
            .iter()
            .any(|entry| account_custody::requires_protected_consumer(&entry.secret)))
    }
    /// Authenticate and check exact manifest selection without creating any
    /// target metadata or vault. Used before accepting prepared setup custody.
    pub fn verify_selection(
        &self,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<(), ConnectionError> {
        matching_bundle(self, password, setup_binding, expected).map(drop)
    }
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
