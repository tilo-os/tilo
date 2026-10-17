//! Privileged custody for an approved raw Home epoch. The app receives only
//! an operation/status; snapshot paths and descriptors remain inside the broker.
use super::{
    migration::PinnedDirectory, migration_durability::readable_directory,
    migration_store::NativePath,
};
use fs_native::{
    btrfs,
    snapshot_lease::{Receipt, Request as LeaseRequest, SnapshotLease},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

pub mod original_caller;
pub use original_caller::OriginalCallerFacts;
mod installed_copy_output;
mod installed_copy_scratch;
pub use installed_copy_output::{
    InstalledCopyCompletion, InstalledCopyTransaction, InstalledVerifiedArtifacts,
};
mod inventory_scratch;
pub use installed_copy_scratch::{InstalledCopyScratch, InstalledCopyScratchFacts};
pub mod offline_output;
mod offline_read;
mod original_copy_read;
mod original_reader;
mod original_worker;
pub use offline_read::OfflineReadAdmission;
pub use original_copy_read::OriginalCopyReadAdmission;
mod command;
pub mod controller;
mod host_runner;
mod process;
pub use host_runner::maybe_run_host;
pub mod applications_qualification;
pub mod consumer_qualification;
mod rpc;
pub use rpc::RequestDirectoryPin;
mod runtime;
pub use rpc::RootSession;
pub use rpc::{Action as RpcAction, Listener as RpcListener, Rendezvous, Reply as RpcReply};
pub use rpc::{MediaReadSession, RootMediaReadSession};
pub use runtime::{TrustedCaptureService, installed_original_consumer_facts};
pub use transport::validate_read_only_block_device;
#[cfg(all(test, feature = "native-capture-qualification"))]
mod native_tests;
mod store;
#[cfg(test)]
pub use store::original::retire_original_source;
pub use store::original::{CompletedOriginalGeometry, SourceReadContext};
pub use store::retained::RetainedCallerDeclaration;
pub use store::retained::{RetainedSourceAuthority, RetainedSourceProjection, RootRetainedPermit};
mod transport;
mod worker;
pub use crate::capture::{State, Status};
pub use store::MountedWitness;
pub use store::original::SourceWitness;
use store::{Record, Store};
pub use worker::OriginalReadClaim;
pub use worker::Request as OriginalReaderRequest;
#[cfg(feature = "capture-worker")]
pub use worker::run_provider;
pub use worker::{Command, Response, Secret};
#[cfg(feature = "capture-worker")]
pub fn worker_protocol() -> u32 {
    if cfg!(feature = "capture-worker-applications") {
        worker::APPLICATIONS_PROTOCOL
    } else {
        worker::WORKER_PROTOCOL
    }
}

fn failure(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn operation(value: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(value)
        .map_err(io::Error::other)?
        .to_string()
        != value
    {
        return Err(failure("capture operation must be a canonical UUID"));
    }
    Ok(())
}

/// Absent provider support is distinct from substituted packaged evidence.
pub fn provider_supported() -> io::Result<bool> {
    match runtime::TrustedCaptureService::native() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(false),
        Err(error) => Err(error),
    }
}
pub fn require_provider() -> io::Result<()> {
    runtime::TrustedCaptureService::native().map(|_| ())
}

pub(in crate::setup) fn connect_rpc(
    rendezvous: &Rendezvous,
    directory: &Path,
    caller: u32,
    operation: &str,
    cancel: Arc<AtomicBool>,
) -> io::Result<RootSession> {
    RootSession::connect(rendezvous, directory, caller, operation, cancel)
}

/// Original scope consent; the privileged helper re-attests every field. Raw
/// paths are private native input, never a replicated UI or public action result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GrantWire")]
pub struct Grant {
    schema: u32,
    pub operation: String,
    caller: u32,
    home: NativePath,
    inode: u64,
    owner: u32,
    filesystem: [u8; 16],
    subvolume: [u8; 16],
    custody: Option<super::migration_review::CaptureCustody>,
    preparation_namespace: Option<NativePath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantWire {
    schema: u32,
    operation: String,
    caller: u32,
    home: NativePath,
    inode: u64,
    owner: u32,
    filesystem: [u8; 16],
    subvolume: [u8; 16],
    custody: Option<super::migration_review::CaptureCustody>,
    preparation_namespace: Option<NativePath>,
}

impl TryFrom<GrantWire> for Grant {
    type Error = io::Error;
    fn try_from(wire: GrantWire) -> io::Result<Self> {
        let value = Self {
            schema: wire.schema,
            operation: wire.operation,
            caller: wire.caller,
            home: wire.home,
            inode: wire.inode,
            owner: wire.owner,
            filesystem: wire.filesystem,
            subvolume: wire.subvolume,
            custody: wire.custody,
            preparation_namespace: wire.preparation_namespace,
        };
        operation(&value.operation)?;
        if value.schema != 1
            || value.caller != value.owner
            || value.inode == 0
            || value.custody.is_some() != value.preparation_namespace.is_some()
            || !value.home.decode().map_err(io::Error::other)?.is_absolute()
        {
            return Err(failure("invalid capture grant shape"));
        }
        if let Some(namespace) = &value.preparation_namespace {
            if !namespace.decode().map_err(io::Error::other)?.is_absolute() {
                return Err(failure("invalid preparation namespace"));
            }
        }
        Ok(value)
    }
}

impl Grant {
    pub fn review(home: &Path, operation_id: &str) -> io::Result<Self> {
        operation(operation_id)?;
        let pin = PinnedDirectory::open_absolute(home)?;
        let metadata = pin.file.metadata()?;
        // SAFETY: getuid reads process credentials without changing them.
        let caller = rustix::process::getuid().as_raw();
        if metadata.uid() != caller {
            return Err(failure("capture home must belong to its original user"));
        }
        let identity = btrfs::subvolume_identity(readable_directory(&pin.file)?.as_fd())?;
        let custody = super::migration_review::CaptureCustody::review(home)?;
        let preparation_namespace = if custody.is_some() {
            Some(NativePath::encode(
                &crate::session::preparation_namespace()
                    .ok_or_else(|| failure("original preparation namespace unavailable"))?,
            ))
        } else {
            None
        };
        pin.validate()?;
        Ok(Self {
            schema: 1,
            operation: operation_id.into(),
            caller,
            home: NativePath::encode(home),
            inode: metadata.ino(),
            owner: metadata.uid(),
            filesystem: identity.filesystem,
            subvolume: identity.uuid,
            custody,
            preparation_namespace,
        })
    }

    fn resolve(&self, caller: u32) -> io::Result<Resolved> {
        operation(&self.operation)?;
        if self.schema != 1 || self.caller != caller || self.owner != caller {
            return Err(failure(
                "capture grant does not belong to the authenticated caller",
            ));
        }
        let home = self.home.decode().map_err(io::Error::other)?;
        let approved = PinnedDirectory::open_absolute(&home)?;
        if let Some(custody) = &self.custody {
            let namespace = self
                .preparation_namespace
                .as_ref()
                .ok_or_else(|| failure("capture custody namespace missing"))?
                .decode()
                .map_err(io::Error::other)?;
            let namespace = PinnedDirectory::open_absolute(&namespace)?;
            if namespace.file.metadata()?.uid() != caller {
                return Err(failure("preparation namespace caller changed"));
            }
            custody.validate_original(&home, &namespace)?;
        } else if self.preparation_namespace.is_some() {
            return Err(failure("unattested preparation namespace"));
        }
        let metadata = approved.file.metadata()?;
        let identity = btrfs::subvolume_identity(readable_directory(&approved.file)?.as_fd())?;
        if metadata.ino() != self.inode
            || metadata.uid() != self.owner
            || identity.filesystem != self.filesystem
            || identity.uuid != self.subvolume
        {
            return Err(failure("approved home identity changed"));
        }
        let mut root_path = home.clone();
        let root = loop {
            approved.validate()?;
            let root = PinnedDirectory::open_absolute(&root_path)?;
            let found = btrfs::subvolume_identity(readable_directory(&root.file)?.as_fd())?;
            if found.filesystem != self.filesystem || found.uuid != self.subvolume {
                return Err(failure("capture crossed the approved subvolume boundary"));
            }
            if root.file.metadata()?.ino() == 256 {
                break root;
            }
            if !root_path.pop() {
                return Err(failure("cannot resolve the approved containing subvolume"));
            }
        };
        let relative = home
            .strip_prefix(&root_path)
            .map_err(|_| failure("invalid capture subtree"))?
            .to_path_buf();
        root.validate()?;
        approved.validate()?;
        Ok(Resolved {
            root,
            approved,
            relative,
        })
    }
}

struct Resolved {
    root: Arc<PinnedDirectory>,
    approved: Arc<PinnedDirectory>,
    relative: PathBuf,
}

/// The canonical launcher worker must own and reap all export descendants.
/// An ordinary app request cannot supply an implementation of this interface.
pub trait Worker: Send {
    fn cancel_and_reap(&mut self) -> io::Result<()>;
}

pub struct Broker {
    store: Store,
    record: Record,
    lease: Option<SnapshotLease>,
    worker: Option<Box<dyn Worker>>,
}

impl Broker {
    pub(in crate::setup) fn reopen(
        grant: Grant,
        caller: u32,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        // SAFETY: only the declared elevated helper may own capture custody.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let store = Store::system(&grant.operation)?;
        let record = store
            .load()?
            .ok_or_else(|| failure("capture operation is missing"))?;
        if record.grant != grant || record.grant.caller != caller {
            return Err(failure("capture continuation consent changed"));
        }
        Self::restore(store, record, caller, cancel)
    }
    /// Cleanup remains possible after the old HOME disappears. The protected
    /// record, caller and exact snapshot identity authorize deletion, not a fresh
    /// resolution of a no-longer-present live source.
    pub(in crate::setup) fn release_existing(grant: &Grant, caller: u32) -> io::Result<Status> {
        // SAFETY: observes process credentials; does not confer authority.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let store = Store::system(&grant.operation)?;
        let mut record = store
            .load()?
            .ok_or_else(|| failure("capture operation is missing"))?;
        if record.grant != *grant || record.grant.caller != caller {
            return Err(failure("capture cleanup consent or caller changed"));
        }
        record.state = State::Releasing;
        store.save(&record, false)?;
        store.recover_intended(&record)?;
        record.state = State::Released;
        store.save(&record, false)?;
        Ok(Status {
            operation: grant.operation.clone(),
            state: State::Released,
            expires_unix: record.capture.as_ref().map(|capture| capture.expires_unix),
        })
    }

    /// Called only by the declared elevated helper. The fixed store must be on
    /// the same filesystem; no hidden directory is created in the user's Home.
    pub(in crate::setup) fn capture(
        grant: Grant,
        caller: u32,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        // SAFETY: reads the helper's effective identity.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let store = Store::system(&grant.operation)?;
        Self::capture_in(grant, caller, cancel, store)
    }

    fn capture_in(
        grant: Grant,
        caller: u32,
        cancel: &AtomicBool,
        store: Store,
    ) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let resolved = grant.resolve(caller)?;
        let identity = btrfs::subvolume_identity(readable_directory(&resolved.root.file)?.as_fd())?;
        let parent_identity = btrfs::subvolume_identity(store.snapshots()?.as_fd())?;
        if parent_identity.filesystem != identity.filesystem {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no private capture store on the approved home filesystem",
            ));
        }
        if let Some(record) = store.load()? {
            if record.grant != grant {
                return Err(failure("capture operation consent changed"));
            }
            return Self::restore(store, record, caller, cancel);
        }
        let record = Record::intent(grant, &identity, &resolved.relative)?;
        store.save(&record, true)?; // Durable intended state precedes the ioctl.
        let mut broker = Self {
            store,
            record,
            lease: None,
            worker: None,
        };
        let request = LeaseRequest {
            name: broker.record.snapshot_name(),
            selected_relative: resolved.relative,
            lifetime: Duration::from_secs(3600),
            reserve_bytes: 256 * 1024 * 1024,
        };
        let lease = SnapshotLease::create(
            &readable_directory(&resolved.root.file)?,
            &readable_directory(&resolved.approved.file)?,
            broker.store.snapshots()?,
            request,
            cancel,
        )?;
        broker.record.set_ready(lease.receipt())?;
        // Hold native cleanup custody even if durable Ready publication fails.
        broker.lease = Some(lease);
        broker.store.save(&broker.record, false)?;
        Ok(broker)
    }

    fn restore(
        store: Store,
        mut record: Record,
        caller: u32,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        record.grant.resolve(caller)?;
        let mut broker = Self {
            store,
            record: record.clone(),
            lease: None,
            worker: None,
        };
        match record.state {
            State::Intended => {
                // No readiness was acknowledged. Delete only the snapshot whose
                // provenance the protected pre-ioctl intent admits.
                broker.store.recover_intended(&record)?;
                record.state = State::Released;
                broker.store.save(&record, false)?;
                broker.record = record;
                Err(failure(
                    "unfinished capture was recovered; use a new operation to capture again",
                ))
            }
            State::Ready => {
                if let Some(worker) = &record.worker {
                    worker.stop_and_observe()?;
                    record.worker = None;
                    broker.store.save(&record, false)?;
                    broker.record = record.clone();
                }
                let receipt = record.receipt()?;
                broker.lease = Some(SnapshotLease::restore(
                    broker.store.snapshots()?,
                    receipt,
                    cancel,
                )?);
                Ok(broker)
            }
            State::Releasing | State::Released => {
                broker.store.recover_intended(&record)?;
                record.state = State::Released;
                broker.store.save(&record, false)?;
                broker.record = record;
                Err(failure("capture has been released"))
            }
        }
    }

    pub fn status(&self) -> Status {
        Status {
            operation: self.record.grant.operation.clone(),
            state: self.record.state,
            expires_unix: self
                .record
                .capture
                .as_ref()
                .map(|capture| capture.expires_unix),
        }
    }

    /// Source identity and the only permitted omission come from protected
    /// broker custody, never from a caller-authored provider command.
    fn worker_request(&self, command: worker::Command) -> io::Result<zeroize::Zeroizing<Vec<u8>>> {
        let request = worker::Request {
            schema: worker::WORKER_PROTOCOL,
            operation: self.record.grant.operation.clone(),
            custody: self.record.grant.custody.clone(),
            command,
        };
        request.require_capture_route()?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&request)?);
        if bytes.len() > 64 * 1024 {
            return Err(failure("capture provider command exceeds bounds"));
        }
        Ok(bytes)
    }

    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.record.grant.resolve(self.record.grant.caller)?;
        self.lease
            .as_ref()
            .ok_or_else(|| failure("capture is not ready"))?
            .validate(cancel)
    }

    /// Only a running admitted command may extend the same unexpired epoch.
    /// Protected publication precedes native adoption for crash restoration.
    fn renew_active(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        self.validate(cancel)?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| failure("capture unavailable"))?;
        let remaining = lease
            .receipt()
            .expires
            .duration_since(SystemTime::now())
            .map_err(|_| failure("capture expired before renewal"))?;
        if remaining > Duration::from_secs(600) {
            return Ok(());
        }
        let Some(worker) = &self.record.worker else {
            return Ok(());
        };
        if self.worker.is_none() || !worker.active_namespace()? {
            return Ok(());
        }
        let renewal = lease.prepare_renewal(Duration::from_secs(3600), cancel)?;
        let mut record = self.record.clone();
        record.set_ready(&renewal)?;
        self.record.grant.resolve(self.record.grant.caller)?;
        self.store.save(&record, false)?;
        self.record = record;
        self.lease
            .as_mut()
            .ok_or_else(|| failure("capture unavailable"))?
            .commit_renewal(&renewal, cancel)
    }

    /// The export target must not contain, or be contained in, the original
    /// live HOME. Snapshot device numbers cannot establish this distinction.
    pub fn validate_output(&self, output: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let approved = self
            .record
            .grant
            .resolve(self.record.grant.caller)?
            .approved;
        let output_metadata = cap_primitives::fs::Metadata::from_file(output)?;
        if !output.metadata()?.is_dir() || approved.contains_identity(&output_metadata) {
            return Err(failure("capture output overlaps original home"));
        }
        let approved_metadata = approved.file.metadata()?;
        let mut current = output.try_clone()?;
        for _ in 0..128 {
            let metadata = current.metadata()?;
            if metadata.dev() == approved_metadata.dev()
                && metadata.ino() == approved_metadata.ino()
            {
                return Err(failure("capture output is inside original home"));
            }
            // This trusted broker examines ancestry of the held grant only;
            // it does not export a parent handle or its contents to an app.
            let parent = File::open(format!("/proc/self/fd/{}/..", current.as_raw_fd()))?;
            let parent_metadata = parent.metadata()?;
            if metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino() {
                return Ok(());
            }
            current = parent;
        }
        Err(failure("capture output ancestry exceeds bound"))
    }

    pub fn release(&mut self) -> io::Result<()> {
        if let Some(worker) = self.worker.as_mut() {
            worker.cancel_and_reap()?;
        }
        self.worker = None;
        self.record.state = State::Releasing;
        self.store.save(&self.record, false)?;
        if let Some(lease) = self.lease.take() {
            lease.release()?;
        }
        self.store.recover_intended(&self.record)?;
        self.record.state = State::Released;
        self.store.save(&self.record, false)
    }

    pub fn finish_failed_command(&mut self) -> io::Result<()> {
        if let Some(worker) = self.worker.as_mut() {
            worker.cancel_and_reap()?;
        }
        if let Some(custody) = &self.record.worker {
            custody.require_stopped()?;
        }
        self.worker = None;
        self.record.worker = None;
        self.store.save(&self.record, false)
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        // Preserve the lease if a worker cannot be proved stopped. Its protected
        // record remains recoverable; never delete beneath an un-reaped writer.
        if let Some(worker) = self.worker.as_mut()
            && worker.cancel_and_reap().is_err()
        {
            if let Some(lease) = self.lease.take() {
                let _ = lease.retain();
            }
        }
    }
}

fn unix_time(value: SystemTime) -> io::Result<u64> {
    value
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(io::Error::other)
}

pub use store::{
    CommittedOriginalApplicationsSelection, CommittedOriginalSelection,
    lookup_committed_original_applications_selection, lookup_committed_original_selection,
    review_committed_original_applications_selection, review_committed_original_selection,
    review_uncommitted_original_applications_selection, review_uncommitted_original_selection,
};

// Retained backing proof for an already admitted preparation directory.
pub use store::BackingWitness;

pub fn installed_copy_retirement_pending() -> bool {
    original_worker::retirement_pending()
}
