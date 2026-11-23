//! Bounded, encrypted transport of explicitly selected local accounts.
//!
//! The version-one envelope and domains are compatible with prepared account
//! artifacts. Filesystem publication belongs to the caller; this codec handles
//! only memory and canonical ownership roots.
use std::collections::BTreeSet;

use std::path::Path;

use accounts::person::AccountList;
use accounts::{Account, AccountId, Secret, SecretVault};
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Frame, FrameError, Label, SymmetricKey, envelope, migration_password};
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

/// Ciphertext only. No key, source ownership, plaintext credential or public
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

const ARTIFACT: Frame = Frame::new(
    b"TILOACC1",
    b"tilo-prepared-account-key/v1\0",
    256,
    MAX_SEALED,
);
pub const MAX_ARTIFACT: usize = ARTIFACT.max_len();

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
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PortableError> {
        ARTIFACT.parse(bytes).map_err(frame_error)?;
        Ok(Self(bytes.to_vec()))
    }
    fn open(
        &self,
        password: &[u8],
        setup: &[u8],
    ) -> Result<(SealedAccounts, SymmetricKey), PortableError> {
        validate_password(password)?;
        let (transport, selected) = ARTIFACT
            .open(password, setup, &self.0)
            .map_err(frame_error)?;
        Ok((SealedAccounts::from_bytes(&selected)?, transport))
    }
}
fn frame_error(error: FrameError) -> PortableError {
    match error {
        FrameError::Malformed => PortableError::InvalidConfiguration,
        FrameError::Unauthenticated => PortableError::CredentialUnavailable,
    }
}
fn validate_password(password: &[u8]) -> Result<(), PortableError> {
    if !migration_password(password) {
        return Err(PortableError::InvalidConfiguration);
    }
    Ok(())
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
        if entry.account.id.as_str().is_empty()
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
pub fn seal_selected_accounts_with_password(
    root: &Path,
    selected: &[AccountId],
    password: &[u8],
    setup_binding: &[u8],
) -> Result<PasswordSealedAccounts, PortableError> {
    validate_password(password)?;
    binding(setup_binding)?;
    let key = SymmetricKey::generate().map_err(|_| PortableError::CredentialUnavailable)?;
    let content = seal_selected_accounts(root, selected, &key, setup_binding)?;
    let bytes = ARTIFACT
        .seal(password, setup_binding, &key, content.as_bytes())
        .map_err(frame_error)?;
    PasswordSealedAccounts::from_bytes(&bytes)
}
/// Explicit selected local accounts only. Read credentials from canonical
/// ownership, then seal them under a fresh caller-owned transport key. Caller
/// must wrap that key in authenticated setup ownership and keep its
/// setup binding; neither source vault files nor source keys are portable.
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
    let record = load_record(root)?;
    let vault = SecretVault::open_person(Some(root));
    if vault.recovery_error().is_some() {
        return Err(PortableError::CredentialUnavailable);
    }
    let mut entries = Vec::with_capacity(selected.len());
    for id in selected {
        let account = record
            .account(id.as_str())
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

pub fn import_password_sealed_accounts(
    root: &Path,
    artifact: &PasswordSealedAccounts,
    password: &[u8],
    setup_binding: &[u8],
) -> Result<Vec<AccountId>, PortableError> {
    let (selected, key) = artifact.open(password, setup_binding)?;
    import_sealed_accounts(root, &selected, &key, setup_binding)
}

/// Authenticate the full artifact and exact selection before touching destination
/// ownership. Intended for explicit private ownership roots, including staging roots.
/// Exact retries succeed; conflicting accounts are never overwritten.
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

/// Import a symmetric envelope into an explicit private ownership root.
pub fn import_sealed_accounts(
    root: &Path,
    sealed: &SealedAccounts,
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<Vec<AccountId>, PortableError> {
    import_bundle(root, &open_bundle(sealed, key, setup_binding)?)
}

fn load_record(root: &Path) -> Result<AccountList, PortableError> {
    let staged =
        accounts::person::staged_in(root).map_err(|_| PortableError::StorageUnavailable)?;
    staged
        .state()
        .map_err(|_| PortableError::StorageUnavailable)
}

fn import_bundle(root: &Path, bundle: &Bundle) -> Result<Vec<AccountId>, PortableError> {
    let mut record =
        accounts::person::staged_in(root).map_err(|_| PortableError::StorageUnavailable)?;
    let existing = record
        .state()
        .map_err(|_| PortableError::StorageUnavailable)?;
    // Reading a vault creates no key where none is stored, so a rejection
    // leaves the target untouched. Any existing vault directory must be
    // readable, even when metadata is still empty.
    let vault_path = accounts::vault_dir_in(root);
    let present = match std::fs::symlink_metadata(&vault_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(PortableError::StorageUnavailable),
    };
    let existing_vault = present.then(|| SecretVault::open_person(Some(root)));
    if existing_vault
        .as_ref()
        .is_some_and(|v| v.recovery_error().is_some())
    {
        return Err(PortableError::StorageUnavailable);
    }
    for entry in &bundle.entries {
        let kept = existing.account(entry.account.id.as_str());
        if kept.as_ref().is_some_and(|a| a != &entry.account) {
            return Err(PortableError::AlreadyPresent);
        }
        let stored = existing_vault
            .as_ref()
            .and_then(|v| v.get(&entry.account.id))
            .map(PrivateSecret);
        if stored.as_ref().is_some_and(|s| s.0 != entry.secret) {
            return Err(PortableError::AlreadyPresent);
        }
        if kept.is_some() && stored.is_none() {
            return Err(PortableError::CredentialUnavailable);
        }
    }
    let mut vault = SecretVault::open_person(Some(root));
    if vault.recovery_error().is_some() {
        return Err(PortableError::StorageUnavailable);
    }
    let mut ids = Vec::with_capacity(bundle.entries.len());
    for entry in &bundle.entries {
        vault.put(&entry.account.id, &entry.secret);
        record
            .apply_confirmed(AccountList::saving(&entry.account))
            .map_err(|_| PortableError::StorageUnavailable)?;
        ids.push(entry.account.id.clone());
    }
    // Publish grants first: interruption can leave a sealed orphan, never
    // connected metadata whose credential was not durably stored.
    vault
        .try_save()
        .map_err(|_| PortableError::StorageUnavailable)?;
    accounts::person::keep_staged_in(root, &mut record)
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
        let mut record = accounts::person::staged_in(root).unwrap();
        record
            .apply_confirmed(AccountList::saving(&account))
            .unwrap();
        record
            .apply_confirmed(AccountList::saving(&Account::new(
                AccountId::new("not-chosen"),
                accounts::AccountTypeId::new("webdav"),
            )))
            .unwrap();
        accounts::person::keep_staged_in(root, &mut record).unwrap();
        id
    }

    tilo_test_env::scenario! {
        /// The password-sealed bundle of accounts an install carries over.
        fn portable_account_bundles_are_authenticated_before_anything_is_written;

        phase canonical_roundtrip_selects_and_reseals_without_changing_source;
        {
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
            assert_eq!(
                accounts::person::staged_in(&to)
                    .unwrap()
                    .state()
                    .unwrap()
                    .len(),
                1
            );
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

        phase operation_target_user_and_uid_are_authenticated_before_writes;
        {
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

        phase legacy_version_one_wire_and_domains_are_preserved;
        {
            let temp = tempfile::tempdir().unwrap();
            let from = temp.path().join("from");
            let to = temp.path().join("to");
            let id = source(&from);
            let selected = std::slice::from_ref(&id);
            let artifact =
                seal_selected_accounts_with_password(&from, selected, PASSWORD, b"legacy-setup")
                    .unwrap();
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

        phase untrusted_expensive_kdf_and_framing_are_refused_before_decryption;
        {
            let temp = tempfile::tempdir().unwrap();
            let from = temp.path().join("from");
            let id = source(&from);
            let artifact =
                seal_selected_accounts_with_password(&from, &[id], PASSWORD, b"setup").unwrap();
            let mut expensive = artifact.as_bytes().to_vec();
            expensive[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(PasswordSealedAccounts::from_bytes(&expensive).is_err());
            let short = &artifact.as_bytes()[..artifact.as_bytes().len() - 1];
            assert!(PasswordSealedAccounts::from_bytes(short).is_err());
        }

        phase invalid_bundle_is_refused_before_target_creation;
        {
            let temp = tempfile::tempdir().unwrap();
            let to = temp.path().join("never-created");
            let key = SymmetricKey::generate().unwrap();
            let account = Account::new(AccountId::new(""), accounts::AccountTypeId::new("webdav"));
            let bundle = Bundle {
                version: 1,
                entries: vec![Entry {
                    account,
                    secret: Secret::password("private-member-password"),
                }],
            };
            let plain = Zeroizing::new(serde_json::to_vec(&bundle).unwrap());
            let context = binding(b"setup-1").unwrap();
            let sealed = SealedAccounts(
                envelope::seal(
                    &key,
                    Context::new(Label::MigrationPackage, &context),
                    &plain,
                )
                .unwrap(),
            );
            assert_eq!(
                import_sealed_accounts(&to, &sealed, &key, b"setup-1"),
                Err(PortableError::InvalidConfiguration)
            );
            assert!(!to.exists());
        }
    }
}
