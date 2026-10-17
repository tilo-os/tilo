//! Held, root-private durable capture intent and custody receipts.
use super::super::{migration_durability, nofollow::metadata_nofollow};
use super::*;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::{ffi::OsStr, io::Read};
use tilo_files::native::atomic::{
    Privacy, PublishMode, StorageDirectory,
    lease::{FileLease, LeaseMode},
};

mod installed_copy_attempt;
pub(in crate::setup::home_capture) use installed_copy_attempt::CompletedCleanup;
pub(super) mod original;
mod output_attempt;
pub use installed_copy_attempt::{InstalledCopyAttempt, InstalledCopyRecovery};
pub(in crate::setup::home_capture) mod retained;
mod scratch;
use original::SourceIntent;
pub use output_attempt::{
    CommittedOriginalApplicationsSelection, CommittedOriginalSelection,
    lookup_committed_original_applications_selection, lookup_committed_original_selection,
    review_committed_original_applications_selection, review_committed_original_selection,
    review_uncommitted_original_applications_selection, review_uncommitted_original_selection,
};
pub(super) use output_attempt::{OriginalOutputExpectations, StageBinding};
pub use scratch::BackingWitness;
pub(super) use scratch::{ScratchLease, ScratchReceipt};

const MAX_RECORD: u64 = 64 * 1024;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    filesystem: [u8; 16],
    id: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    readonly: bool,
}
impl From<&btrfs::SubvolumeIdentity> for Identity {
    fn from(value: &btrfs::SubvolumeIdentity) -> Self {
        Self {
            filesystem: value.filesystem,
            id: value.id,
            uuid: value.uuid,
            parent_uuid: value.parent_uuid,
            readonly: value.readonly,
        }
    }
}
impl From<&Identity> for btrfs::SubvolumeIdentity {
    fn from(value: &Identity) -> Self {
        Self {
            filesystem: value.filesystem,
            id: value.id,
            uuid: value.uuid,
            parent_uuid: value.parent_uuid,
            readonly: value.readonly,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Capture {
    snapshot: Identity,
    pub(super) expires_unix: u64,
    reserve_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    schema: u32,
    pub(super) grant: Grant,
    source: Identity,
    relative: NativePath,
    pub(super) state: State,
    pub(super) capture: Option<Capture>,
    #[serde(default)]
    pub(super) worker: Option<super::process::Custody>,
}
impl Record {
    pub(super) fn intent(
        grant: Grant,
        source: &btrfs::SubvolumeIdentity,
        relative: &Path,
    ) -> io::Result<Self> {
        operation(&grant.operation)?;
        if source.uuid != grant.subvolume || source.filesystem != grant.filesystem {
            return Err(failure(
                "capture intent source does not match original consent",
            ));
        }
        Ok(Self {
            schema: 1,
            grant,
            source: source.into(),
            relative: NativePath::encode(relative),
            state: State::Intended,
            capture: None,
            worker: None,
        })
    }

    pub(super) fn snapshot_name(&self) -> String {
        format!("home-{}", self.grant.operation)
    }

    pub(super) fn set_ready(&mut self, receipt: &Receipt) -> io::Result<()> {
        if receipt.name != self.snapshot_name()
            || Identity::from(&receipt.source) != self.source
            || NativePath::encode(&receipt.selected_relative) != self.relative
            || receipt.selected_inode != self.grant.inode
            || receipt.selected_owner != self.grant.owner
            || !receipt.snapshot.readonly
            || receipt.snapshot.parent_uuid != self.source.uuid
        {
            return Err(failure("captured receipt does not match durable intent"));
        }
        self.capture = Some(Capture {
            snapshot: (&receipt.snapshot).into(),
            expires_unix: unix_time(receipt.expires)?,
            reserve_bytes: receipt.reserve_bytes,
        });
        self.state = State::Ready;
        Ok(())
    }

    pub(super) fn receipt(&self) -> io::Result<Receipt> {
        let capture = self
            .capture
            .as_ref()
            .ok_or_else(|| failure("capture receipt is missing"))?;
        Ok(Receipt {
            name: self.snapshot_name(),
            source: (&self.source).into(),
            snapshot: (&capture.snapshot).into(),
            selected_relative: self.relative.decode().map_err(io::Error::other)?,
            selected_inode: self.grant.inode,
            selected_owner: self.grant.owner,
            expires: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(capture.expires_unix))
                .ok_or_else(|| failure("capture expiry overflow"))?,
            reserve_bytes: capture.reserve_bytes,
        })
    }

    fn validate(&self) -> io::Result<()> {
        operation(&self.grant.operation)?;
        if self.schema != 1
            || self.grant.schema != 1
            || self.grant.owner != self.grant.caller
            || self.source.uuid != self.grant.subvolume
            || self.source.filesystem != self.grant.filesystem
        {
            return Err(failure("invalid protected capture record"));
        }
        let relative = self.relative.decode().map_err(io::Error::other)?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(failure("invalid capture subtree"));
        }
        if self.state == State::Ready && self.capture.is_none() {
            return Err(failure("missing ready receipt"));
        }
        if let Some(capture) = &self.capture {
            if !capture.snapshot.readonly
                || capture.snapshot.parent_uuid != self.source.uuid
                || capture.snapshot.filesystem != self.source.filesystem
                || capture.reserve_bytes == 0
            {
                return Err(failure("invalid retained capture provenance"));
            }
        }
        Ok(())
    }
}

/// The operation ledger retains legacy snapshot receipts unchanged. An
/// original-volume intent carries no invented snapshot/capture identity.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum DurableRecord {
    Capture(Record),
    Original(SourceIntent),
}

