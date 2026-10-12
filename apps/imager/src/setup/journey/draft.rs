//! Private restart pointers. These records contain no grants or password material.
use super::super::{
    destination_security, migration::PinnedDirectory, prepared_identity::PreparedIdentity,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

const LIMIT: u64 = 32 * 1024;
const INDEX: &str = "draft.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DestinationChoice {
    Computer,
    Another,
    Desktop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum IdentityKind {
    New,
    Backup,
    Enrolled,
    CurrentDevice,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicChoices {
    pub destination: Option<DestinationChoice>,
    pub migrate: Option<bool>,
    pub categories: [bool; 3],
    #[serde(default = "default_browser_domains")]
    pub browser_domains: [bool; 2],
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub excluded_flatpak_apps: std::collections::BTreeSet<String>,
    pub device_label: String,
    #[serde(default = "install_profile_model::default_purpose")]
    pub purpose: String,
}
fn default_browser_domains() -> [bool; 2] {
    [true; 2]
}
impl PublicChoices {
    #[cfg(target_os = "linux")]
    pub(super) fn includes_accounts(&self) -> bool {
        self.migrate == Some(true) && self.categories[2]
    }
    fn validate(&self) -> io::Result<()> {
        if !install_profile_model::valid_purpose(&self.purpose) {
            return Err(invalid());
        }
        if self.excluded_flatpak_apps.len() > 4096
            || self
                .excluded_flatpak_apps
                .iter()
                .any(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
        {
            return Err(invalid());
        }
        if self.device_label.len() > 64 || self.device_label.chars().any(char::is_control) {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    operation: String,
    kind: IdentityKind,
    root_key: Option<[u8; 32]>,
    device_key: Option<[u8; 32]>,
    record_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_device: Option<tilo_identity_host::enrolled_transport::CurrentDeviceDeclaration>,
}
impl Pointer {
    fn validate(&self) -> io::Result<()> {
        if uuid::Uuid::parse_str(&self.operation)
            .map_err(|_| invalid())?
            .to_string()
            != self.operation
        {
            return Err(invalid());
        }
        if self.current_device.is_some()
            != (self.kind == IdentityKind::CurrentDevice && self.root_key.is_some())
        {
            return Err(invalid());
        }
        if let Some(current) = &self.current_device {
            if Some(current.public_key()) != self.root_key
                || Some(current.device_public_key()) != self.device_key
                || Some(current.record_name()) != self.record_name.as_deref()
            {
                return Err(invalid());
            }
        }
        match (self.kind, self.root_key, self.device_key, &self.record_name) {
            (_, None, None, None)
            | (IdentityKind::New | IdentityKind::Backup, Some(_), None, None) => Ok(()),
            (
                IdentityKind::Enrolled | IdentityKind::CurrentDevice,
                Some(_),
                Some(_),
                Some(name),
            ) if Path::new(name).extension() == Some(OsStr::new("json"))
                && name.len() <= 128
                && !name.contains(['/', '\\'])
                && name != "."
                && name != ".."
                && !name.chars().any(char::is_control) =>
            {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    choices: Option<PublicChoices>,
    preparation: Option<Pointer>,
    #[serde(default)]
    #[cfg(target_os = "linux")]
    account_checkpoint: Option<super::accounts::checkpoint::Receipt>,
}
#[derive(Clone)]
pub(super) struct DraftStore {
    root: PathBuf,
    pin: Arc<PinnedDirectory>,
}
#[derive(Clone)]
pub(super) struct PendingDraft {
    store: DraftStore,
    pointer: Pointer,
    choices: PublicChoices,
}
#[derive(Clone)]
pub(super) struct ReadyDraft {
    pending: PendingDraft,
    #[cfg(target_os = "linux")]
    account_checkpoint: Option<super::accounts::checkpoint::Receipt>,
}
pub(super) enum DraftStatus {
    Missing,
    Pending(PendingDraft),
    Ready(ReadyDraft),
}
fn invalid() -> io::Error {
    io::Error::other("Installer restart draft is invalid; retained preparation was not changed")
}

impl DraftStore {
    pub(super) fn for_session() -> io::Result<Self> {
        let identity = super::personal::preparation_directory().ok_or_else(invalid)?;
        let root = identity
            .parent()
            .and_then(Path::parent)
            .ok_or_else(invalid)?;
        Self::open(root)
    }
    pub(super) fn open(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() || root.file_name() != Some(OsStr::new("preparations")) {
            return Err(invalid());
        }
        let pin = super::personal::ensure_private_tree(root)?;
        destination_security::check_private_file(&pin.file)?;
        pin.validate_destination()?;
        Ok(Self {
            root: root.to_owned(),
            pin,
        })
    }
    fn storage(&self) -> io::Result<StorageDirectory> {
        self.pin.validate_destination()?;
        destination_security::check_private_file(&self.pin.file)?;
        StorageDirectory::from_file(super::super::migration_durability::readable_directory(
            &self.pin.file,
        )?)
    }
    fn lock(&self) -> io::Result<advisory_lock::FileLock> {
        let held = self.storage()?;
        match held.publish(
            OsStr::new("draft.lock"),
            &[],
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            Ok(()) => {}
            Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into_io_error()),
        }
        let file = held.open_append_existing(OsStr::new("draft.lock"), Privacy::OwnerOnly)?;
        advisory_lock::FileLock::try_exclusive(file).map_err(io::Error::other)
    }
    fn read(&self) -> io::Result<Option<Record>> {
        let held = self.storage()?;
        let file = match held.open_read(OsStr::new(INDEX), Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > LIMIT {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(invalid());
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if record.schema != 1 || record.preparation.is_some() != record.choices.is_some() {
            return Err(invalid());
        }
        if let Some(choices) = &record.choices {
            choices.validate()?;
        }
        if let Some(pointer) = &record.preparation {
            pointer.validate()?;
        }
        #[cfg(target_os = "linux")]
        if let Some(receipt) = &record.account_checkpoint {
            receipt.validate()?;
            let pointer = record.preparation.as_ref().ok_or_else(invalid)?;
            if receipt.binding != pointer.operation || Some(receipt.group) != pointer.root_key {
                return Err(invalid());
            }
        }
        self.pin.validate()?;
        Ok(Some(record))
    }
    fn write(&self, record: &Record) -> io::Result<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| invalid())?;
        if bytes.len() as u64 > LIMIT {
            return Err(invalid());
        }
        self.storage()?
            .publish(
                OsStr::new(INDEX),
                &bytes,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        self.pin.validate()
    }
    pub(super) fn review(&self) -> io::Result<DraftStatus> {
        let Some(record) = self.read()? else {
            return Ok(DraftStatus::Missing);
        };
        let Some(pointer) = record.preparation else {
            return Ok(DraftStatus::Missing);
        };
        let pending = PendingDraft {
            store: self.clone(),
            pointer,
            choices: record.choices.ok_or_else(invalid)?,
        };
        if pending.pointer.root_key.is_some() {
            Ok(DraftStatus::Ready(ReadyDraft {
                pending,
                #[cfg(target_os = "linux")]
                account_checkpoint: record.account_checkpoint,
            }))
        } else {
            Ok(DraftStatus::Pending(pending))
        }
    }
    /// Reserve before minting keys. Failure after this commit remains Pending.
    pub(super) fn reserve(
        &self,
        choices: &PublicChoices,
        kind: IdentityKind,
    ) -> io::Result<PendingDraft> {
        let _lock = self.lock()?;
        if !matches!(self.review()?, DraftStatus::Missing) {
            return Err(invalid());
        }
        choices.validate()?;
        let pointer = Pointer {
            operation: uuid::Uuid::new_v4().to_string(),
            kind,
            root_key: None,
            device_key: None,
            record_name: None,
            current_device: None,
        };
        self.write(&Record {
            schema: 1,
            choices: Some(choices.clone()),
            preparation: Some(pointer.clone()),
            #[cfg(target_os = "linux")]
            account_checkpoint: None,
        })?;
        let operation = self.pin.create_child(OsStr::new(&pointer.operation))?;
        super::super::migration_durability::sync_directory(&operation.file)?;
        super::super::migration_durability::sync_directory(&self.pin.file)?;
        Ok(PendingDraft {
            store: self.clone(),
            pointer,
            choices: choices.clone(),
        })
    }
    pub(super) fn acknowledge(
        &self,
        pending: &PendingDraft,
        prepared: &Arc<PreparedIdentity>,
        record_name: Option<&str>,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        let current = self.read()?.ok_or_else(invalid)?;
        if self.root != pending.store.root || current.preparation.as_ref() != Some(&pending.pointer)
        {
            return Err(invalid());
        }
        let mut pointer = pending.pointer.clone();
        pointer.root_key = Some(prepared.public_key());
        pointer.device_key = prepared.device_public_key();
        pointer.current_device = prepared.current_declaration()?;
        pointer.record_name = record_name.map(str::to_owned);
        pointer.validate()?;
        let ready = ReadyDraft {
            #[cfg(target_os = "linux")]
            account_checkpoint: None,
            pending: PendingDraft {
                store: self.clone(),
                pointer: pointer.clone(),
                choices: pending.choices.clone(),
            },
        };
        ready.restore_custody()?;
        self.write(&Record {
            schema: 1,
            choices: current.choices,
            preparation: Some(pointer),
            #[cfg(target_os = "linux")]
            account_checkpoint: current.account_checkpoint,
        })
    }
    /// Record an already authenticated backup/enrollment in this exact namespace.
    pub(super) fn record_prepared(
        &self,
        choices: &PublicChoices,
        kind: IdentityKind,
        prepared: &Arc<PreparedIdentity>,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        choices.validate()?;
        let directory = prepared.custody_directory();
        let operation = directory.parent().ok_or_else(invalid)?;
        if operation.parent() != Some(self.root.as_path())
            || directory.file_name()
                != Some(OsStr::new(if kind == IdentityKind::Enrolled {
                    "identities"
                } else {
                    "identity"
                }))
        {
            return Err(invalid());
        }
        let pointer = Pointer {
            operation: operation
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(invalid)?
                .to_owned(),
            kind,
            root_key: Some(prepared.public_key()),
            device_key: prepared.device_public_key(),
            current_device: prepared.current_declaration()?,
            record_name: prepared.record_name().map(str::to_owned),
        };
        pointer.validate()?;
        let current = self.read()?;
        if current
            .as_ref()
            .and_then(|record| record.preparation.as_ref())
            .is_some_and(|known| {
                known.operation != pointer.operation
                    || known.kind != pointer.kind
                    || known
                        .root_key
                        .is_some_and(|key| Some(key) != pointer.root_key)
            })
        {
            return Err(invalid());
        }
        ReadyDraft {
            #[cfg(target_os = "linux")]
            account_checkpoint: None,
            pending: PendingDraft {
                store: self.clone(),
                pointer: pointer.clone(),
                choices: choices.clone(),
            },
        }
        .restore_custody()?;
        self.write(&Record {
            schema: 1,
            choices: Some(choices.clone()),
            preparation: Some(pointer),
            #[cfg(target_os = "linux")]
            account_checkpoint: current.and_then(|record| {
                choices
                    .includes_accounts()
                    .then_some(record.account_checkpoint)
                    .flatten()
            }),
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_account_checkpoint(
        &self,
        receipt: &super::accounts::checkpoint::Receipt,
        expected: Option<&super::accounts::checkpoint::Receipt>,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        receipt.validate()?;
        let mut record = self.read()?.ok_or_else(invalid)?;
        let pointer = record.preparation.as_ref().ok_or_else(invalid)?;
        if !record
            .choices
            .as_ref()
            .is_some_and(PublicChoices::includes_accounts)
            || receipt.binding != pointer.operation
            || Some(receipt.group) != pointer.root_key
        {
            return Err(invalid());
        }
        if record.account_checkpoint.as_ref() != expected {
            return Err(invalid());
        }
        record.account_checkpoint = Some(receipt.clone());
        self.write(&record)
    }

    pub(super) fn update_choices_for(
        &self,
        prepared: &Arc<PreparedIdentity>,
        choices: &PublicChoices,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        choices.validate()?;
        let mut record = self.read()?.ok_or_else(invalid)?;
        let pointer = record.preparation.as_ref().ok_or_else(invalid)?;
        let expected_directory =
            self.root
                .join(&pointer.operation)
                .join(if pointer.kind == IdentityKind::Enrolled {
                    "identities"
                } else {
                    "identity"
                });
        prepared.metadata()?;
        if prepared.custody_directory() != expected_directory
            || Some(prepared.public_key()) != pointer.root_key
            || prepared.device_public_key() != pointer.device_key
            || prepared.current_declaration()? != pointer.current_device
            || prepared.record_name() != pointer.record_name.as_deref()
        {
            return Err(invalid());
        }
        record.choices = Some(choices.clone());
        #[cfg(target_os = "linux")]
        if !choices.includes_accounts() {
            record.account_checkpoint = None;
        }
        self.write(&record)
    }
    /// Retire only the pointer. Never remove identity/provider custody.
    pub(super) fn retire(&self) -> io::Result<()> {
        let _lock = self.lock()?;
        self.read()?;
        self.write(&Record {
            schema: 1,
            choices: None,
            preparation: None,
            #[cfg(target_os = "linux")]
            account_checkpoint: None,
        })
    }
}
impl PendingDraft {
    pub(super) fn operation_directory(&self) -> PathBuf {
        self.store.root.join(&self.pointer.operation)
    }
    pub(super) fn identity_directory(&self) -> PathBuf {
        self.operation_directory()
            .join(if self.pointer.kind == IdentityKind::Enrolled {
                "identities"
            } else {
                "identity"
            })
    }
    /// Explicit recovery of New custody created before acknowledgement. Other
    /// kinds require their original external authentication again.
    pub(super) fn recover_created_new(&self) -> io::Result<Arc<PreparedIdentity>> {
        if self.kind() != IdentityKind::New || self.pointer.root_key.is_some() {
            return Err(invalid());
        }
        let _lock = self.store.lock()?;
        let current = self.store.read()?.ok_or_else(invalid)?;
        if current.preparation.as_ref() != Some(&self.pointer) {
            return Err(invalid());
        }
        let identity = self.identity_directory();
        let pin = PinnedDirectory::open_absolute(&identity)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        let metadata = runtime_identity::load_metadata_from_path(&identity.join("primary.json"))
            .map_err(|_| invalid())?;
        let prepared = Arc::new(PreparedIdentity::resume(
            &identity,
            metadata.group_public_key,
        )?);
        let mut pointer = self.pointer.clone();
        pointer.root_key = Some(prepared.public_key());
        self.store.write(&Record {
            schema: 1,
            choices: current.choices,
            preparation: Some(pointer),
            #[cfg(target_os = "linux")]
            account_checkpoint: current.account_checkpoint,
        })?;
        Ok(prepared)
    }

    pub(super) fn choices(&self) -> &PublicChoices {
        &self.choices
    }
    pub(super) fn kind(&self) -> IdentityKind {
        self.pointer.kind
    }
}
impl ReadyDraft {
    #[cfg(target_os = "linux")]
    pub(super) fn acknowledged_public_key(&self) -> Option<[u8; 32]> {
        self.pending.pointer.root_key
    }
    #[cfg(target_os = "linux")]
    pub(super) fn account_checkpoint(&self) -> Option<&super::accounts::checkpoint::Receipt> {
        self.choices()
            .includes_accounts()
            .then_some(self.account_checkpoint.as_ref())
            .flatten()
    }
    pub(super) fn operation_directory(&self) -> PathBuf {
        self.pending.operation_directory()
    }
    pub(super) fn choices(&self) -> &PublicChoices {
        self.pending.choices()
    }
    pub(super) fn kind(&self) -> IdentityKind {
        self.pending.kind()
    }
    pub(super) fn resume(&self) -> io::Result<Arc<PreparedIdentity>> {
        self.with_current(|| self.restore_custody())
    }
    /// Fence restoration against selection replacement until authentication and
    /// fresh staging finish. Public presentation choices may change independently.
    pub(super) fn with_current<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let _guard = self.acquire_current()?;
        action()
    }
    /// Retain generation custody through worker delivery and receiver acceptance.
    pub(super) fn acquire_current(&self) -> io::Result<advisory_lock::FileLock> {
        let guard = self.pending.store.lock()?;
        let current = self.pending.store.read()?.ok_or_else(invalid)?;
        let matches = current.preparation.as_ref() == Some(&self.pending.pointer);
        #[cfg(target_os = "linux")]
        let matches = matches && current.account_checkpoint == self.account_checkpoint;
        if !matches {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Prepared setup changed; review the current preparation again",
            ));
        }
        Ok(guard)
    }
    fn restore_custody(&self) -> io::Result<Arc<PreparedIdentity>> {
        self.pending.store.storage()?;
        let expected = self.pending.pointer.root_key.ok_or_else(invalid)?;
        if self.kind() == IdentityKind::CurrentDevice
            && self.pending.choices.destination != Some(DestinationChoice::Desktop)
        {
            return Err(invalid());
        }
        let identity = match self.kind() {
            IdentityKind::New | IdentityKind::Backup => {
                PreparedIdentity::resume(&self.pending.identity_directory(), expected)?
            }
            IdentityKind::CurrentDevice => PreparedIdentity::resume_current(
                &self.pending.identity_directory(),
                self.pending
                    .pointer
                    .current_device
                    .as_ref()
                    .ok_or_else(invalid)?,
            )?,
            IdentityKind::Enrolled => {
                let record = self.pending.identity_directory().join(
                    self.pending
                        .pointer
                        .record_name
                        .as_ref()
                        .ok_or_else(invalid)?,
                );
                let now = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|_| invalid())?
                        .as_nanos(),
                )
                .map_err(|_| invalid())?;
                let enrolled = tilo_identity_host::enrolled_transport::EnrolledDevice::review(
                    &self.pending.operation_directory().join("future-device"),
                    &record,
                    expected,
                    self.pending.pointer.device_key.ok_or_else(invalid)?,
                    now,
                )?;
                PreparedIdentity::from_enrolled(Arc::new(enrolled))?
            }
        };
        Ok(Arc::new(identity))
    }
}
#[cfg(all(test, unix))]
mod tests;
