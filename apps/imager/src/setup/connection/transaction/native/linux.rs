//! Handle-relative regular-file primitives. No store receipts, cleanup or public
//! activation lives here. Relocation evidence is never sealed byte recovery.
mod lease_process;
use crate::setup::connection::endpoint::{self, EndpointError, NativeDirectory, PinnedEndpoint};
use crate::setup::connection::model::{ConnectionId, EndpointIdentity, NativeName, RelativePath};
use crate::setup::destination_security;
use lease_process::Receiver;
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;
use std::time::Instant;

pub(in crate::setup::connection::transaction) fn maybe_run_lease_receiver(
    args: &[String],
) -> Option<i32> {
    lease_process::maybe_run(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::connection::endpoint::{
        Access, EndpointContext, probe_endpoint, verify_selected_write,
    };
    fn endpoint(path: &std::path::Path) -> PinnedEndpoint {
        let probe = probe_endpoint(path).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        verify_selected_write(&endpoint).unwrap();
        endpoint
    }
    fn binding() -> OperationBinding {
        OperationBinding {
            connection: ConnectionId([1; 16]),
            operation: [2; 16],
            store_session: [3; 16],
            revision: 1,
            fence: 0,
        }
    }
    thread_local! { static BEFORE_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_before_relocate() {
        BEFORE_RELOCATE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    thread_local! { static BEFORE_PUBLICATION_REREAD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_before_publication_reread() {
        BEFORE_PUBLICATION_REREAD.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_final_publication_rejects_substituted_destination_ancestry() {
        let tree = fixture();
        // The source is deliberately outside the ancestry being substituted.
        std::fs::write(tree.path().join("source"), b"proposal").unwrap();
        std::fs::create_dir(tree.path().join("destination-parent")).unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        let target = epoch
            .pin_parent(
                Side::Target,
                &RelativePath::unix(b"destination-parent/file").unwrap(),
            )
            .unwrap();
        let absent = epoch.observe_absent(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let stage = epoch.stage_from_guard(&guard, &private).unwrap();
        let expected = stage.guard.actual.clone();
        let raced_root = tree.path().to_owned();
        BEFORE_PUBLICATION_REREAD.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(
                    raced_root.join("destination-parent"),
                    raced_root.join("saved-parent"),
                )
                .unwrap();
                std::fs::create_dir(raced_root.join("destination-parent")).unwrap();
                std::fs::write(raced_root.join("destination-parent/file"), b"raced").unwrap();
            }));
        });
        let result = epoch.publish_absent(stage, &target, absent, &guard);
        assert!(matches!(
            result,
            Err(Failure {
                cause: EndpointError::IdentityChanged,
                boundary: Boundary::Uncertain,
                ..
            })
        ));
        assert_eq!(epoch.boundary, Boundary::Uncertain);
        assert!(epoch.check().is_err());
        assert_eq!(
            std::fs::read(tree.path().join("source")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            std::fs::read(tree.path().join("saved-parent/file")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            object(&File::open(tree.path().join("saved-parent/file")).unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read(tree.path().join("destination-parent/file")).unwrap(),
            b"raced"
        );
    }
    thread_local! { static FAIL_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    pub(in crate::setup::connection::transaction) fn take_barrier_failure() -> bool {
        FAIL_BARRIER.with(|value| value.replace(false))
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_publication_is_actual_and_barrier_failure_preserves_every_name() {
        for fail in [false, true] {
            let tree = fixture();
            std::fs::write(tree.path().join("source"), b"proposal").unwrap();
            let endpoint = endpoint(tree.path());
            let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
            let source = epoch
                .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
                .unwrap();
            let guard = epoch.guard_regular(&source).unwrap();
            let dest = epoch
                .pin_parent(Side::Target, &RelativePath::unix(b"destination").unwrap())
                .unwrap();
            let absent = epoch.observe_absent(&dest).unwrap();
            let private = epoch.create_namespace(&dest).unwrap();
            let stage = epoch.stage_from_guard(&guard, &private).unwrap();
            let expected = stage.guard.actual.clone();
            FAIL_BARRIER.with(|value| value.set(fail));
            let published = epoch.publish_absent(stage, &dest, absent, &guard);
            if fail {
                assert!(matches!(
                    published,
                    Err(Failure {
                        boundary: Boundary::Uncertain,
                        ..
                    })
                ));
                assert!(epoch.check().is_err());
            } else {
                let published = published.unwrap();
                assert_eq!(published.identity, expected);
                assert_eq!(
                    published.bytes.digest,
                    <[u8; 32]>::from(Sha256::digest(b"proposal"))
                );
            }
            assert_eq!(
                std::fs::read(tree.path().join("source")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(tree.path().join("destination")).unwrap(),
                b"proposal"
            );
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_raced_writable_inode_is_preserved_as_actual_quarantine() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        std::fs::write(tree.path().join("racer"), b"new").unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(tree.path().join("racer"))
            .unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let target = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let expected_actual = object(&writer).unwrap();
        let path = tree.path().to_owned();
        BEFORE_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(path.join("live"), path.join("old")).unwrap();
                std::fs::rename(path.join("racer"), path.join("live")).unwrap();
            }));
        });
        let displaced = epoch.relocate_noreplace(&target, &private, &guard).unwrap();
        assert!(!displaced.matches_guard);
        assert_eq!(displaced.actual, expected_actual);
        writer.write_all_at(b"changed", 0).unwrap();
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private.name.to_str().unwrap())
                    .join("displaced")
            )
            .unwrap(),
            b"changed"
        );
        assert_eq!(std::fs::read(tree.path().join("old")).unwrap(), b"old");
        assert!(epoch.guard_displaced(&displaced).is_err());
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_source_disappearance_and_missing_parent_do_not_publish() {
        let tree = fixture();
        std::fs::write(tree.path().join("source"), b"source").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        std::fs::rename(tree.path().join("source"), tree.path().join("saved")).unwrap();
        assert!(epoch.capture(&guard).is_err());
        drop(epoch);
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        assert!(
            epoch
                .pin_parent(Side::Target, &RelativePath::unix(b"missing/child").unwrap())
                .is_err()
        );
        assert!(!tree.path().join("missing").exists());
        assert_eq!(std::fs::read(tree.path().join("saved")).unwrap(), b"source");
    }
    thread_local! { static AFTER_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_after_relocate() {
        AFTER_RELOCATE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_revocation_during_namespace_drain_keeps_accounted_quarantine() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let target = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let captured = tree
            .path()
            .join(private.name.to_str().unwrap())
            .join("displaced");
        let writer_path = captured.clone();
        AFTER_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                use std::os::unix::fs::OpenOptionsExt;
                assert!(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(writer_path)
                        .is_err()
                );
            }));
        });
        let displaced = epoch.relocate_noreplace(&target, &private, &guard).unwrap();
        assert!(displaced.matches_guard);
        assert_eq!(epoch.boundary, Boundary::DurableQuarantine);
        assert!(epoch.check().is_err());
        assert_eq!(std::fs::read(captured).unwrap(), b"old");
        assert!(!tree.path().join("live").exists());
    }
    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir_in(
            std::env::var_os("QOS_ENDPOINT_FIXTURE").expect("authorized disposable native volume"),
        )
        .unwrap()
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_exclusive_stage_and_absent_publication_preserve_raced_live() {
        let tree = fixture();
        std::fs::write(tree.path().join("source"), b"proposal").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        let dest = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"destination").unwrap())
            .unwrap();
        let absent = epoch.observe_absent(&dest).unwrap();
        let private = epoch.create_namespace(&dest).unwrap();
        let stage = epoch.stage_from_guard(&guard, &private).unwrap();
        assert_eq!(
            open_at(
                private.directory.file(),
                c"stage",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        std::fs::write(tree.path().join("destination"), b"raced").unwrap();
        assert!(epoch.publish_absent(stage, &dest, absent, &guard).is_err());
        assert_eq!(
            std::fs::read(tree.path().join("destination")).unwrap(),
            b"raced"
        );
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private.name.to_str().unwrap())
                    .join("stage")
            )
            .unwrap(),
            b"proposal"
        );
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_actual_displacement_keeps_unexpected_directory_and_old_bytes() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let parent = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&parent).unwrap();
        let private = epoch.create_namespace(&parent).unwrap();
        let raced_root = tree.path().to_owned();
        BEFORE_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(raced_root.join("live"), raced_root.join("original")).unwrap();
                std::fs::create_dir(raced_root.join("live")).unwrap();
                std::fs::write(raced_root.join("live/child"), b"raced tree").unwrap();
            }));
        });
        let moved = epoch.relocate_noreplace(&parent, &private, &guard).unwrap();
        assert!(!moved.matches_guard);
        assert_eq!(moved.actual.kind, libc::S_IFDIR);
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private.name.to_str().unwrap())
                    .join("displaced/child")
            )
            .unwrap(),
            b"raced tree"
        );
        assert_eq!(std::fs::read(tree.path().join("original")).unwrap(), b"old");
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_missing_source_parent_substitution_and_cancel_refuse_publication() {
        let tree = fixture();
        std::fs::create_dir(tree.path().join("parent")).unwrap();
        std::fs::write(tree.path().join("parent/source"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"parent/source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        std::fs::rename(tree.path().join("parent"), tree.path().join("moved")).unwrap();
        std::fs::create_dir(tree.path().join("parent")).unwrap();
        assert!(epoch.capture(&guard).is_err());
        epoch.cancel();
        assert!(epoch.check().is_err());
        assert_eq!(
            std::fs::read(tree.path().join("moved/source")).unwrap(),
            b"old"
        );
    }
}

/// Context is data, never mutation authority. All evidence below additionally
/// holds a fresh unpredictable epoch and actual retained native identities.
pub(in crate::setup::connection::transaction) struct OperationBinding {
    pub(in crate::setup::connection::transaction) connection: ConnectionId,
    pub(in crate::setup::connection::transaction) operation: [u8; 16],
    pub(in crate::setup::connection::transaction) store_session: [u8; 16],
    pub(in crate::setup::connection::transaction) revision: u64,
    pub(in crate::setup::connection::transaction) fence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup::connection::transaction) enum Boundary {
    Untouched,
    PrivateStaged,
    DurableQuarantine,
    Published,
    Uncertain,
}
#[derive(Debug)]
pub(in crate::setup::connection::transaction) struct Failure {
    cause: EndpointError,
    boundary: Boundary,
    os_error: Option<i32>,
}
impl Failure {
    pub(in crate::setup::connection::transaction) fn os_error(&self) -> Option<i32> {
        self.os_error
    }
    pub(in crate::setup::connection::transaction) fn cause(&self) -> EndpointError {
        self.cause
    }
    pub(in crate::setup::connection::transaction) fn boundary(&self) -> Boundary {
        self.boundary
    }
}
type Result<T> = std::result::Result<T, Failure>;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::setup::connection::transaction) enum Side {
    Source,
    Target,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectId {
    dev: u64,
    ino: u64,
    kind: u32,
    generation: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
}
impl Stamp {
    fn new(meta: &Metadata) -> Self {
        Self {
            length: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            links: meta.nlink(),
        }
    }
    fn relocated_eq(&self, other: &Self) -> bool {
        self.length == other.length
            && self.modified == other.modified
            && self.mode == other.mode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.links == other.links
    }
}
pub(in crate::setup::connection::transaction) struct ParentGuard {
    directory: NativeDirectory,
    name: CString,
    epoch: [u8; 16],
    side: Side,
}
pub(in crate::setup::connection::transaction) struct StableFileGuard {
    parent: ParentGuard,
    file: Arc<File>,
    actual: ObjectId,
    stamp: Stamp,
    executable: bool,
}
pub(in crate::setup::connection::transaction) struct PrivateNamespace {
    directory: NativeDirectory,
    name: CString,
    epoch: [u8; 16],
    identity: ObjectId,
    intended_parent: ObjectId,
    intended_name: CString,
}
pub(in crate::setup::connection::transaction) struct AbsentName {
    epoch: [u8; 16],
    parent: ObjectId,
    name: CString,
}
pub(in crate::setup::connection::transaction) struct Capture {
    digest: [u8; 32],
    length: u64,
    executable: bool,
}
struct SourceBinding {
    parent: NativeDirectory,
    name: CString,
    actual: ObjectId,
    side: Side,
}
pub(in crate::setup::connection::transaction) struct StagedFile {
    source: SourceBinding,
    guard: StableFileGuard,
    bytes: Capture,
    namespace: NativeDirectory,
    intended_parent: ObjectId,
    intended_name: CString,
}
pub(in crate::setup::connection::transaction) struct RelocatedObject {
    epoch: [u8; 16],
    actual: ObjectId,
    retained: File,
    namespace: NativeDirectory,
    original_parent: NativeDirectory,
    original_name: CString,
    matches_guard: bool,
    // Deliberately no BytesVerified/SealedDisplacement field or conversion.
}
pub(in crate::setup::connection::transaction) struct NativePublication {
    source: SourceBinding,
    epoch: [u8; 16],
    identity: ObjectId,
    bytes: Capture,
    parent: NativeDirectory,
    name: CString,
}
pub(in crate::setup::connection::transaction) struct NativeEpoch<'a> {
    source: &'a PinnedEndpoint,
    target: &'a PinnedEndpoint,
    source_root: NativeDirectory,
    target_root: NativeDirectory,
    identities: [EndpointIdentity; 2],
    binding: OperationBinding,
    nonce: [u8; 16],
    leases: Receiver,
    deadline: Instant,
    bytes_read: u64,
    boundary: Boundary,
    poisoned: bool,
    relocated: bool,
    namespace_created: bool,
}
fn random_id() -> io::Result<[u8; 16]> {
    let mut bytes = [0; 16];
    // SAFETY: correctly sized writable buffer; kernel entropy, no path fallback.
    let result = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if result != bytes.len() as isize || bytes == [0; 16] {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes)
}
fn object(file: &File) -> std::result::Result<ObjectId, EndpointError> {
    let metadata = file.metadata()?;
    Ok(ObjectId {
        dev: metadata.dev(),
        ino: metadata.ino(),
        kind: metadata.mode() & libc::S_IFMT,
        generation: Sha256::digest(endpoint::native_object_generation(file)?).into(),
    })
}
fn open_at(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
    // SAFETY: live parent, bounded NUL-terminated single component and fixed mode.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returns a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn rename_noreplace(from: &File, name: &CStr, to: &File, slot: &CStr) -> io::Result<()> {
    // SAFETY: retained same-mount directory fds and validated single components.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            name.as_ptr(),
            to.as_raw_fd(),
            slot.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn sync_directory(directory: &File) -> io::Result<()> {
    open_at(directory, c".", libc::O_RDONLY | libc::O_DIRECTORY)?.sync_all()
}
impl<'a> NativeEpoch<'a> {
    pub(in crate::setup::connection::transaction) fn begin(
        source: &'a PinnedEndpoint,
        target: &'a PinnedEndpoint,
        binding: OperationBinding,
    ) -> Result<Self> {
        let fail = |cause| Failure {
            cause,
            boundary: Boundary::Untouched,
            os_error: None,
        };
        let source_root = source.native_directory(false).map_err(fail)?;
        let target_root = target.native_directory(true).map_err(fail)?;
        let nonce = random_id().map_err(|e| fail(e.into()))?;
        let leases = Receiver::start(nonce).map_err(|e| {
            let os_error = e.raw_os_error();
            let cause = if e.kind() == io::ErrorKind::WouldBlock {
                EndpointError::Unverified
            } else {
                e.into()
            };
            Failure {
                cause,
                boundary: Boundary::Untouched,
                os_error,
            }
        })?;
        Ok(Self {
            source,
            target,
            source_root,
            target_root,
            identities: [source.identity(), target.identity()],
            binding,
            nonce,
            leases,
            deadline: Instant::now() + lease_process::WORK,
            bytes_read: 0,
            boundary: Boundary::Untouched,
            poisoned: false,
            relocated: false,
            namespace_created: false,
        })
    }
    fn failure(&mut self, cause: EndpointError) -> Failure {
        self.poisoned = true;
        self.leases.revoke();
        Failure {
            cause,
            boundary: self.boundary,
            os_error: None,
        }
    }
    fn ep<T>(&mut self, result: std::result::Result<T, EndpointError>) -> Result<T> {
        result.map_err(|e| self.failure(e))
    }
    fn io<T>(&mut self, result: io::Result<T>) -> Result<T> {
        result.map_err(|e| {
            let os_error = e.raw_os_error();
            let cause = if e.kind() == io::ErrorKind::AlreadyExists {
                EndpointError::Unstable
            } else if e.kind() == io::ErrorKind::WouldBlock {
                EndpointError::Unverified
            } else {
                e.into()
            };
            let mut failure = self.failure(cause);
            failure.os_error = os_error;
            failure
        })
    }
    pub(in crate::setup::connection::transaction) fn cancel(&mut self) {
        self.poisoned = true;
        self.leases.revoke();
    }
    pub(in crate::setup::connection::transaction) fn check(&mut self) -> Result<()> {
        if self.poisoned || Instant::now() >= self.deadline {
            return Err(self.failure(EndpointError::Cancelled));
        }
        let result = self
            .source
            .revalidate()
            .and_then(|()| self.target.revalidate_for_write());
        self.ep(result)?;
        let result = self.leases.check();
        self.io(result)
    }
    fn parent_check(&mut self, parent: &ParentGuard) -> Result<()> {
        if parent.epoch != self.nonce {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.ep(parent.directory.validate(parent.side == Side::Target))
    }
    fn namespace_check(&mut self, private: &PrivateNamespace) -> Result<()> {
        if private.epoch != self.nonce {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.ep(private.directory.validate(true))?;
        self.io(destination_security::check_private_file(
            private.directory.file(),
        ))?;
        if self.ep(object(private.directory.file()))? != private.identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        Ok(())
    }
    pub(in crate::setup::connection::transaction) fn pin_parent(
        &mut self,
        side: Side,
        path: &RelativePath,
    ) -> Result<ParentGuard> {
        self.check()?;
        let Some((last, ancestors)) = path.components().split_last() else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let mut directory = if side == Side::Source {
            self.source_root.clone()
        } else {
            self.target_root.clone()
        };
        for name in ancestors {
            let NativeName::Unix(bytes) = name else {
                return Err(self.failure(EndpointError::Unsupported));
            };
            directory = self.ep(directory.child(OsStr::from_bytes(bytes), side == Side::Target))?;
        }
        let NativeName::Unix(bytes) = last else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let name =
            CString::new(bytes.as_slice()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        self.check()?;
        Ok(ParentGuard {
            directory,
            name,
            side,
            epoch: self.nonce,
        })
    }
    pub(in crate::setup::connection::transaction) fn guard_regular(
        &mut self,
        parent: &ParentGuard,
    ) -> Result<StableFileGuard> {
        self.check()?;
        self.parent_check(parent)?;
        let before = self.io(open_at(parent.directory.file(), &parent.name, libc::O_PATH))?;
        if !self.io(before.metadata())?.is_file() {
            return Err(self.failure(EndpointError::Unsupported));
        }
        let identity = self.ep(object(&before))?;
        let file = self.io(open_at(
            parent.directory.file(),
            &parent.name,
            libc::O_RDONLY,
        ))?;
        self.ep(endpoint::native_same_mount(parent.directory.file(), &file))?;
        if self.ep(object(&file))? != identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let executable = self.ep(endpoint::native_regular_metadata(&file))?;
        let result = self.leases.acquire(file);
        let file = self.io(result)?;
        let stamp = Stamp::new(&self.io(file.metadata())?);
        let guard = StableFileGuard {
            parent: ParentGuard {
                directory: parent.directory.clone(),
                name: parent.name.clone(),
                epoch: self.nonce,
                side: parent.side,
            },
            file,
            actual: identity,
            stamp,
            executable,
        };
        self.validate_guard(&guard, true)?;
        Ok(guard)
    }
    fn validate_guard(&mut self, guard: &StableFileGuard, at_name: bool) -> Result<()> {
        self.check()?;
        self.parent_check(&guard.parent)?;
        if self.ep(object(&guard.file))? != guard.actual
            || Stamp::new(&self.io(guard.file.metadata())?) != guard.stamp
            || self.ep(endpoint::native_regular_metadata(&guard.file))? != guard.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        if at_name {
            let named = self.io(open_at(
                guard.parent.directory.file(),
                &guard.parent.name,
                libc::O_PATH,
            ))?;
            if self.ep(object(&named))? != guard.actual {
                return Err(self.failure(EndpointError::IdentityChanged));
            }
        }
        Ok(())
    }
    fn read_hash(&mut self, file: &File, mut output: Option<&mut File>) -> Result<Capture> {
        let mut hash = Sha256::new();
        let mut offset = 0_u64;
        let mut buffer = [0_u8; 16384];
        loop {
            self.check()?;
            let read = self.io(file.read_at(&mut buffer, offset))?;
            if read == 0 {
                break;
            }
            self.bytes_read = self
                .bytes_read
                .checked_add(read as u64)
                .ok_or_else(|| self.failure(EndpointError::LimitExceeded))?;
            if self.bytes_read > 1024 * 1024 * 1024 {
                return Err(self.failure(EndpointError::LimitExceeded));
            }
            let bytes = buffer
                .get(..read)
                .ok_or_else(|| self.failure(EndpointError::LimitExceeded))?;
            hash.update(bytes);
            if let Some(destination) = &mut output {
                self.io(destination.write_all(bytes))?;
            }
            offset = offset
                .checked_add(read as u64)
                .ok_or_else(|| self.failure(EndpointError::LimitExceeded))?;
        }
        Ok(Capture {
            digest: hash.finalize().into(),
            length: offset,
            executable: self.ep(endpoint::native_regular_metadata(file))?,
        })
    }
    pub(in crate::setup::connection::transaction) fn capture(
        &mut self,
        guard: &StableFileGuard,
    ) -> Result<Capture> {
        self.validate_guard(guard, true)?;
        let capture = self.read_hash(&guard.file, None)?;
        self.validate_guard(guard, true)?;
        if capture.length != guard.stamp.length {
            return Err(self.failure(EndpointError::Unstable));
        }
        Ok(capture)
    }
    fn barriers(&mut self, first: &NativeDirectory, second: &NativeDirectory) -> Result<()> {
        // Barrier failure is always uncertain, even when the prior syscall was
        // successful. Never downgrade it to the preceding durable boundary.
        self.boundary = Boundary::Uncertain;
        #[cfg(test)]
        if tests::take_barrier_failure() {
            return Err(self.failure(EndpointError::Offline));
        }
        self.io(sync_directory(first.file()))?;
        let a = self.io(first.file().metadata())?;
        let b = self.io(second.file().metadata())?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            self.io(sync_directory(second.file()))?;
        }
        Ok(())
    }
    pub(in crate::setup::connection::transaction) fn observe_absent(
        &mut self,
        parent: &ParentGuard,
    ) -> Result<AbsentName> {
        self.check()?;
        self.parent_check(parent)?;
        if parent.side != Side::Target || self.relocated {
            return Err(self.failure(EndpointError::Unsupported));
        }
        match open_at(parent.directory.file(), &parent.name, libc::O_PATH) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(self.failure(error.into())),
            Ok(_) => return Err(self.failure(EndpointError::Unstable)),
        }
        self.parent_check(parent)?;
        Ok(AbsentName {
            epoch: self.nonce,
            parent: self.ep(object(parent.directory.file()))?,
            name: parent.name.clone(),
        })
    }
    pub(in crate::setup::connection::transaction) fn create_namespace(
        &mut self,
        target: &ParentGuard,
    ) -> Result<PrivateNamespace> {
        self.check()?;
        self.parent_check(target)?;
        if target.side != Side::Target || self.namespace_created {
            return Err(self.failure(EndpointError::Unsupported));
        }
        let random = self.io(random_id())?;
        let mut label = String::from(".qos-operation-");
        use std::fmt::Write as _;
        for byte in random {
            write!(label, "{byte:02x}").map_err(|_| self.failure(EndpointError::Unsupported))?;
        }
        let name =
            CString::new(label.as_bytes()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        let root = self.target_root.clone();
        self.namespace_created = true;
        self.io(destination_security::create_directory(
            root.file(),
            OsStr::from_bytes(name.to_bytes()),
        ))?;
        let directory = self.ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
        self.io(destination_security::check_private_file(directory.file()))?;
        let identity = self.ep(object(directory.file()))?;
        let private = PrivateNamespace {
            directory,
            identity,
            name,
            epoch: self.nonce,
            intended_parent: self.ep(object(target.directory.file()))?,
            intended_name: target.name.clone(),
        };
        // Bounded native intent, not a store intent acknowledgement. Persist exact
        // destination native name and parent generation before any live mutation.
        let mut intent = self.io(open_at(
            private.directory.file(),
            c"intent",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        let parent_identity = self.ep(object(target.directory.file()))?;
        let mut descriptor_bytes = Vec::new();
        for bytes in [
            b"QNI1".as_slice(),
            &self.nonce,
            &self.binding.connection.0,
            &self.binding.operation,
            &self.binding.store_session,
            &self.binding.revision.to_le_bytes(),
            &self.binding.fence.to_le_bytes(),
            &self.identities[0].volume,
            &self.identities[0].root,
            &self.identities[1].volume,
            &self.identities[1].root,
            &private.identity.generation,
            &parent_identity.generation,
            target.name.to_bytes(),
        ] {
            descriptor_bytes.extend_from_slice(bytes);
        }
        self.io(intent.write_all(&descriptor_bytes))?;
        self.io(intent.sync_all())?;
        self.barriers(&private.directory, &root)?;
        self.boundary = Boundary::Untouched;
        self.namespace_check(&private)?;
        self.check()?;
        Ok(private)
    }
    pub(in crate::setup::connection::transaction) fn stage_from_guard(
        &mut self,
        source: &StableFileGuard,
        private: &PrivateNamespace,
    ) -> Result<StagedFile> {
        self.validate_guard(source, true)?;
        self.namespace_check(private)?;
        let mut file = self.io(open_at(
            private.directory.file(),
            c"stage",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        self.io(destination_security::check_private_file(&file))?;
        let bytes = self.read_hash(&source.file, Some(&mut file))?;
        self.validate_guard(source, true)?;
        // Only portable executable semantics; ownership/labels remain local.
        // SAFETY: exclusive generated stage fd; private host-local permissions.
        let result = unsafe {
            libc::fchmod(
                file.as_raw_fd(),
                if bytes.executable { 0o700 } else { 0o600 },
            )
        };
        if result != 0 {
            return Err(self.failure(io::Error::last_os_error().into()));
        }
        self.io(file.sync_all())?;
        drop(file); // read lease cannot coexist with our own writable description.
        let parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"stage".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
        };
        let guard = self.guard_regular(&parent)?;
        let verified = self.capture(&guard)?;
        if bytes.digest != verified.digest
            || bytes.length != verified.length
            || bytes.executable != verified.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let mut descriptor = self.io(open_at(
            private.directory.file(),
            c"staged-intent",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        let mut descriptor_bytes = Vec::new();
        for value in [
            b"QNS1".as_slice(),
            &self.nonce,
            &guard.actual.generation,
            &bytes.digest,
            &bytes.length.to_le_bytes(),
            &[u8::from(bytes.executable)],
        ] {
            descriptor_bytes.extend_from_slice(value);
        }
        self.io(descriptor.write_all(&descriptor_bytes))?;
        self.io(descriptor.sync_all())?;
        self.barriers(&private.directory, &private.directory)?;
        self.boundary = Boundary::PrivateStaged;
        self.namespace_check(private)?;
        self.validate_guard(source, true)?;
        Ok(StagedFile {
            source: SourceBinding {
                parent: source.parent.directory.clone(),
                name: source.parent.name.clone(),
                actual: source.actual.clone(),
                side: source.parent.side,
            },
            guard,
            bytes,
            namespace: private.directory.clone(),
            intended_parent: private.intended_parent.clone(),
            intended_name: private.intended_name.clone(),
        })
    }
    pub(in crate::setup::connection::transaction) fn relocate_noreplace(
        &mut self,
        target: &ParentGuard,
        private: &PrivateNamespace,
        expected: &StableFileGuard,
    ) -> Result<RelocatedObject> {
        self.check()?;
        self.parent_check(target)?;
        self.namespace_check(private)?;
        if target.side != Side::Target
            || private.intended_name != target.name
            || private.intended_parent != self.ep(object(target.directory.file()))?
            || expected.parent.epoch != self.nonce
            || expected.parent.name != target.name
            || self.ep(object(expected.parent.directory.file()))?
                != self.ep(object(target.directory.file()))?
            || self.relocated
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        // Check retained bytes, not the path occupant: rename can still capture a
        // raced replacement. The result below identifies that actual object.
        self.validate_guard(expected, false)?;
        #[cfg(test)]
        tests::run_before_relocate();
        self.boundary = Boundary::Uncertain;
        self.io(rename_noreplace(
            target.directory.file(),
            &target.name,
            private.directory.file(),
            c"displaced",
        ))?;
        self.relocated = true;
        self.boundary = Boundary::Uncertain;
        #[cfg(test)]
        tests::run_after_relocate();
        let retained = self.io(open_at(
            private.directory.file(),
            c"displaced",
            libc::O_PATH,
        ))?;
        self.ep(endpoint::native_same_mount(
            private.directory.file(),
            &retained,
        ))?;
        let actual = self.ep(object(&retained))?;
        let stamp = Stamp::new(&self.io(retained.metadata())?);
        let matches_guard = actual == expected.actual && stamp.relocated_eq(&expected.stamp);
        self.barriers(&target.directory, &private.directory)?;
        self.parent_check(target)?;
        self.namespace_check(private)?;
        let named = self.io(open_at(
            private.directory.file(),
            c"displaced",
            libc::O_PATH,
        ))?;
        if self.ep(object(&named))? != actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.boundary = Boundary::DurableQuarantine;
        // A break does not erase durable actual-object accounting. No success
        // receipt or publish authority follows from this preserved-name result.
        let result = self.leases.check();
        if result.is_err() {
            self.cancel();
        }
        Ok(RelocatedObject {
            epoch: self.nonce,
            actual,
            retained,
            namespace: private.directory.clone(),
            original_parent: target.directory.clone(),
            original_name: target.name.clone(),
            matches_guard,
        })
    }
    pub(in crate::setup::connection::transaction) fn guard_displaced(
        &mut self,
        displaced: &RelocatedObject,
    ) -> Result<StableFileGuard> {
        if displaced.epoch != self.nonce
            || !displaced.matches_guard
            || displaced.actual.kind != libc::S_IFREG
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let parent = ParentGuard {
            directory: displaced.namespace.clone(),
            name: c"displaced".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
        };
        let guard = self.guard_regular(&parent)?;
        if guard.actual != displaced.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        Ok(guard)
    }
    /// Only original additions are publishable in this slice. Relocation never
    /// manufactures the reviewed store recovery acknowledgement needed to replace.
    pub(in crate::setup::connection::transaction) fn publish_absent(
        &mut self,
        staged: StagedFile,
        target: &ParentGuard,
        absent: AbsentName,
        source: &StableFileGuard,
    ) -> Result<NativePublication> {
        self.check()?;
        self.parent_check(target)?;
        if self.relocated
            || staged.source.actual != source.actual
            || staged.source.name != source.parent.name
            || staged.source.side != source.parent.side
            || self.ep(object(staged.source.parent.file()))?
                != self.ep(object(source.parent.directory.file()))?
            || staged.intended_name != target.name
            || staged.intended_parent != absent.parent
            || target.side != Side::Target
            || absent.epoch != self.nonce
            || absent.name != target.name
            || absent.parent != self.ep(object(target.directory.file()))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        drop(absent); // consume the one-shot observed-absence evidence.
        self.validate_guard(source, true)?;
        self.validate_guard(&staged.guard, true)?;
        let fresh = self.capture(source)?;
        if fresh.digest != staged.bytes.digest
            || fresh.length != staged.bytes.length
            || fresh.executable != staged.bytes.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.boundary = Boundary::Uncertain;
        self.io(rename_noreplace(
            staged.namespace.file(),
            c"stage",
            target.directory.file(),
            &target.name,
        ))?;
        self.boundary = Boundary::Uncertain;
        self.barriers(&staged.namespace, &target.directory)?;
        self.parent_check(target)?;
        let actual = self.io(open_at(
            target.directory.file(),
            &target.name,
            libc::O_RDONLY,
        ))?;
        if self.ep(object(&actual))? != staged.guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let before = Stamp::new(&self.io(actual.metadata())?);
        if !before.relocated_eq(&staged.guard.stamp) {
            return Err(self.failure(EndpointError::Unstable));
        }
        #[cfg(test)]
        tests::run_before_publication_reread();
        let bytes = self.read_hash(&actual, None)?;
        if before != Stamp::new(&self.io(actual.metadata())?) {
            return Err(self.failure(EndpointError::Unstable));
        }
        if bytes.digest != staged.bytes.digest
            || bytes.length != staged.bytes.length
            || bytes.executable != staged.bytes.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.validate_guard(source, true)?;
        self.check()?;
        let named = self.io(open_at(target.directory.file(), &target.name, libc::O_PATH))?;
        if self.ep(object(&named))? != staged.guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        // The reread and source postflight can take time. A retained directory
        // fd still names a detached old parent, so recheck its ancestry/security
        // at the final proof boundary rather than trusting the earlier check.
        self.parent_check(target)?;
        self.boundary = Boundary::Published;
        Ok(NativePublication {
            source: staged.source,
            epoch: self.nonce,
            identity: staged.guard.actual,
            bytes,
            parent: target.directory.clone(),
            name: target.name.clone(),
        })
    }
}