pub(super) struct Store {
    records: Arc<PinnedDirectory>,
    snapshots: Arc<PinnedDirectory>,
    operation: String,
    _ownership: FileLease,
    records_path: Option<PathBuf>,
}

fn storage(pin: &Arc<PinnedDirectory>) -> io::Result<StorageDirectory> {
    pin.validate_destination()?;
    StorageDirectory::from_file(migration_durability::readable_directory(&pin.file)?)
}

fn directory(
    parent: &Arc<PinnedDirectory>,
    name: &str,
    private: bool,
) -> io::Result<Arc<PinnedDirectory>> {
    let directory = match metadata_nofollow(&parent.file, Path::new(name)) {
        Ok(metadata) => parent.open_child(OsStr::new(name), &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent.create_child(OsStr::new(name))?
        }
        Err(error) => return Err(error),
    };
    let held = storage(&directory)?;
    held.validate_namespace_control()?;
    if private {
        held.validate_privacy(Privacy::OwnerOnly)?;
    }
    migration_durability::sync_directory(&parent.file)?;
    Ok(directory)
}

impl Store {
    pub(super) fn system(operation_id: &str) -> io::Result<Self> {
        operation(operation_id)?;
        // The privileged helper owns this fixed namespace. Never repair an
        // unsafe existing directory by chmod/chown or follow a symlink.
        let parent = PinnedDirectory::open_absolute(Path::new("/var/lib"))?;
        storage(&parent)?.validate_namespace_control()?;
        let parent = directory(&parent, "tilo-imager", false)?;
        let root = directory(&parent, "home-capture", true)?;
        let mut store = Self::open(root, operation_id)?;
        store.records_path = Some(PathBuf::from("/var/lib/tilo-imager/home-capture/records"));
        Ok(store)
    }

