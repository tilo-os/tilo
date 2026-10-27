//! Selected Thunderbird mail and personal data as a sealed transport bundle,
//! and its import into the destination user's canonical Mail account store
//! and Mail cache. Selection is part of the authenticated artifact: a caller
//! cannot widen an export after review.
//!
//! A transfer that stays on this computer does not build that bundle: the
//! source Home is already granted, so [`stream::import_reviewed`] reads the
//! reviewed folders chunk by chunk and publishes message by message. The
//! sealed artifact is for a transfer that has to pass through installation
//! media, where it is bounded as a whole.
//!
//! The canonical Mail cache is `$XDG_CACHE_HOME/so.tilo.mail/mail/<account>.json`
//! plus `<account>.files/<message>.<index>` for attachment bytes, matching
//! `apps/mail/src/store.rs`. Thunderbird folders with no Mail system-folder
//! equivalent are placed in Archive with a `Thunderbird/<path>` label, while
//! their exact original mbox/maildir bytes are retained below
//! `.local/share/tilo/mail-import/thunderbird/` so nothing is flattened away.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use accounts::email::{AccountConfig, AuthMethod};
use accounts::thunderbird::{AccountMetadata, Transport};
use accounts::{Account, AccountId, AccountStore};
use mail_parser::MimeHeaders;
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, PasswordParams, SymmetricKey, envelope};
use zeroize::Zeroizing;

use crate::account_sources::thunderbird::{
    FolderKind, ReviewedFolder, ReviewedThunderbird, ThunderbirdAccount, ThunderbirdFolder,
    read_maildir_files, read_reviewed,
};

use super::ConnectionError;
use super::actions::target_root;

mod personal_data;
mod stream;

pub use stream::import_reviewed;

const DOMAIN: &[u8] = b"tilo-thunderbird-selection/v1\0";
const MAX_BINDING: usize = 4096;
const MAX_PLAINTEXT: usize = 256 * 1024 * 1024;
const MAX_SEALED: usize = MAX_PLAINTEXT + 4096;
const MAX_FOLDERS: usize = 4096;
const MAX_ACCOUNTS: usize = 128;
const MAX_FILES: usize = 8192;
const CACHE_VERSION: u32 = 1;
/// Upper bound of one account's kept-mail file. The Mail app holds this file
/// in memory when it opens an account, so a larger corpus is refused rather
/// than read whole or overwritten.
const MAX_CACHE: u64 = 256 * 1024 * 1024;
/// How often a merge is retried after the kept file changed under it.
const CACHE_ATTEMPTS: usize = 3;
const MAIL_APP: &str = "so.tilo.mail";
const ARTIFACT_MAGIC: &[u8; 8] = b"TILOTB01";
const WRAP_DOMAIN: &[u8] = b"tilo-thunderbird-password/v1\0";
const MAX_WRAPPED: usize = 16 * 1024;
/// Upper bound of one password-wrapped selected-mail artifact.
pub const MAX_THUNDERBIRD_ARTIFACT: usize = MAX_SEALED + MAX_WRAPPED + 16;

/// The exact reviewed children to export. An empty account list is an empty
/// selection, not "everything". IDs are the opaque IDs from discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdSelection {
    pub accounts: Vec<ThunderbirdAccountSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdAccountSelection {
    pub source_id: String,
    pub folders: Vec<String>,
    pub address_books: Vec<String>,
    pub calendars: Vec<String>,
}

impl ThunderbirdSelection {
    /// Select every discovered child. The UI still shows each child so the
    /// person can remove one before approving the transfer.
    pub fn all(reviewed: &ReviewedThunderbird) -> Self {
        Self {
            accounts: reviewed
                .accounts
                .iter()
                .map(|account| ThunderbirdAccountSelection {
                    source_id: account.inventory.source_id.clone(),
                    folders: account
                        .folders
                        .iter()
                        .map(|folder| folder.inventory.source_id.clone())
                        .collect(),
                    address_books: reviewed
                        .address_books
                        .iter()
                        .map(|book| book.source_id.clone())
                        .collect(),
                    calendars: reviewed
                        .calendars
                        .iter()
                        .map(|calendar| calendar.source_id.clone())
                        .collect(),
                })
                .collect(),
        }
    }

