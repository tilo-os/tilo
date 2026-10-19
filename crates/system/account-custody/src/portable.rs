//! Bounded, encrypted transport of explicitly selected local accounts.
//!
//! The version-one envelope and domains are compatible with prepared account
//! artifacts. Filesystem publication belongs to the caller; this codec handles
//! only memory and canonical custody roots.
use std::collections::BTreeSet;

use std::path::Path;

use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, PasswordParams, SymmetricKey, envelope};
use zeroize::{Zeroize, Zeroizing};

/// A portable artifact was malformed, unauthenticated, or could not be stored.
/// Errors deliberately never include credentials or decrypted metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortableError {
    InvalidConfiguration,
    CredentialUnavailable,
    AccountMissing,
    StorageUnavailable,
    AlreadyPresent,
    WrongUser,
}
impl std::fmt::Display for PortableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "selected account transport: {self:?}")
    }
}
impl std::error::Error for PortableError {}

/// Authenticated operation, installation target and destination account identity.
/// This encodes an unambiguous binding inside the existing version-one domains.
/// The caller must obtain these identities from its authenticated install plan,
/// not from an untrusted artifact. No root account is a valid installed consumer.
#[derive(Clone, Debug)]
pub struct PortableBinding {
    bytes: Vec<u8>,
    uid: u32,
}
impl PortableBinding {
    /// Construct a target-specific binding. Empty or oversized identities and
    /// uid zero are refused.
    /// # Errors
    /// Returns `InvalidConfiguration` for an invalid identity.
    pub fn new(operation: &str, target: &str, user: &str, uid: u32) -> Result<Self, PortableError> {
        if uid == 0
            || [operation, target, user]
                .iter()
                .any(|s| s.is_empty() || s.len() > 1024)
        {
            return Err(PortableError::InvalidConfiguration);
        }
        let bytes = serde_json::to_vec(&("tilo-account-target/v1", operation, target, user, uid))
            .map_err(|_| PortableError::InvalidConfiguration)?;
        binding(&bytes)?;
        Ok(Self { bytes, uid })
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn target_uid(&self) -> u32 {
        self.uid
    }
}

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
    /// # Errors
    /// Refuses oversized, malformed or non-key-based envelopes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PortableError> {
        if bytes.len() > MAX_SEALED {
            return Err(PortableError::InvalidConfiguration);
        }
        let parsed =
            envelope::Envelope::parse(bytes).map_err(|_| PortableError::CredentialUnavailable)?;
        if !matches!(parsed.source, envelope::KeySource::Key(_)) {
            return Err(PortableError::InvalidConfiguration);
        }
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
    /// Parse a bounded password-wrapped artifact without decrypting it.
    /// # Errors
    /// Refuses invalid framing, envelope types or password-derivation parameters.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PortableError> {
        parse_artifact(bytes)?;
        Ok(Self(bytes.to_vec()))
    }
    fn open(
        &self,
        password: &[u8],
        setup: &[u8],
    ) -> Result<(SealedAccounts, SymmetricKey), PortableError> {
        validate_password(password)?;
        let context = wrap_binding(setup)?;
        let (wrapped, selected) = parse_artifact(&self.0)?;
        let key = envelope::open_with_password(
            password,
            Context::new(Label::MigrationPackage, &context),
            wrapped,
        )
        .map_err(|_| PortableError::CredentialUnavailable)?;
        let mut key: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| PortableError::CredentialUnavailable)?;
        let transport = SymmetricKey::from_bytes(key);
        key.zeroize();
        Ok((SealedAccounts::from_bytes(selected)?, transport))
    }
}
fn validate_password(password: &[u8]) -> Result<(), PortableError> {
    if password.len() > 4096
        || std::str::from_utf8(password).map_or(true, |text| text.chars().count() < 15)
    {
        return Err(PortableError::InvalidConfiguration);
    }
    Ok(())
}
fn wrap_binding(setup: &[u8]) -> Result<Vec<u8>, PortableError> {
    binding(setup)?;
    let mut context = WRAP_DOMAIN.to_vec();
    context.extend_from_slice(setup);
    Ok(context)
}
fn parse_artifact(bytes: &[u8]) -> Result<(&[u8], &[u8]), PortableError> {
    if bytes.len() > MAX_ARTIFACT || bytes.get(..8) != Some(ARTIFACT_MAGIC) {
        return Err(PortableError::InvalidConfiguration);
    }
    let length = |offset: usize| -> Result<usize, PortableError> {
        let end = offset
            .checked_add(4)
            .ok_or(PortableError::InvalidConfiguration)?;
        let bytes: [u8; 4] = bytes
            .get(offset..end)
            .ok_or(PortableError::InvalidConfiguration)?
            .try_into()
            .map_err(|_| PortableError::InvalidConfiguration)?;
        Ok(u32::from_le_bytes(bytes) as usize)
    };
    let wrapped_len = length(8)?;
    if wrapped_len > MAX_WRAPPED {
        return Err(PortableError::InvalidConfiguration);
    }
    let wrapped_end = 12_usize
        .checked_add(wrapped_len)
        .ok_or(PortableError::InvalidConfiguration)?;
    let wrapped = bytes
        .get(12..wrapped_end)
        .ok_or(PortableError::InvalidConfiguration)?;
    let selected_len = length(wrapped_end)?;
    let start = wrapped_end
        .checked_add(4)
        .ok_or(PortableError::InvalidConfiguration)?;
    if selected_len > MAX_SEALED || start.checked_add(selected_len) != Some(bytes.len()) {
        return Err(PortableError::InvalidConfiguration);
    }
    let parsed =
        envelope::Envelope::parse(wrapped).map_err(|_| PortableError::CredentialUnavailable)?;
    // Reject untrusted expensive KDF costs before deriving any key. Version 1
    // writes only this canonical policy.
    if !matches!(parsed.source, envelope::KeySource::Password { params, salt } if params == PasswordParams::DEFAULT && salt.len() == envelope::SALT_LEN)
    {
        return Err(PortableError::InvalidConfiguration);
    }
    let selected = bytes
        .get(start..)
        .ok_or(PortableError::InvalidConfiguration)?;
    if !matches!(
        envelope::Envelope::parse(selected)
            .map_err(|_| PortableError::CredentialUnavailable)?
            .source,
        envelope::KeySource::Key(_)
    ) {
        return Err(PortableError::InvalidConfiguration);
    }
    Ok((wrapped, selected))
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
fn binding(setup: &[u8]) -> Result<Vec<u8>, PortableError> {
    if setup.is_empty() || setup.len() > 4096 {
        return Err(PortableError::InvalidConfiguration);
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
fn valid_entries(entries: &[Entry]) -> Result<(), PortableError> {
    if entries.is_empty() || entries.len() > MAX_ACCOUNTS {
        return Err(PortableError::InvalidConfiguration);
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
            return Err(PortableError::InvalidConfiguration);
        }
    }
    Ok(())
}
/// Produce an actual portable selected-account artifact with a fresh
/// content key wrapped under the person's explicit transport password.
/// # Errors
/// Refuses invalid passwords, bindings or selections, unavailable custody and
/// encryption failures.
pub fn seal_selected_accounts_with_password(
    root: &Path,
    selected: &[AccountId],
    password: &[u8],
    setup_binding: &[u8],
) -> Result<PasswordSealedAccounts, PortableError> {
    validate_password(password)?;
    let context = wrap_binding(setup_binding)?;
    let key = SymmetricKey::generate().map_err(|_| PortableError::CredentialUnavailable)?;
    let selected = seal_selected_accounts(root, selected, &key, setup_binding)?;
    let wrapped = envelope::seal_with_password(
        password,
        PasswordParams::DEFAULT,
        Context::new(Label::MigrationPackage, &context),
        key.expose_secret(),
    )
    .map_err(|_| PortableError::CredentialUnavailable)?;
    let capacity = 16_usize
        .checked_add(wrapped.len())
        .and_then(|size| size.checked_add(selected.0.len()))
        .ok_or(PortableError::InvalidConfiguration)?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(ARTIFACT_MAGIC);
    bytes.extend_from_slice(
        &u32::try_from(wrapped.len())
            .map_err(|_| PortableError::InvalidConfiguration)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&wrapped);
    bytes.extend_from_slice(
        &u32::try_from(selected.0.len())
            .map_err(|_| PortableError::InvalidConfiguration)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(selected.as_bytes());
    PasswordSealedAccounts::from_bytes(&bytes)
}
/// Explicit selected local accounts only. Read credentials from canonical
/// custody, then seal them under a fresh caller-owned transport key. Caller
/// must wrap that key in authenticated setup custody and keep its
/// setup binding; neither source vault files nor source keys are portable.
/// # Errors
/// Refuses invalid bindings or selections, missing accounts or credentials,
/// inaccessible storage and encryption failures.
pub fn seal_selected_accounts(
    root: &Path,
    selected: &[AccountId],
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<SealedAccounts, PortableError> {
    let context = binding(setup_binding)?;
    if selected.is_empty() || selected.len() > MAX_ACCOUNTS {
        return Err(PortableError::InvalidConfiguration);
    }
    let store = load_store(root)?;
    let vault = SecretVault::open_read_only_in(root);
    if vault.recovery_error().is_some() {
        return Err(PortableError::CredentialUnavailable);
    }
    let mut entries = Vec::with_capacity(selected.len());
    for id in selected {
        let account = store
            .get(id)
            .filter(|account| account.owner.is_none())
            .cloned()
            .ok_or(PortableError::AccountMissing)?;
        let secret = vault.get(id).ok_or(PortableError::CredentialUnavailable)?;
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
    .map_err(|_| PortableError::InvalidConfiguration)?;
    if plain.len() > MAX_PLAINTEXT {
        return Err(PortableError::InvalidConfiguration);
    }
    let sealed = envelope::seal(key, Context::new(Label::MigrationPackage, &context), &plain)
        .map_err(|_| PortableError::CredentialUnavailable)?;
    Ok(SealedAccounts(sealed))
}

/// Authenticate the full artifact and exact selection before touching destination
/// custody. Intended for explicit private custody roots, including staging roots;
/// installed-user consumption must use [`adopt_installed_accounts`].
/// Exact retries succeed; conflicting accounts are never overwritten.
/// # Errors
/// Refuses invalid selections, authentication failures and storage conflicts.
pub fn import_password_sealed_accounts_matching(
    root: &Path,
    artifact: &PasswordSealedAccounts,
    password: &[u8],
    setup_binding: &[u8],
    expected: &[AccountId],
) -> Result<Vec<AccountId>, PortableError> {
    import_bundle(
        root,
        &matching_bundle(artifact, password, setup_binding, expected)?,
    )
}

/// Import a symmetric envelope into an explicit private custody root.
/// # Errors
/// Refuses authentication failures, invalid bundles and storage conflicts.
pub fn import_sealed_accounts(
    root: &Path,
    sealed: &SealedAccounts,
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<Vec<AccountId>, PortableError> {
    import_bundle(root, &open_bundle(sealed, key, setup_binding)?)
}

/// Adopt into the current installed user's canonical configuration root only.
/// Neither root nor a privileged process impersonating the target may consume
/// credentials. Run this after entering the destination user's login context.
/// The caller must authenticate the supplied binding and expected IDs.
/// # Errors
/// Refuses root, a different real/effective uid, or invalid custody/artifacts.
#[cfg(unix)]
pub fn adopt_installed_accounts(
    artifact: &PasswordSealedAccounts,
    password: &[u8],
    target: &PortableBinding,
    expected: &[AccountId],
) -> Result<Vec<AccountId>, PortableError> {
    let real = rustix::process::getuid().as_raw();
    let effective = rustix::process::geteuid().as_raw();
    if real == 0 || real != target.uid || effective != target.uid {
        return Err(PortableError::WrongUser);
    }
    let root = accounts::config_root().ok_or(PortableError::StorageUnavailable)?;
    import_password_sealed_accounts_matching(&root, artifact, password, target.as_bytes(), expected)
}

fn load_store(root: &Path) -> Result<AccountStore, PortableError> {
    match std::fs::read(accounts::store_path_in(root)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| PortableError::StorageUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AccountStore::new()),
        Err(_) => Err(PortableError::StorageUnavailable),
    }
}

fn import_bundle(root: &Path, bundle: &Bundle) -> Result<Vec<AccountId>, PortableError> {
    let mut store = load_store(root)?;
    // A read-only preflight cannot create target keys on rejection. Any existing
    // vault directory must be readable, even when metadata is still empty.
    let vault_path = accounts::vault_dir_in(root);
    let present = match std::fs::symlink_metadata(&vault_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(PortableError::StorageUnavailable),
    };
    let existing_vault = present.then(|| SecretVault::open_read_only_in(root));
    if existing_vault
        .as_ref()
        .is_some_and(|v| v.recovery_error().is_some())
    {
        return Err(PortableError::StorageUnavailable);
    }
    for entry in &bundle.entries {
        let existing = store.get(&entry.account.id);
        if existing.is_some_and(|a| a != &entry.account) {
            return Err(PortableError::AlreadyPresent);
        }
        let stored = existing_vault
            .as_ref()
            .and_then(|v| v.get(&entry.account.id))
            .map(PrivateSecret);
        if stored.as_ref().is_some_and(|s| s.0 != entry.secret) {
            return Err(PortableError::AlreadyPresent);
        }
        if existing.is_some() && stored.is_none() {
            return Err(PortableError::CredentialUnavailable);
        }
    }
    let mut vault = SecretVault::open_in(root);
    if vault.recovery_error().is_some() {
        return Err(PortableError::StorageUnavailable);
    }
    let mut ids = Vec::with_capacity(bundle.entries.len());
    for entry in &bundle.entries {
        vault.put(&entry.account.id, &entry.secret);
        store.upsert(entry.account.clone());
        ids.push(entry.account.id.clone());
    }
    // Publish grants first: interruption can leave a sealed orphan, never
    // connected metadata whose credential was not durably stored.
    vault
        .try_save()
        .map_err(|_| PortableError::StorageUnavailable)?;
    store
        .save_in(root)
        .map_err(|_| PortableError::StorageUnavailable)?;
    Ok(ids)
}

struct PrivateSecret(Secret);
impl Drop for PrivateSecret {
    fn drop(&mut self) {
        self.0.password.zeroize();
        if let Some(token) = &mut self.0.token {
            token.zeroize();
        }
    }
}

fn open_bundle(
    sealed: &SealedAccounts,
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<Bundle, PortableError> {
    let context = binding(setup_binding)?;
    if sealed.0.len() > MAX_SEALED {
        return Err(PortableError::InvalidConfiguration);
    }
    let plain = envelope::open(
        key,
        Context::new(Label::MigrationPackage, &context),
        &sealed.0,
    )
    .map_err(|_| PortableError::CredentialUnavailable)?;
    if plain.len() > MAX_PLAINTEXT {
        return Err(PortableError::InvalidConfiguration);
    }
    let bundle: Bundle =
        serde_json::from_slice(&plain).map_err(|_| PortableError::InvalidConfiguration)?;
    if bundle.version != 1 {
        return Err(PortableError::InvalidConfiguration);
    }
    valid_entries(&bundle.entries)?;
    Ok(bundle)
}

fn matching_bundle(
    artifact: &PasswordSealedAccounts,
    password: &[u8],
    setup_binding: &[u8],
    expected: &[AccountId],
) -> Result<Bundle, PortableError> {
    if expected.is_empty() || expected.len() > MAX_ACCOUNTS {
        return Err(PortableError::InvalidConfiguration);
    }
    let requested: BTreeSet<_> = expected.iter().map(AccountId::as_str).collect();
    if requested.len() != expected.len() {
        return Err(PortableError::InvalidConfiguration);
    }
    let (selected, key) = artifact.open(password, setup_binding)?;
    let bundle = open_bundle(&selected, &key, setup_binding)?;
    let present: BTreeSet<_> = bundle
        .entries
        .iter()
        .map(|entry| entry.account.id.as_str())
        .collect();
    if requested != present {
        return Err(PortableError::InvalidConfiguration);
    }
    Ok(bundle)
}

impl PasswordSealedAccounts {
    /// Inspect the exact authenticated selection's consumer requirement.
    /// # Errors
    /// Refuses incorrect passwords, binding, selection or invalid sealed custody.
    pub fn requires_protected_consumer(
        &self,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<bool, PortableError> {
        let bundle = matching_bundle(self, password, setup_binding, expected)?;
        Ok(bundle
            .entries
            .iter()
            .any(|entry| crate::requires_protected_consumer(&entry.secret)))
    }
    /// Authenticate and check exact manifest selection without creating any
    /// target metadata or vault. Used before accepting prepared setup custody.
    /// # Errors
    /// Refuses incorrect passwords, bindings, selections or invalid sealed custody.
    pub fn verify_selection(
        &self,
        password: &[u8],
        setup_binding: &[u8],
        expected: &[AccountId],
    ) -> Result<(), PortableError> {
        matching_bundle(self, password, setup_binding, expected).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PASSWORD: &[u8] = b"a long transport password";

    fn source(root: &Path) -> AccountId {
        let account = Account::new(
            AccountId::new("chosen"),
            accounts::AccountTypeId::new("webdav"),
        );
        let id = account.id.clone();
        let mut vault = SecretVault::open_in(root);
        vault.put(&id, &Secret::password("private account password"));
        vault.try_save().unwrap();
        let mut store = AccountStore::new();
        store.upsert(account);
        store.upsert(Account::new(
            AccountId::new("not-chosen"),
            accounts::AccountTypeId::new("webdav"),
        ));
        store.save_in(root).unwrap();
        id
    }

    #[test]
    fn canonical_roundtrip_selects_and_reseals_without_changing_source() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        let id = source(&from);
        let source_key = std::fs::read(accounts::vault_dir_in(&from).join("vault.key")).unwrap();
        let source_cipher = std::fs::read(accounts::vault_dir_in(&from).join("vault.enc")).unwrap();
        let binding = PortableBinding::new("operation", "disk", "person", 1000).unwrap();
        let selected = std::slice::from_ref(&id);
        let artifact =
            seal_selected_accounts_with_password(&from, selected, PASSWORD, binding.as_bytes())
                .unwrap();
        let artifact = PasswordSealedAccounts::from_bytes(artifact.as_bytes()).unwrap();
        assert_eq!(
            import_password_sealed_accounts_matching(
                &to,
                &artifact,
                PASSWORD,
                binding.as_bytes(),
                selected
            )
            .unwrap(),
            selected
        );
        assert_eq!(
            import_password_sealed_accounts_matching(
                &to,
                &artifact,
                PASSWORD,
                binding.as_bytes(),
                selected
            )
            .unwrap(),
            selected
        );
        assert_eq!(AccountStore::load_in(&to).len(), 1);
        assert_eq!(
            SecretVault::open_read_only_in(&to)
                .get(&id)
                .unwrap()
                .password,
            "private account password"
        );
        assert_ne!(
            std::fs::read(accounts::vault_dir_in(&to).join("vault.key")).unwrap(),
            source_key
        );
        assert_eq!(
            std::fs::read(accounts::vault_dir_in(&from).join("vault.key")).unwrap(),
            source_key
        );
        assert_eq!(
            std::fs::read(accounts::vault_dir_in(&from).join("vault.enc")).unwrap(),
            source_cipher
        );
        assert!(
            seal_selected_accounts_with_password(
                &from,
                &[id.clone(), id],
                PASSWORD,
                binding.as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn operation_target_user_and_uid_are_authenticated_before_writes() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        let id = source(&from);
        let key = SymmetricKey::generate().unwrap();
        let intended = PortableBinding::new("op", "disk", "alice", 1000).unwrap();
        let artifact = seal_selected_accounts(&from, &[id], &key, intended.as_bytes()).unwrap();
        for binding in [
            PortableBinding::new("other", "disk", "alice", 1000).unwrap(),
            PortableBinding::new("op", "other", "alice", 1000).unwrap(),
            PortableBinding::new("op", "disk", "bob", 1000).unwrap(),
            PortableBinding::new("op", "disk", "alice", 1001).unwrap(),
        ] {
            assert_eq!(
                import_sealed_accounts(&to, &artifact, &key, binding.as_bytes()),
                Err(PortableError::CredentialUnavailable)
            );
            assert!(!to.exists());
        }
        assert!(PortableBinding::new("op", "disk", "root", 0).is_err());
    }

    #[test]
    fn legacy_version_one_wire_and_domains_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        let id = source(&from);
        let selected = std::slice::from_ref(&id);
        let artifact =
            seal_selected_accounts_with_password(&from, selected, PASSWORD, b"legacy-setup")
                .unwrap();
        // Decode independently using the original framing and literal domains.
        let wire = artifact.as_bytes();
        assert_eq!(&wire[..8], b"TILOACC1");
        let wrapped_len = u32::from_le_bytes(wire[8..12].try_into().unwrap()) as usize;
        let key = envelope::open_with_password(
            PASSWORD,
            Context::new(
                Label::MigrationPackage,
                b"tilo-prepared-account-key/v1\0legacy-setup",
            ),
            &wire[12..12 + wrapped_len],
        )
        .unwrap();
        let key = SymmetricKey::from_bytes(key.as_slice().try_into().unwrap());
        let start = 16 + wrapped_len;
        assert_eq!(
            u32::from_le_bytes(wire[start - 4..start].try_into().unwrap()) as usize,
            wire.len() - start
        );
        let plain = envelope::open(
            &key,
            Context::new(
                Label::MigrationPackage,
                b"tilo-prepared-accounts/v1\0legacy-setup",
            ),
            &wire[start..],
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["entries"][0]["account"]["id"], "chosen");
        assert_eq!(
            json["entries"][0]["secret"]["password"],
            "private account password"
        );
        assert_eq!(json.as_object().unwrap().len(), 2);
        assert!(
            import_password_sealed_accounts_matching(
                &to,
                &artifact,
                PASSWORD,
                b"legacy-setup",
                &[AccountId::new("other")]
            )
            .is_err()
        );
        assert!(!to.exists());
        import_password_sealed_accounts_matching(
            &to,
            &artifact,
            PASSWORD,
            b"legacy-setup",
            selected,
        )
        .unwrap();
        let mut trailing = wire.to_vec();
        trailing.push(0);
        assert!(PasswordSealedAccounts::from_bytes(&trailing).is_err());
    }

    // Re-enter this exact test in a fresh process: changing environment or uid
    // in the parallel test runner would affect unrelated custody tests.
    #[test]
    #[cfg(unix)]
    fn installed_adoption_succeeds_as_private_target_user() {
        use std::os::unix::{fs::PermissionsExt, process::CommandExt};

        const PAYLOAD: &str = "TILO_ACCOUNT_ADOPTION_TEST_PAYLOAD";
        if let Some(payload) = std::env::var_os(PAYLOAD) {
            check_installed_adoption(Path::new(&payload));
        } else {
            let temp = tempfile::tempdir().unwrap();
            let from = temp.path().join("from");
            let id = source(&from);
            let real = rustix::process::getuid().as_raw();
            if real == 0 {
                // Root in a capability-less job sandbox cannot give the
                // fixture to the target user or become it. Require the
                // capability where it exists (the micro-VM check sets
                // TILO_REQUIRE_CHOWN=1) and skip with that reason elsewhere.
                tilo_test_env::require_capability!(
                    Chown,
                    tilo_test_env::switch_identity(65534),
                    "the adoption fixture gives its files to the target user and becomes it"
                );
            }
            let uid = if real == 0 { 65534 } else { real };
            let gid = if real == 0 {
                65534
            } else {
                rustix::process::getgid().as_raw()
            };
            let target = PortableBinding::new("operation", "disk", "person", uid).unwrap();
            let artifact = seal_selected_accounts_with_password(
                &from,
                std::slice::from_ref(&id),
                PASSWORD,
                target.as_bytes(),
            )
            .unwrap();
            let home = temp.path().join("home");
            let config = home.join("config");
            std::fs::create_dir_all(&config).unwrap();
            let artifact_path = temp.path().join("artifact");
            let binding_path = temp.path().join("binding.json");
            let expected_path = temp.path().join("expected.json");
            std::fs::write(&artifact_path, artifact.as_bytes()).unwrap();
            std::fs::write(&binding_path, target.as_bytes()).unwrap();
            std::fs::write(&expected_path, serde_json::to_vec(&vec![id]).unwrap()).unwrap();
            // The private parent must also be traversable by the dropped uid.
            // Only this test's directories and transport inputs change owner;
            // the source vault remains in the producing process's custody.
            for path in [
                temp.path(),
                &home,
                &config,
                &artifact_path,
                &binding_path,
                &expected_path,
            ] {
                if real == 0 {
                    std::os::unix::fs::chown(path, Some(uid), Some(gid)).unwrap();
                }
                let mode = if path.is_dir() { 0o700 } else { 0o600 };
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            // Bound the child so a wedged fixture fails the lane instead of
            // stalling it; timeout exits 124 and the status check below fails.
            let mut child = std::process::Command::new("timeout");
            child
                .args(["--kill-after=5", "60"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "portable::tests::installed_adoption_succeeds_as_private_target_user",
                    "--nocapture",
                ])
                .env(PAYLOAD, temp.path())
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &config);
            if real == 0 {
                // The runner's TMPDIR ancestors may be root-private. Pass our
                // directory through stdin so the child can traverse its own
                // private inputs without granting access to those ancestors.
                let child_root = Path::new("/dev/fd/0");
                child
                    .stdin(std::fs::File::open(temp.path()).unwrap())
                    .env(PAYLOAD, child_root)
                    .env("HOME", child_root.join("home"))
                    .env("XDG_CONFIG_HOME", child_root.join("home/config"));
                // CommandExt changes both real and effective identities before
                // exec, rather than impersonating the user with only an euid.
                child.gid(gid).uid(uid);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "installed adoption helper failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // An incorrectly filtered helper would otherwise exit successfully
            // after executing zero tests.
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    #[cfg(unix)]
    fn check_installed_adoption(payload: &Path) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let (domain, operation, disk, user, uid): (String, String, String, String, u32) =
            serde_json::from_slice(&std::fs::read(payload.join("binding.json")).unwrap()).unwrap();
        assert_eq!(domain, "tilo-account-target/v1");
        assert_ne!(uid, 0);
        assert_eq!(rustix::process::getuid().as_raw(), uid);
        assert_eq!(rustix::process::geteuid().as_raw(), uid);
        let target = PortableBinding::new(&operation, &disk, &user, uid).unwrap();
        let expected: Vec<AccountId> =
            serde_json::from_slice(&std::fs::read(payload.join("expected.json")).unwrap()).unwrap();
        let artifact =
            PasswordSealedAccounts::from_bytes(&std::fs::read(payload.join("artifact")).unwrap())
                .unwrap();
        let home = payload.join("home");
        let config = home.join("config");
        for path in [payload, &home, &config] {
            let metadata = std::fs::metadata(path).unwrap();
            assert_eq!(metadata.uid(), uid);
            assert_eq!(metadata.gid(), rustix::process::getgid().as_raw());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        let root = config.join("tilo");
        assert_eq!(accounts::config_root().unwrap(), root);
        for wrong in [
            PortableBinding::new("wrong-operation", &disk, &user, uid).unwrap(),
            PortableBinding::new(&operation, "wrong-disk", &user, uid).unwrap(),
            PortableBinding::new(&operation, &disk, "wrong-user", uid).unwrap(),
        ] {
            assert_eq!(
                adopt_installed_accounts(&artifact, PASSWORD, &wrong, &expected),
                Err(PortableError::CredentialUnavailable)
            );
            assert_eq!(std::fs::read_dir(&config).unwrap().count(), 0);
        }
        assert_eq!(
            adopt_installed_accounts(&artifact, PASSWORD, &target, &[AccountId::new("other")]),
            Err(PortableError::InvalidConfiguration)
        );
        assert!(!root.exists());
        assert_eq!(
            adopt_installed_accounts(&artifact, PASSWORD, &target, &expected).unwrap(),
            expected
        );
        let key_path = accounts::vault_dir_in(&root).join("vault.key");
        let key = std::fs::read(&key_path).unwrap();
        let metadata = std::fs::read(accounts::store_path_in(&root)).unwrap();
        assert_eq!(
            adopt_installed_accounts(&artifact, PASSWORD, &target, &expected).unwrap(),
            expected
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), key);
        assert_eq!(
            std::fs::read(accounts::store_path_in(&root)).unwrap(),
            metadata
        );
        let store = AccountStore::load_in(&root);
        assert_eq!(store.len(), 1);
        assert!(store.get(&expected[0]).unwrap().owner.is_none());
        let vault = SecretVault::open_read_only_in(&root);
        assert!(vault.recovery_error().is_none());
        assert_eq!(
            vault.get(&expected[0]).unwrap(),
            Secret::password("private account password")
        );
        assert!(vault.get(&AccountId::new("not-chosen")).is_none());
        assert_eq!(
            std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for path in [key_path, accounts::store_path_in(&root)] {
            assert_eq!(std::fs::metadata(path).unwrap().uid(), uid);
        }
        assert!(!home.join(".config").exists());
    }

    #[test]
    #[cfg(unix)]
    fn installed_adoption_refuses_foreign_uid_before_authentication() {
        let foreign = rustix::process::getuid().as_raw().saturating_add(1).max(1);
        let target = PortableBinding::new("operation", "target", "person", foreign).unwrap();
        // Deliberately invalid artifact: identity refusal must precede decoding
        // and cannot create either metadata or a vault.
        assert_eq!(
            adopt_installed_accounts(&PasswordSealedAccounts(Vec::new()), PASSWORD, &target, &[]),
            Err(PortableError::WrongUser)
        );
    }
}