    pub(super) fn open(root: Arc<PinnedDirectory>, operation_id: &str) -> io::Result<Self> {
        operation(operation_id)?;
        storage(&root)?.validate_privacy(Privacy::OwnerOnly)?;
        let records = directory(&root, "records", true)?;
        let snapshots = directory(&root, "snapshots", true)?;
        let held = storage(&records)?;
        let name = format!("{operation_id}.lock");
        let name = OsStr::new(&name);
        let file = match held.open_append_existing(name, Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match held.publish(name, b"", PublishMode::IfAbsent, Privacy::OwnerOnly) {
                    Ok(_) => {}
                    Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into_io_error()),
                }
                held.open_append_existing(name, Privacy::OwnerOnly)?
            }
            Err(error) => return Err(error),
        };
        let lease = FileLease::try_acquire(file, LeaseMode::Exclusive).map_err(io::Error::other)?;
        Ok(Self {
            records,
            snapshots,
            operation: operation_id.into(),
            _ownership: lease,
            records_path: None,
        })
    }

    /// The boot output record/lease must live on its held migration medium,
    /// not volatile boot state or the readonly original source filesystem.
    pub(super) fn check_original_output_filesystem(&self, device: u64) -> io::Result<()> {
        self.check_original_output_operation(&self.operation)?;
        if self.records.file.metadata()?.dev() != device
            || self.snapshots.file.metadata()?.dev() != device
            || self._ownership.metadata()?.dev() != device
        {
            return Err(failure("Original output Store is outside the held medium"));
        }
        Ok(())
    }
    /// Incomplete original output intent. IfAbsent prevents silent reuse after
    /// a lost acknowledgement/crash; restart needs separate verified recovery.
    pub(super) fn claim_original_output_intent(
        &self,
        digest: [u8; 32],
        attempt: &str,
    ) -> io::Result<(File, Vec<u8>)> {
        self.check_original_output_operation(&self.operation)?;
        if digest == [0; 32]
            || uuid::Uuid::parse_str(attempt)
                .ok()
                .map(|id| id.to_string())
                .as_deref()
                != Some(attempt)
        {
            return Err(failure("Original output intent binding is invalid"));
        }
        let mut bytes = b"TILOOUTPUT1\0".to_vec();
        bytes.extend_from_slice(&digest);
        bytes.extend_from_slice(self.operation.as_bytes());
        bytes.extend_from_slice(attempt.as_bytes());
        let directory = storage(&self.records)?;
        let name = format!("{}.original-output", self.operation);
        match directory.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
            Ok(existing) => return Err(original_output_conflict(existing, &bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Err(error) = directory.publish(
            OsStr::new(&name),
            &bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            if error.io_error().kind() == io::ErrorKind::AlreadyExists {
                let existing = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
                return Err(original_output_conflict(existing, &bytes)?);
            }
            return Err(error.into_io_error());
        }
        let file = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        self.check_original_output_intent(&file, &bytes)?;
        Ok((file, bytes))
    }
    pub(super) fn check_original_output_intent(
        &self,
        held: &File,
        expected: &[u8],
    ) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.check_original_output_operation(&self.operation)?;
        if expected.len() > 128 || held.metadata()?.len() != expected.len() as u64 {
            return Err(failure("Original output intent exceeds its bound"));
        }
        let directory = storage(&self.records)?;
        let name = format!("{}.original-output", self.operation);
        let named = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        if tilo_files::native::atomic::FileIdentity::of(held)?
            != tilo_files::native::atomic::FileIdentity::of(&named)?
        {
            return Err(failure("Original output intent name changed"));
        }
        let mut bytes = vec![0; expected.len()];
        held.read_exact_at(&mut bytes, 0)?;
        if bytes != expected {
            return Err(failure("Original output intent binding changed"));
        }
        Ok(())
    }
    /// Retain and recheck the SAME operation lease, including its named inode.
    pub(super) fn check_original_output_operation(&self, operation: &str) -> io::Result<()> {
        if self.operation != operation {
            return Err(failure(
                "Original output Store belongs to another operation",
            ));
        }
        let storage = storage(&self.records)?;
        storage.validate_privacy(Privacy::OwnerOnly)?;
        let file =
            storage.open_read(OsStr::new(&format!("{operation}.lock")), Privacy::OwnerOnly)?;
        let current = tilo_files::native::atomic::FileIdentity::of(&file)?;
        if current != self._ownership.identity()? {
            return Err(failure("Original output operation lease name changed"));
        }
        Ok(())
    }
    /// Only the canonical installed Store may supply the local-copy parent.
    pub(super) fn installed_copy_snapshots(
        &self,
        operation: &str,
    ) -> io::Result<Arc<PinnedDirectory>> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || self.records_path.as_deref()
                != Some(Path::new("/var/lib/tilo-imager/home-capture/records"))
        {
            return Err(failure(
                "Installed copy requires the canonical privileged Store",
            ));
        }
        self.check_original_output_operation(operation)?;
        storage(&self.snapshots)?.validate_privacy(Privacy::OwnerOnly)?;
        let snapshots = self.snapshots.clone();
        self.check_original_output_operation(operation)?;
        Ok(snapshots)
    }
    pub(super) fn snapshots(&self) -> io::Result<File> {
        self.snapshots.validate_destination()?;
        migration_durability::readable_directory(&self.snapshots.file)
    }

    pub(super) fn save(&self, record: &Record, initial: bool) -> io::Result<()> {
        record.validate()?;
        if record.grant.operation != self.operation {
            return Err(failure("capture record operation changed"));
        }
        let bytes = serde_json::to_vec(record)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure("capture record exceeds bound"));
        }
        let storage = storage(&self.records)?;
        storage.validate_privacy(Privacy::OwnerOnly)?;
        storage
            .publish(
                OsStr::new(&format!("{}.json", self.operation)),
                &bytes,
                if initial {
                    PublishMode::IfAbsent
                } else {
                    PublishMode::Replace
                },
                Privacy::OwnerOnly,
            )
            .map_err(io::Error::other)?;
        self.records.validate_destination()
    }

    pub(super) fn load(&self) -> io::Result<Option<Record>> {
        let storage = storage(&self.records)?;
        let file = match storage.open_read(
            OsStr::new(&format!("{}.json", self.operation)),
            Privacy::OwnerOnly,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_RECORD {
            return Err(failure("capture record exceeds bound"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure("capture record exceeds bound"));
        }
        let record = match serde_json::from_slice::<DurableRecord>(&bytes)? {
            DurableRecord::Capture(record) => record,
            DurableRecord::Original(_) => {
                return Err(failure(
                    "Original-volume intent is not a snapshot capture lease",
                ));
            }
        };
        record.validate()?;
        if record.grant.operation != self.operation {
            return Err(failure("protected operation mismatch"));
        }
        self.records.validate_destination()?;
        Ok(Some(record))
    }

    pub(super) fn recover_intended(&self, record: &Record) -> io::Result<()> {
        if let Some(worker) = &record.worker {
            worker.stop_and_observe()?;
        }
        record.validate()?;
        let name = record.snapshot_name();
        let metadata = match metadata_nofollow(&self.snapshots.file, Path::new(&name)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        self.snapshots.validate_destination()?;
        let snapshot = self.snapshots.open_child(OsStr::new(&name), &metadata)?;
        let snapshot_handle = migration_durability::readable_directory(&snapshot.file)?;
        let parent_handle = self.snapshots()?;
        let actual = btrfs::subvolume_identity(snapshot_handle.as_fd())?;
        if !actual.readonly
            || actual.parent_uuid != record.source.uuid
            || actual.filesystem != record.source.filesystem
            || snapshot.file.metadata()?.ino() != 256
            || record
                .capture
                .as_ref()
                .is_some_and(|capture| Identity::from(&actual) != capture.snapshot)
        {
            return Err(failure(
                "unfinished snapshot is not the exact broker-owned capture",
            ));
        }
        btrfs::destroy_snapshot_id(parent_handle.as_fd(), snapshot_handle.as_fd(), &actual)?;
        migration_durability::sync_directory(&self.snapshots.file)
    }
}

/// Closed diagnostics only. Existing incomplete intent is a recovery candidate,
/// never a new output grant. Future recovery reattests exact source/media and
/// verifies prior process/checkpoints before reusing anything.
fn original_output_conflict(file: File, expected: &[u8]) -> io::Result<io::Error> {
    let length = file.metadata()?.len();
    if length > 128 {
        return Err(failure("Original output prior intent exceeds its bound"));
    }
    let mut bytes = Vec::new();
    file.take(129).read_to_end(&mut bytes)?;
    if bytes.len() > 128 || !bytes.starts_with(b"TILOOUTPUT1\0") {
        return Err(failure("Original output prior intent is unsupported"));
    }
    if bytes == expected {
        Ok(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Original output same intent requires verified recovery",
        ))
    } else {
        Ok(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Original output different intent refuses replacement",
        ))
    }
}

pub use original::mounted::MountedWitness;