    /// Whether this selection names exactly the reviewed account and no
    /// child outside it.
    pub fn validate(&self, reviewed: &ReviewedThunderbird) -> Result<(), ConnectionError> {
        if self.accounts.is_empty() || self.accounts.len() > MAX_ACCOUNTS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        for selected in &self.accounts {
            if !seen.insert(selected.source_id.as_str()) {
                return Err(ConnectionError::InvalidConfiguration);
            }
            let account = reviewed
                .accounts
                .iter()
                .find(|account| account.inventory.source_id == selected.source_id)
                .ok_or(ConnectionError::AccountMissing)?;
            unique(&selected.folders, MAX_FOLDERS)?;
            unique(&selected.address_books, MAX_FOLDERS)?;
            unique(&selected.calendars, MAX_FOLDERS)?;
            for folder in &selected.folders {
                if !account
                    .folders
                    .iter()
                    .any(|item| &item.inventory.source_id == folder)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
            for book in &selected.address_books {
                if !reviewed
                    .address_books
                    .iter()
                    .any(|item| &item.source_id == book)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
            for calendar in &selected.calendars {
                if !reviewed
                    .calendars
                    .iter()
                    .any(|item| &item.source_id == calendar)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
        }
        Ok(())
    }

    /// Structural checks that need no live source. The manifest uses this
    /// before the exact reviewed selection is available again.
    ///
    /// # Errors
    /// Refuses an empty, oversized or duplicate selection shape.
    pub fn validate_shape(&self) -> Result<(), ConnectionError> {
        if self.accounts.is_empty() || self.accounts.len() > MAX_ACCOUNTS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        for selected in &self.accounts {
            if selected.source_id.is_empty()
                || selected.source_id.len() > 256
                || !seen.insert(selected.source_id.as_str())
            {
                return Err(ConnectionError::InvalidConfiguration);
            }
            unique(&selected.folders, MAX_FOLDERS)?;
            unique(&selected.address_books, MAX_FOLDERS)?;
            unique(&selected.calendars, MAX_FOLDERS)?;
        }
        Ok(())
    }
}

fn unique(values: &[String], limit: usize) -> Result<(), ConnectionError> {
    if values.len() > limit
        || values
            .iter()
            .any(|value| value.is_empty() || value.len() > 256)
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let set: BTreeSet<_> = values.iter().collect();
    (set.len() == values.len())
        .then_some(())
        .ok_or(ConnectionError::InvalidConfiguration)
}

/// Ciphertext only. No key, source path, email address or message body is
/// visible to the caller until it authenticates the artifact.
pub struct SealedThunderbird(Vec<u8>);

impl std::fmt::Debug for SealedThunderbird {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedThunderbird")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl SealedThunderbird {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Parse only the envelope framing. Authentication happens on import.
    ///
    /// # Errors
    /// Refuses an oversized or malformed envelope.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectionError> {
        if bytes.len() > MAX_SEALED {
            return Err(ConnectionError::InvalidConfiguration);
        }
        envelope::Envelope::parse(bytes).map_err(|_| ConnectionError::CredentialUnavailable)?;
        Ok(Self(bytes.to_vec()))
    }
}

/// A selected-mail artifact whose content key is wrapped under the person's
/// preparation password. The outer package never holds a password or an
/// unwrapped key; the destination helper authenticates this exact ciphertext.
pub struct PasswordSealedThunderbird(Vec<u8>);

impl std::fmt::Debug for PasswordSealedThunderbird {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PasswordSealedThunderbird")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl PasswordSealedThunderbird {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Parse the framing and canonical password-KDF policy without deriving a
    /// key. Authentication happens only in [`Self::open`].
    ///
    /// # Errors
    /// Refuses an oversized, malformed or non-canonical artifact.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectionError> {
        parse_artifact(bytes)?;
        Ok(Self(bytes.to_vec()))
    }

    /// Read one already-granted private artifact with bounded/no-follow access.
    ///
    /// # Errors
    /// Refuses redirected, oversized or non-private inputs.
    pub fn read(path: &Path) -> Result<Self, ConnectionError> {
        read_artifact(path).map_err(|_| ConnectionError::StorageUnavailable)
    }

    /// Seal exactly the reviewed selection under a fresh content key, wrapped
    /// with the person's preparation password and the same setup binding.
    pub fn seal(
        reviewed: &ReviewedThunderbird,
        selection: &ThunderbirdSelection,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<Self, ConnectionError> {
        selection.validate(reviewed)?;
        validate_password(password)?;
        let context = wrap_binding(setup_binding)?;
        let key = SymmetricKey::generate().map_err(|_| ConnectionError::CredentialUnavailable)?;
        let selected = seal_selected(reviewed, selection, &key, setup_binding)?;
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
        bytes.extend_from_slice(&selected.0);
        Self::from_bytes(&bytes)
    }

    /// Authenticate the password wrap and the inner selection, then hand the
    /// retained key to the actual import. A wrong password, binding or changed
    /// bytes is refused before any target custody is touched.
    ///
    /// # Errors
    /// Refuses a non-canonical artifact, wrong password/binding or changed
    /// inner ciphertext.
    pub fn open(
        &self,
        password: &[u8],
        setup_binding: &[u8],
    ) -> Result<(SealedThunderbird, SymmetricKey), ConnectionError> {
        validate_password(password)?;
        let context = wrap_binding(setup_binding)?;
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
            SealedThunderbird::from_bytes(selected)?,
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
    if bytes.len() > MAX_THUNDERBIRD_ARTIFACT || bytes.get(..8) != Some(ARTIFACT_MAGIC) {
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

fn read_artifact(path: &Path) -> std::io::Result<PasswordSealedThunderbird> {
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
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_THUNDERBIRD_ARTIFACT as u64 {
        return Err(std::io::Error::other(
            "Mail artifact exceeds bounds or is not a file",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_THUNDERBIRD_ARTIFACT as u64 + 1)
        .read_to_end(&mut bytes)?;
    parent.validate()?;
    PasswordSealedThunderbird::from_bytes(&bytes)
        .map_err(|_| std::io::Error::other("Malformed mail artifact"))
}

/// What the destination actually received, in opaque source IDs and counts.
/// It never carries a source path, email address or message content.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThunderbirdImportReceipt {
    pub accounts: Vec<ThunderbirdAccountReceipt>,
    pub address_books: usize,
    pub calendars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThunderbirdAccountReceipt {
    pub source_id: String,
    pub created: bool,
    pub needs_reconnect: bool,
    pub folders: Vec<ThunderbirdFolderReceipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThunderbirdFolderReceipt {
    pub source_id: String,
    pub messages: u64,
    pub bytes: u64,
    pub local_only: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    version: u8,
    profiles: Vec<BundleProfile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleProfile {
    source_id: String,
    label: String,
    accounts: Vec<BundleAccount>,
    address_books: Vec<BundleFile>,
    calendars: Vec<BundleFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleAccount {
    inventory: ThunderbirdAccount,
    metadata: AccountMetadata,
    folders: Vec<BundleFolder>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFolder {
    inventory: ThunderbirdFolder,
    files: Vec<BundleData>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleData {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    source_id: String,
    name: String,
    bytes: Vec<u8>,
}

fn binding(binding: &[u8]) -> Result<Vec<u8>, ConnectionError> {
    if binding.is_empty() || binding.len() > MAX_BINDING {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut context = DOMAIN.to_vec();
    context.extend_from_slice(binding);
    Ok(context)
}

/// Export exactly the reviewed selection under a fresh caller-owned transport
/// key. The source files are re-read through their pinned profile handles and
/// the profile is revalidated before the artifact is produced.
///
/// # Errors
/// Refuses an invalid selection, a changed source, unavailable local files or
/// a bundle beyond the transport bound.
pub fn seal_selected(
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
    key: &SymmetricKey,
    setup_binding: &[u8],
) -> Result<SealedThunderbird, ConnectionError> {
    selection.validate(reviewed)?;
    let context = binding(setup_binding)?;
    reviewed
        .root
        .validate()
        .map_err(|_| ConnectionError::AccountMissing)?;
    let mut profiles = Vec::new();
    for profile in [reviewed] {
        let selected_accounts: BTreeMap<_, _> = selection
            .accounts
            .iter()
            .map(|account| (account.source_id.as_str(), account))
            .collect();
        let mut accounts = Vec::new();
        for account in &profile.accounts {
            let Some(selected) = selected_accounts.get(account.inventory.source_id.as_str()) else {
                continue;
            };
            let mut folders = Vec::new();
            for folder in &account.folders {
                if !selected
                    .folders
                    .iter()
                    .any(|id| id == &folder.inventory.source_id)
                {
                    continue;
                }
                folders.push(read_folder(profile, folder)?);
            }
            accounts.push(BundleAccount {
                inventory: account.inventory.clone(),
                metadata: account.metadata.clone(),
                folders,
            });
        }
        let mut address_books = Vec::new();
        for book in &profile.address_books {
            if selection
                .accounts
                .iter()
                .any(|account| account.address_books.iter().any(|id| id == &book.source_id))
            {
                address_books.push(read_bundle_file(profile, book)?);
            }
        }
        let mut calendars = Vec::new();
        for calendar in &profile.calendars {
            if selection
                .accounts
                .iter()
                .any(|account| account.calendars.iter().any(|id| id == &calendar.source_id))
            {
                calendars.push(read_bundle_file(profile, calendar)?);
            }
        }
        profiles.push(BundleProfile {
            source_id: profile.profile.source_id.clone(),
            label: profile.profile.label.clone(),
            accounts,
            address_books,
            calendars,
        });
    }
    reviewed
        .root
        .validate()
        .map_err(|_| ConnectionError::AccountMissing)?;
    let bundle = Bundle {
        version: 1,
        profiles,
    };
    let mut plain = Zeroizing::new(Vec::new());
    serde_json::to_writer(&mut Bounded(&mut plain), &bundle)
        .map_err(|_| ConnectionError::InvalidConfiguration)?;
    if plain.len() > MAX_PLAINTEXT {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let sealed = envelope::seal(key, Context::new(Label::MigrationPackage, &context), &plain)
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    Ok(SealedThunderbird(sealed))
}

fn read_folder(
    reviewed: &ReviewedThunderbird,
    folder: &ReviewedFolder,
) -> Result<BundleFolder, ConnectionError> {
    let files = match folder.kind {
        FolderKind::Mbox => vec![BundleData {
            name: folder
                .inventory
                .hierarchy
                .last()
                .cloned()
                .unwrap_or_else(|| "mail".into()),
            bytes: read_reviewed(reviewed, &folder.relative, MAX_PLAINTEXT as u64)
                .map_err(|_| ConnectionError::AccountMissing)?,
        }],
        FolderKind::Maildir => read_maildir_files(reviewed, folder, MAX_PLAINTEXT as u64)
            .map_err(|_| ConnectionError::AccountMissing)?
            .into_iter()
            .map(|(name, bytes)| BundleData { name, bytes })
            .collect(),
    };
    if files.len() > MAX_FILES {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(BundleFolder {
        inventory: folder.inventory.clone(),
        files,
    })
}

fn read_bundle_file(
    reviewed: &ReviewedThunderbird,
    file: &crate::account_sources::thunderbird::ReviewedFile,
) -> Result<BundleFile, ConnectionError> {
    let relative = file
        .relative
        .as_ref()
        .ok_or(ConnectionError::AccountMissing)?;
    let bytes = read_reviewed(reviewed, relative, MAX_PLAINTEXT as u64)
        .map_err(|_| ConnectionError::AccountMissing)?;
    Ok(BundleFile {
        source_id: file.source_id.clone(),
        name: file.name.clone(),
        bytes,
    })
}

struct Bounded<'a>(&'a mut Vec<u8>);

impl std::io::Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_PLAINTEXT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Thunderbird selection exceeds its bound",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Authenticate the artifact against the same setup binding and the exact
/// reviewed selection, then publish canonical Mail accounts and local mail
/// under `target_home`. An existing account with the same email is reused;
/// its configuration is never overwritten with Thunderbird's.
///
/// # Errors
/// Refuses a wrong key/binding/selection, changed target, conflicting account
/// or unavailable destination storage. No account is reported created unless
/// its metadata and selected local mail were written.
pub fn import_selected(
    target_home: &Path,
    sealed: &SealedThunderbird,
    key: &SymmetricKey,
    setup_binding: &[u8],
    expected: &ThunderbirdSelection,
) -> Result<ThunderbirdImportReceipt, ConnectionError> {
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
    verify_selection(&bundle, expected)?;
    let root = target_root(target_home).map_err(|_| ConnectionError::InvalidConfiguration)?;
    let mut store = super::connect::load_store(&root)?;
    let cache = ensure_dir(target_home, &[".cache", MAIL_APP, "mail"])?;
    let retained = ensure_dir(
        target_home,
        &[".local", "share", "tilo", "mail-import", "thunderbird"],
    )?;
    let mut receipt = ThunderbirdImportReceipt {
        accounts: Vec::new(),
        address_books: 0,
        calendars: 0,
    };
    for profile in &bundle.profiles {
        for account in &profile.accounts {
            let selected = expected
                .accounts
                .iter()
                .find(|selected| selected.source_id == account.inventory.source_id)
                .ok_or(ConnectionError::AccountMissing)?;
            let (id, created) = publish_account(&mut store, &account.inventory, &account.metadata)?;
            // Read the account's kept mail once, so every selected folder is
            // merged into one file and messages already kept by the Mail app
            // keep their read/starred state, folder and labels.
            let stem = file_stem(id.as_str());
            let files = cache.join(format!("{stem}.files"));
            let mut seen = read_cache_ids(&cache, &id)?;
            let mut inserted = Vec::new();
            let mut folders = Vec::new();
            for folder in &account.folders {
                if !selected
                    .folders
                    .iter()
                    .any(|id| id == &folder.inventory.source_id)
                {
                    continue;
                }
                let parsed = messages(folder, &account.inventory, &files, &mut seen)?;
                let messages = parsed.len() as u64;
                inserted.extend(parsed);
                retain_folder(
                    &retained,
                    &profile.source_id,
                    &account.inventory.source_id,
                    folder,
                )?;
                folders.push(ThunderbirdFolderReceipt {
                    source_id: folder.inventory.source_id.clone(),
                    messages,
                    bytes: folder.inventory.bytes,
                    local_only: folder.inventory.local_only,
                });
            }
            if !inserted.is_empty() {
                publish_cache(&cache, &id, &inserted)?;
            }
            receipt.accounts.push(ThunderbirdAccountReceipt {
                source_id: account.inventory.source_id.clone(),
                created,
                needs_reconnect: account.inventory.needs_reconnect,
                folders,
            });
        }
        for book in &profile.address_books {
            if expected
                .accounts
                .iter()
                .any(|account| account.address_books.iter().any(|id| id == &book.source_id))
            {
                retain_file(&retained, &profile.source_id, "address-books", book)?;
                personal_data::stage(
                    target_home,
                    &profile.source_id,
                    &book.source_id,
                    personal_data::Sidecar::AddressBook,
                    &book.bytes,
                )
                .map_err(|_| ConnectionError::StorageUnavailable)?;
                receipt.address_books += 1;
            }
        }
        for calendar in &profile.calendars {
            if expected
                .accounts
                .iter()
                .any(|account| account.calendars.iter().any(|id| id == &calendar.source_id))
            {
                retain_file(&retained, &profile.source_id, "calendars", calendar)?;
                personal_data::stage(
                    target_home,
                    &profile.source_id,
                    &calendar.source_id,
                    personal_data::Sidecar::Calendar,
                    &calendar.bytes,
                )
                .map_err(|_| ConnectionError::StorageUnavailable)?;
                receipt.calendars += 1;
            }
        }
    }
    store
        .save_in(&root)
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    Ok(receipt)
}

fn verify_selection(
    bundle: &Bundle,
    expected: &ThunderbirdSelection,
) -> Result<(), ConnectionError> {
    if expected.accounts.is_empty() || expected.accounts.len() > MAX_ACCOUNTS {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut actual = Vec::new();
    for profile in &bundle.profiles {
        for account in &profile.accounts {
            let selected = expected
                .accounts
                .iter()
                .find(|selected| selected.source_id == account.inventory.source_id)
                .ok_or(ConnectionError::InvalidConfiguration)?;
            let mut folders: Vec<_> = account
                .folders
                .iter()
                .map(|folder| folder.inventory.source_id.clone())
                .collect();
            let mut expected_folders = selected.folders.clone();
            folders.sort();
            expected_folders.sort();
            if folders != expected_folders {
                return Err(ConnectionError::InvalidConfiguration);
            }
            actual.push(account.inventory.source_id.clone());
        }
    }
    let mut expected_ids: Vec<_> = expected
        .accounts
        .iter()
        .map(|account| account.source_id.clone())
        .collect();
    actual.sort();
    expected_ids.sort();
    if actual != expected_ids {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(())
}

fn publish_account(
    store: &mut AccountStore,
    inventory: &ThunderbirdAccount,
    metadata: &AccountMetadata,
) -> Result<(AccountId, bool), ConnectionError> {
    let email = inventory
        .email
        .as_deref()
        .filter(|email| email.contains('@'))
        .map(str::to_ascii_lowercase);
    let id = email.as_ref().map_or_else(
        || AccountId::new(format!("thunderbird-{}", inventory.source_id)),
        AccountId::new,
    );
    let existing = store
        .get(&id)
        .or_else(|| {
            email.as_deref().and_then(|email| {
                store.all().iter().find(|account| {
                    account
                        .config
                        .get("email")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| value.eq_ignore_ascii_case(email))
                        || account
                            .config
                            .get("connected_identity")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| value.eq_ignore_ascii_case(email))
                })
            })
        })
        .cloned();
    if let Some(mut existing) = existing {
        if !matches!(
            existing.type_id.as_str(),
            "email" | accounts::thunderbird::PENDING_TYPE
        ) {
            return Err(ConnectionError::AlreadyPresent);
        }
        provenance(&mut existing, inventory, metadata);
        store.upsert(existing.clone());
        return Ok((existing.id, false));
    }
    let mut account = Account::new(id.clone(), accounts::thunderbird::PENDING_TYPE);
    account.display_name = inventory.display_name.chars().take(256).collect();
    account.config = config(inventory, metadata)?;
    provenance(&mut account, inventory, metadata);
    store.upsert(account);
    Ok((id, true))
}

fn provenance(account: &mut Account, inventory: &ThunderbirdAccount, metadata: &AccountMetadata) {
    if !account.config.is_object() {
        account.config = serde_json::json!({});
    }
    if let Some(config) = account.config.as_object_mut() {
        config.insert(
            "source_provenance".into(),
            serde_json::json!({
                "mail_client": "thunderbird",
                "profile": metadata.source_account,
                "account": metadata.server_key,
                "kind": inventory.kind,
            }),
        );
        config.insert(
            "source_thunderbird_account".into(),
            serde_json::Value::String(inventory.source_id.clone()),
        );
    }
}

fn config(
    inventory: &ThunderbirdAccount,
    metadata: &AccountMetadata,
) -> Result<serde_json::Value, ConnectionError> {
    let candidate = metadata.candidate.as_ref();
    let authentication = candidate.map_or(AuthMethod::Password, |candidate| {
        if candidate.requires_oauth() {
            AuthMethod::OAuth
        } else {
            AuthMethod::Password
        }
    });
    let mut config = candidate
        .and_then(|candidate| candidate.reviewed_config(authentication).ok())
        .unwrap_or_else(|| {
            let (imap_host, imap_port, imap_tls) =
                candidate.map_or((String::new(), 993, true), |c| {
                    (
                        c.incoming.host.clone(),
                        c.incoming.port,
                        c.incoming.transport == Transport::Tls,
                    )
                });
            let (smtp_host, smtp_port, smtp_tls) =
                candidate.map_or((String::new(), 465, true), |c| {
                    (
                        c.outgoing.host.clone(),
                        c.outgoing.port,
                        c.outgoing.transport != Transport::Plain,
                    )
                });
            AccountConfig {
                display_name: inventory.display_name.clone(),
                email: inventory.email.clone().unwrap_or_default(),
                imap_host,
                imap_port,
                imap_tls,
                smtp_host,
                smtp_port,
                smtp_tls,
                username: candidate.map_or_else(String::new, |c| c.incoming.username.clone()),
                auth: authentication,
                import_pending: true,
                password: String::new(),
            }
        });
    config.import_pending = true;
    config.password.clear();
    config
        .to_value()
        .map_err(|_| ConnectionError::InvalidConfiguration)
}

/// The kept-mail file the Mail app reads: the same wire shape as
/// `crate::model::Message` in `apps/mail`, so the app can open what import
/// writes. Mutable flags and labels belong to the person, so an existing
/// message always wins over the imported copy.
#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    messages: Vec<CachedMessage>,
}

impl Kept {
    fn empty() -> Self {
        Self {
            version: CACHE_VERSION,
            messages: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedMessage {
    id: uuid::Uuid,
    rfc_message_id: Option<String>,
    in_reply_to: Option<String>,
    from: CachedAddr,
    to: Vec<CachedAddr>,
    cc: Vec<CachedAddr>,
    subject: String,
    body: String,
    #[serde(default)]
    html_body: Option<String>,
    attachments: Vec<CachedAttachment>,
    timestamp_min: i64,
    read: bool,
    starred: bool,
    folder: CachedFolder,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    account: String,
    #[serde(default)]
    server: Option<CachedServerPlace>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedAddr {
    name: Option<String>,
    email: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedAttachment {
    filename: String,
    mime: String,
    size: u64,
    hydrated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_id: Option<String>,
    kept: bool,
}

/// The canonical system folders, spelled exactly as the Mail app stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedFolder {
    Inbox,
    Starred,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
}

/// Where a server holds a message. Imported mail is local-only, so this stays
/// absent, but the shape must match the Mail app's `ServerPlace`.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct CachedServerPlace {
    folder: CachedFolder,
    uid: u32,
    uid_validity: u32,
}

/// The message IDs an account's kept file already holds. An existing but
/// unreadable file is refused rather than replaced: it may be the person's
/// mail, and the app cannot read it either.
fn read_cache_ids(
    cache: &Path,
    account: &AccountId,
) -> Result<BTreeSet<uuid::Uuid>, ConnectionError> {
    let path = cache_path(cache, account);
    let Some(bytes) = read_private(&path, MAX_CACHE)? else {
        return Ok(BTreeSet::new());
    };
    let kept = parse_kept(Some(&bytes))?;
    Ok(kept
        .messages
        .into_iter()
        .map(|message| message.id)
        .collect())
}

fn cache_path(cache: &Path, account: &AccountId) -> PathBuf {
    cache.join(format!("{}.json", file_stem(account.as_str())))
}

/// Parse a kept-mail snapshot. A version this importer does not know is
/// treated as empty, exactly as the Mail app treats it; malformed content is
/// an error, because replacing it would destroy mail the person may still
/// recover.
fn parse_kept(bytes: Option<&[u8]>) -> Result<Kept, ConnectionError> {
    let Some(bytes) = bytes else {
        return Ok(Kept::empty());
    };
    let mut kept: Kept =
        serde_json::from_slice(bytes).map_err(|_| ConnectionError::StorageUnavailable)?;
    if kept.version != CACHE_VERSION {
        return Ok(Kept::empty());
    }
    let mut seen = BTreeSet::new();
    kept.messages.retain(|message| seen.insert(message.id));
    Ok(kept)
}

/// Merge new messages into the account's kept mail, publish it atomically and
/// only over the exact bytes that were read: a file another writer changed in
/// between is merged again instead of being overwritten.
fn publish_cache(
    cache: &Path,
    account: &AccountId,
    incoming: &[CachedMessage],
) -> Result<(), ConnectionError> {
    let path = cache_path(cache, account);
    for _ in 0..CACHE_ATTEMPTS {
        let snapshot = read_private(&path, MAX_CACHE)?;
        let mut kept = parse_kept(snapshot.as_deref())?;
        let mut seen: BTreeSet<_> = kept.messages.iter().map(|message| message.id).collect();
        for message in incoming {
            if seen.insert(message.id) {
                kept.messages.push(message.clone());
            }
        }
        kept.messages
            .sort_by_key(|message| std::cmp::Reverse(message.timestamp_min));
        let bytes = serde_json::to_vec(&kept).map_err(|_| ConnectionError::StorageUnavailable)?;
        if bytes.len() as u64 > MAX_CACHE {
            return Err(ConnectionError::StorageUnavailable);
        }
        if read_private(&path, MAX_CACHE)? != snapshot {
            continue;
        }
        return atomic_private_write(&path, &bytes)
            .map_err(|_| ConnectionError::StorageUnavailable);
    }
    Err(ConnectionError::StorageUnavailable)
}

fn messages(
    folder: &BundleFolder,
    inventory: &ThunderbirdAccount,
    files: &Path,
    seen: &mut BTreeSet<uuid::Uuid>,
) -> Result<Vec<CachedMessage>, ConnectionError> {
    let mut output = Vec::new();
    for file in &folder.files {
        if folder.inventory.format == "mbox" {
            for raw in mbox_messages(&file.bytes) {
                keep_message(
                    message(raw, &folder.inventory, inventory, None)?,
                    files,
                    seen,
                    &mut output,
                )?;
            }
        } else {
            keep_message(
                message(
                    &file.bytes,
                    &folder.inventory,
                    inventory,
                    Some(maildir_flags(&file.name)),
                )?,
                files,
                seen,
                &mut output,
            )?;
        }
    }
    Ok(output)
}

/// Insert a parsed message once, writing its attachment bytes only when it is
/// actually new so an existing kept copy keeps its state untouched.
fn keep_message(
    parsed: Option<(CachedMessage, Vec<(usize, Vec<u8>)>)>,
    files: &Path,
    seen: &mut BTreeSet<uuid::Uuid>,
    output: &mut Vec<CachedMessage>,
) -> Result<(), ConnectionError> {
    let Some((message, attachments)) = parsed else {
        return Ok(());
    };
    if !seen.insert(message.id) {
        return Ok(());
    }
    for (index, bytes) in attachments {
        write_file(&files.join(format!("{}.{index}", message.id)), &bytes)?;
    }
    output.push(message);
    Ok(())
}

/// One parsed message and the attachment bytes that belong beside the kept
/// file, written only once the message is actually inserted.
fn message(
    raw: &[u8],
    folder: &ThunderbirdFolder,
    inventory: &ThunderbirdAccount,
    maildir: Option<(bool, bool)>,
) -> Result<Option<(CachedMessage, Vec<(usize, Vec<u8>)>)>, ConnectionError> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Ok(None);
    };
    let rfc_message_id = parsed.message_id().map(std::string::ToString::to_string);
    let id = rfc_message_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map_or_else(
            || {
                let digest = blake3::hash(raw);
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(&digest.as_bytes()[..16]);
                uuid::Uuid::from_bytes(bytes)
            },
            stable_message_id,
        );
    let from = parsed
        .from()
        .and_then(|addresses| addresses.first())
        .map_or_else(
            || CachedAddr {
                name: None,
                email: "unknown@unknown".into(),
            },
            addr,
        );
    let to = parsed
        .to()
        .map(|addresses| addresses.iter().map(addr).collect())
        .unwrap_or_default();
    let cc = parsed
        .cc()
        .map(|addresses| addresses.iter().map(addr).collect())
        .unwrap_or_default();
    let body = parsed
        .body_text(0)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default();
    let html_body = parsed
        .html_bodies()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Html(html) => Some(html.to_string()),
            _ => None,
        })
        .filter(|html| !html.trim().is_empty());
    let mut attachments = Vec::new();
    let mut payloads = Vec::new();
    for (index, part) in parsed.attachments().enumerate() {
        let filename = part
            .attachment_name()
            .map_or_else(|| "attachment.bin".into(), str::to_owned);
        let bytes = part.contents().to_vec();
        let mime = mime_guess::from_path(&filename)
            .first_or_octet_stream()
            .to_string();
        attachments.push(CachedAttachment {
            filename,
            mime,
            size: bytes.len() as u64,
            hydrated: true,
            content_id: None,
            kept: true,
        });
        payloads.push((index, bytes));
    }
    let (read, starred) = maildir.unwrap_or_else(|| mbox_flags(raw));
    let (folder_key, labels) = mail_folder(&folder.hierarchy, folder.local_only);
    let timestamp_min = parsed
        .date()
        .map_or(0, |date| date.to_timestamp().saturating_div(60));
    Ok(Some((
        CachedMessage {
            id,
            rfc_message_id,
            in_reply_to: parsed
                .references()
                .as_text_list()
                .and_then(|ids| ids.first())
                .map(std::string::ToString::to_string)
                .or_else(|| parsed.in_reply_to().as_text().map(str::to_string)),
            from,
            to,
            cc,
            subject: parsed.subject().unwrap_or("(no subject)").to_string(),
            body,
            html_body,
            attachments,
            timestamp_min,
            read,
            starred,
            folder: folder_key,
            labels,
            account: inventory.source_id.clone(),
            server: None,
        },
        payloads,
    )))
}

fn addr(address: &mail_parser::Addr) -> CachedAddr {
    CachedAddr {
        name: address
            .name
            .as_ref()
            .map(std::string::ToString::to_string)
            .filter(|name| !name.is_empty()),
        email: address
            .address
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    }
}

fn stable_message_id(message_id: &str) -> uuid::Uuid {
    fn fnv1a(bytes: &[u8], key: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }
    let norm = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    let hi = fnv1a(norm.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(norm.as_bytes(), 0x0102_0304_0506_0708);
    uuid::Uuid::from_u128((u128::from(hi) << 64) | u128::from(lo))
}

fn mail_folder(hierarchy: &[String], local_only: bool) -> (CachedFolder, Vec<String>) {
    let name = hierarchy
        .last()
        .map_or("", String::as_str)
        .to_ascii_lowercase();
    let folder = match name.as_str() {
        "inbox" => CachedFolder::Inbox,
        "sent" | "sent items" | "sent mail" => CachedFolder::Sent,
        "drafts" | "draft" => CachedFolder::Drafts,
        "trash" | "deleted items" | "deleted" => CachedFolder::Trash,
        "junk" | "spam" | "junk e-mail" => CachedFolder::Junk,
        _ => CachedFolder::Archive,
    };
    let labels =
        if matches!(folder, CachedFolder::Archive) && name != "archive" && name != "archives" {
            let path = hierarchy
                .iter()
                .map(|part| {
                    part.chars()
                        .filter(|ch| !ch.is_control() && *ch != '/')
                        .take(128)
                        .collect::<String>()
                })
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            if path.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "Thunderbird/{}",
                    if local_only {
                        path
                    } else {
                        format!("{path} (local copy)")
                    }
                )]
            }
        } else {
            Vec::new()
        };
    (folder, labels)
}

fn mbox_flags(raw: &[u8]) -> (bool, bool) {
    const STATUS: &[u8] = b"x-mozilla-status:";
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|window| window == b"\n\n"))
        .unwrap_or(raw.len());
    for line in raw[..end].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() < STATUS.len() {
            continue;
        }
        let (head, value) = line.split_at(STATUS.len());
        if !head.eq_ignore_ascii_case(STATUS) {
            continue;
        }
        if let Ok(value) = std::str::from_utf8(value)
            && let Ok(value) = u32::from_str_radix(value.trim(), 16)
        {
            return ((value & 0x0001) != 0, (value & 0x0004) != 0);
        }
    }
    (false, false)
}

fn maildir_flags(name: &str) -> (bool, bool) {
    name.split_once(":2,").map_or((false, false), |(_, flags)| {
        (flags.contains('S'), flags.contains('F'))
    })
}

fn mbox_messages(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut offset = 0;
    while let Some(found) = bytes[offset..]
        .windows(5)
        .position(|window| window == b"From ")
    {
        let start = offset + found;
        if start == 0 || bytes[start - 1] == b'\n' {
            starts.push(start);
        }
        offset = start + 5;
    }
    let mut ranges = Vec::new();
    let mut message_start = None;
    for start in starts {
        if let Some(previous) = message_start.replace(start) {
            ranges.push(previous..start);
        }
    }
    if let Some(previous) = message_start {
        ranges.push(previous..bytes.len());
    }
    ranges.into_iter().map(move |range| &bytes[range])
}

fn retain_folder(
    root: &Path,
    profile: &str,
    account: &str,
    folder: &BundleFolder,
) -> Result<(), ConnectionError> {
    let mut directory = root.join(component(profile)).join(component(account));
    for name in &folder.inventory.hierarchy {
        directory.push(component(name));
    }
    ensure_private_dir(&directory)?;
    for file in &folder.files {
        let path = if folder.inventory.format == "mbox" {
            directory.join(format!("{}.mbox", component(&file.name)))
        } else {
            let mut parts = file.name.splitn(2, '/');
            let part = component(parts.next().unwrap_or("cur"));
            let name = component(parts.next().unwrap_or("message"));
            ensure_private_dir(&directory.join(&part))?;
            directory.join(part).join(name)
        };
        write_file(&path, &file.bytes)?;
    }
    Ok(())
}

fn retain_file(
    root: &Path,
    profile: &str,
    kind: &str,
    file: &BundleFile,
) -> Result<(), ConnectionError> {
    let directory = root
        .join(component(profile))
        .join(kind)
        .join(component(&file.source_id));
    ensure_private_dir(&directory)?;
    write_file(&directory.join(component(&file.name)), &file.bytes)
}

fn component(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '@' | '+') {
                ch
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "item".into()
    } else {
        name
    }
}

fn file_stem(account: &str) -> String {
    account
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '-' | '_' | '+') {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn ensure_dir(base: &Path, parts: &[&str]) -> Result<PathBuf, ConnectionError> {
    let mut path = base.to_path_buf();
    for part in parts {
        path.push(part);
        ensure_private_dir(&path)?;
    }
    Ok(path)
}

fn ensure_private_dir(path: &Path) -> Result<(), ConnectionError> {
    match std::fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() && !info.is_symlink() => {}
        Ok(_) => return Err(ConnectionError::InvalidConfiguration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(ConnectionError::InvalidConfiguration)?;
            ensure_private_dir(parent)?;
            std::fs::create_dir(path).map_err(|_| ConnectionError::StorageUnavailable)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| ConnectionError::StorageUnavailable)?;
            }
        }
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    }
    Ok(())
}

/// Read one bounded, non-redirected file. A missing directory or file is
/// `None`; anything else that cannot be read safely is an error, never an
/// empty file that would authorize replacing it.
fn read_private(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, ConnectionError> {
    use crate::migration_directory::PinnedDirectory;
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    let parent_path = path.parent().ok_or(ConnectionError::InvalidConfiguration)?;
    let name = path
        .file_name()
        .ok_or(ConnectionError::InvalidConfiguration)?;
    let parent = match PinnedDirectory::open_absolute(parent_path) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = match cap_primitives::fs::open(&parent.file, Path::new(name), &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    };
    let metadata = file
        .metadata()
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(ConnectionError::StorageUnavailable);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    parent
        .validate()
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    if bytes.len() as u64 > limit {
        return Err(ConnectionError::StorageUnavailable);
    }
    Ok(Some(bytes))
}

/// Write `bytes` beside `path` and move them over it, so a reader never sees
/// half a file and a crash leaves the previous one intact.
fn atomic_private_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Kept mail needs a parent"))?;
    ensure_private_dir(parent)
        .map_err(|_| std::io::Error::other("Kept mail folder is unavailable"))?;
    let temporary = parent.join(format!(".tilo-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ConnectionError> {
    match std::fs::symlink_metadata(path) {
        Ok(info) if info.is_file() && !info.is_symlink() => {
            let existing = std::fs::read(path).map_err(|_| ConnectionError::StorageUnavailable)?;
            return if existing == bytes {
                Ok(())
            } else {
                Err(ConnectionError::AlreadyPresent)
            };
        }
        Ok(_) => return Err(ConnectionError::InvalidConfiguration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    }
    atomic_private_write(path, bytes).map_err(|_| ConnectionError::StorageUnavailable)
}

#[cfg(test)]
mod tests;
