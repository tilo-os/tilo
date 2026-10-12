//! Explicit, password-authenticated continuation of owned prepared connections.
//! The draft index owns the current immutable generation; no source grant is restored.
use super::{PreparedAccounts, PreparedEntry};
use crate::setup::{
    account_transfer::AccountActions,
    migration::PinnedDirectory,
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
use zeroize::Zeroizing;

const LIMITS: Limits = Limits {
    plaintext_bytes: 32 * 1024 * 1024,
    entries: 16,
    metadata_bytes: 256 * 1024,
};
const MANIFEST_LIMIT: u64 = 128 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::setup::journey) struct Receipt {
    pub(in crate::setup::journey) schema: u32,
    pub(in crate::setup::journey) filename: String,
    pub(in crate::setup::journey) sealed_bytes: u64,
    pub(in crate::setup::journey) ciphertext_sha256: [u8; 32],
    pub(in crate::setup::journey) binding: String,
    pub(in crate::setup::journey) group: [u8; 32],
    pub(in crate::setup::journey) accounts: usize,
}
impl Receipt {
    pub(in crate::setup::journey) fn validate(&self) -> io::Result<()> {
        let name = self.filename.strip_suffix(".sealed").ok_or_else(invalid)?;
        if self.schema != 1
            || uuid::Uuid::parse_str(name).is_err()
            || format!(
                "{}.sealed",
                uuid::Uuid::parse_str(name).map_err(|_| invalid())?
            ) != self.filename
            || self.sealed_bytes == 0
            || self.sealed_bytes > LIMITS.sealed_bound().ok_or_else(invalid)?
            || self.accounts > 64
            || self.binding.is_empty()
            || self.binding.len() > 256
            || self.binding.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    binding: String,
    group: [u8; 32],
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    selected: PreparedEntry,
    account: accounts::Account,
}
fn invalid() -> io::Error {
    io::Error::other("Prepared connections could not be authenticated")
}
fn private_temp() -> io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .prefix("tilo-account-checkpoint-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}
fn directory(path: &Path) -> io::Result<(Arc<PinnedDirectory>, StorageDirectory)> {
    let pin = PinnedDirectory::open_absolute(path)?;
    pin.validate_destination()?;
    let storage = StorageDirectory::from_file(
        crate::setup::migration_durability::readable_directory(&pin.file)?,
    )?;
    Ok((pin, storage))
}
fn read(storage: &StorageDirectory, name: &str, bound: u64) -> io::Result<Vec<u8>> {
    let mut file = storage.open_read(name.as_ref(), Privacy::OwnerOnly)?;
    if file.metadata()?.len() > bound {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(bound.checked_add(1).ok_or_else(invalid)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        return Err(invalid());
    }
    Ok(bytes)
}
fn manifest(handle: &PreparedAccounts, binding: &str, group: [u8; 32]) -> io::Result<Manifest> {
    let store = accounts::AccountStore::load_in(&handle.config_root());
    let entries = handle
        .entries
        .iter()
        .map(|selected| {
            let account = store
                .get(&accounts::AccountId::new(&selected.id))
                .cloned()
                .ok_or_else(invalid)?;
            Ok(Entry {
                selected: selected.clone(),
                account,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let manifest = Manifest {
        schema: 1,
        binding: binding.into(),
        group,
        entries,
    };
    validate_manifest(&manifest)?;
    Ok(manifest)
}
fn validate_manifest(manifest: &Manifest) -> io::Result<()> {
    if manifest.schema != 1
        || manifest.entries.len() > 64
        || manifest.binding.is_empty()
        || manifest.binding.len() > 256
        || manifest.binding.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for entry in &manifest.entries {
        if entry.selected.id != entry.account.id.as_str()
            || entry.selected.id.is_empty()
            || entry.selected.id.len() > 256
            || entry
                .selected
                .id
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.' | ':'))
            || !ids.insert(&entry.selected.id)
            || entry.account.owner.is_some()
            || entry.selected.label.len() > 4096
            || entry.selected.label.chars().any(char::is_control)
            || entry
                .selected
                .source_id
                .as_ref()
                .is_some_and(|id| id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Publish a new immutable generation. The caller commits its receipt in the private draft.
pub(in crate::setup::journey) fn save(
    handle: &PreparedAccounts,
    root: &Path,
    binding: &str,
    group: [u8; 32],
    password: &[u8],
) -> io::Result<Receipt> {
    let _custody = checkpoint_authority(handle, root, binding, group)?;
    ensure_checkpoint_directory(root)?;
    let (pin, _) = directory(root)?;
    let filename = format!("{}.sealed", uuid::Uuid::new_v4());
    let package = root.join(&filename);

    let temp = private_temp()?;
    let tree = temp.path().join("tree");
    std::fs::create_dir(&tree)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700))?;
    let manifest = manifest(handle, binding, group)?;
    let mut bounded = BoundedMetadata(Vec::new());
    serde_json::to_writer(&mut bounded, &manifest).map_err(|_| invalid())?;
    let bytes = bounded.0;
    let mut file = tilo_files::native::private::create_new(&tree.join("manifest.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if !handle.entries.is_empty() {
        handle
            .export_password_sealed(&tree.join("accounts.sealed"), password, binding.as_bytes())
            .map_err(|_| invalid())?;
    }
    let cancel = AtomicBool::new(false);
    ExportReview::review(&tree, &package, Purpose::PreparationAccounts, LIMITS)?
        .export(Zeroizing::new(password.to_vec()), &cancel)?;
    pin.validate_destination()?;
    let reviewed = ImportReview::review(
        &package,
        password,
        Purpose::PreparationAccounts,
        LIMITS,
        &cancel,
    )?;
    let receipt = Receipt {
        schema: 1,
        filename,
        sealed_bytes: reviewed.info().sealed_bytes,
        ciphertext_sha256: reviewed.ciphertext_digest(),
        binding: binding.into(),
        group,
        accounts: handle.count(),
    };
    receipt.validate()?;
    Ok(receipt)
}

/// Explicitly authenticate the exact draft generation into fresh private custody.
pub(in crate::setup::journey) fn restore(
    root: &Path,
    receipt: &Receipt,
    password: &[u8],
) -> io::Result<PreparedAccounts> {
    receipt.validate()?;
    let (pin, storage) = directory(root)?;
    let sealed = read(
        &storage,
        &receipt.filename,
        LIMITS.sealed_bound().ok_or_else(invalid)?,
    )?;
    if sealed.len() as u64 != receipt.sealed_bytes
        || <[u8; 32]>::from(Sha256::digest(&sealed)) != receipt.ciphertext_sha256
    {
        return Err(invalid());
    }
    let cancel = AtomicBool::new(false);
    let reviewed = ImportReview::review(
        &root.join(&receipt.filename),
        password,
        Purpose::PreparationAccounts,
        LIMITS,
        &cancel,
    )?;
    if reviewed.ciphertext_digest() != receipt.ciphertext_sha256 {
        return Err(invalid());
    }
    pin.validate_destination()?;
    let quarantine = private_temp()?;
    let tree = reviewed.extract(
        Zeroizing::new(password.to_vec()),
        &quarantine.path().join("tree"),
        &cancel,
    )?;
    let (_, extracted) = directory(&tree.root)?;
    let manifest: Manifest =
        serde_json::from_slice(&read(&extracted, "manifest.json", MANIFEST_LIMIT)?)
            .map_err(|_| invalid())?;
    validate_manifest(&manifest)?;
    if manifest.binding != receipt.binding
        || manifest.group != receipt.group
        || manifest.entries.len() != receipt.accounts
    {
        return Err(invalid());
    }
    let expected_names: BTreeSet<_> = if manifest.entries.is_empty() {
        ["manifest.json"].into_iter().collect()
    } else {
        ["manifest.json", "accounts.sealed"].into_iter().collect()
    };
    let names = std::fs::read_dir(&tree.root)?
        .map(|entry| {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(invalid());
            }
            entry.file_name().into_string().map_err(|_| invalid())
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    if names.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected_names {
        return Err(invalid());
    }
    let stage = Arc::new(private_temp()?);
    let actions = AccountActions::default();
    let connector = actions.connector(stage.path()).map_err(|_| invalid())?;
    if !manifest.entries.is_empty() {
        let ids = manifest
            .entries
            .iter()
            .map(|entry| entry.account.id.clone())
            .collect::<Vec<_>>();
        connector
            .import_selected_accounts_matching(
                &tree.root.join("accounts.sealed"),
                password,
                receipt.binding.as_bytes(),
                &ids,
            )
            .map_err(|_| invalid())?;
        let store = accounts::AccountStore::load_in(&stage.path().join(".config/tilo"));
        for entry in &manifest.entries {
            if store.get(&entry.account.id) != Some(&entry.account) {
                return Err(invalid());
            }
        }
    }
    Ok(PreparedAccounts {
        stage,
        entries: manifest
            .entries
            .into_iter()
            .map(|entry| entry.selected)
            .collect(),
        guard: actions.destination_snapshot(),
    })
}

pub(super) enum Completion {
    Saved(Receipt, PreparedAccounts),
    Restored(PreparedAccounts, Option<advisory_lock::FileLock>),
}
pub(super) struct Worker {
    restoring: bool,
    result: std::sync::mpsc::Receiver<io::Result<Completion>>,
}
impl Worker {
    pub(super) fn save(
        handle: Option<PreparedAccounts>,
        guard: crate::setup::account_transfer::DestinationGuard,
        root: std::path::PathBuf,
        binding: String,
        group: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(false, move || {
            let handle = match handle {
                Some(handle) => handle,
                None => PreparedAccounts {
                    stage: Arc::new(private_temp()?),
                    entries: Vec::new(),
                    guard,
                },
            };
            let receipt = save(&handle, &root, &binding, group, &password)?;
            Ok(Completion::Saved(receipt, handle))
        })
    }
    #[cfg(test)]
    pub(super) fn restore(
        root: std::path::PathBuf,
        receipt: Receipt,
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(true, move || {
            restore(&root, &receipt, &password).map(|handle| Completion::Restored(handle, None))
        })
    }
    pub(super) fn restore_ready(
        ready: crate::setup::journey::draft::ReadyDraft,
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(true, move || {
            let guard = ready.acquire_current()?;
            if !ready.choices().includes_accounts() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Saved connections were left out of this preparation",
                ));
            }
            let receipt = ready.account_checkpoint().ok_or_else(invalid)?;
            let handle = restore(
                &ready.operation_directory().join("accounts"),
                receipt,
                &password,
            )?;
            Ok(Completion::Restored(handle, Some(guard)))
        })
    }
    fn spawn(
        restoring: bool,
        work: impl FnOnce() -> io::Result<Completion> + Send + 'static,
    ) -> Self {
        let (send, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(work());
        });
        Self { restoring, result }
    }
    pub(super) fn restoring(&self) -> bool {
        self.restoring
    }
    pub(super) fn poll(&self) -> Option<io::Result<Completion>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(invalid())),
        }
    }
}

struct BoundedMetadata(Vec<u8>);
impl Write for BoundedMetadata {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let total = self.0.len().checked_add(bytes.len()).ok_or_else(invalid)?;
        if total as u64 > MANIFEST_LIMIT {
            return Err(invalid());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Generic artifacts retain the original-home exclusion. Only an exact current
// acknowledged installer operation may publish its reserved encrypted checkpoint
// within that home; the lease excludes replacing that operation during export.
fn checkpoint_authority(
    handle: &PreparedAccounts,
    root: &Path,
    binding: &str,
    group: [u8; 32],
) -> io::Result<Option<advisory_lock::FileLock>> {
    if root.file_name() != Some(std::ffi::OsStr::new("accounts")) {
        return Err(invalid());
    }
    let operation = root.parent().ok_or_else(invalid)?;
    if handle.guard.validate_destination(operation).is_ok() {
        return Ok(None);
    }
    let preparations = operation.parent().ok_or_else(invalid)?;
    if super::super::personal::preparation_namespace().as_deref() != Some(preparations) {
        return Err(invalid());
    }
    let admitted = PinnedDirectory::open_absolute(preparations)?;
    admitted.validate_destination()?;
    let store = super::super::draft::DraftStore::open(preparations)?;
    let super::super::draft::DraftStatus::Ready(ready) = store.review()? else {
        return Err(invalid());
    };
    if ready.operation_directory() != operation
        || operation.file_name().and_then(std::ffi::OsStr::to_str) != Some(binding)
        || ready.acknowledged_public_key() != Some(group)
    {
        return Err(invalid());
    }
    ready.acquire_current().map(Some)
}

fn ensure_checkpoint_directory(root: &Path) -> io::Result<()> {
    if root.file_name() != Some(std::ffi::OsStr::new("accounts")) {
        return Err(invalid());
    }
    let parent = PinnedDirectory::open_absolute(root.parent().ok_or_else(invalid)?)?;
    parent.validate_destination()?;
    match std::fs::symlink_metadata(root) {
        Ok(_) => {
            directory(root)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent.create_child(std::ffi::OsStr::new("accounts"))?;
        }
        Err(error) => return Err(error),
    }
    parent.validate_destination()?;
    Ok(())
}
