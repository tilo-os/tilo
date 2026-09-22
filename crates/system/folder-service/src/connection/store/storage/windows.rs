//! Windows store adaptation. Public admission stays refused until a reviewed
//! live authority provider and native persistence qualification both exist.
use super::super::super::windows_native as native;
use super::{
    Arc, Current, Directory, File, GcIntent, Path, Read, Storage, StoreError, Write,
    destination_security, file_digest, format, parse_hash, sync_directory, sync_file,
};
#[cfg(test)]
use super::{Header, OsStr, PathBuf, attest, child};
use native::Role;

pub(in super::super) struct Metadata {
    metadata: cap_primitives::fs::Metadata,
    id: format::WindowsFileId,
}
impl std::ops::Deref for Metadata {
    type Target = cap_primitives::fs::Metadata;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}
pub(in super::super) fn regular(file: &File) -> Result<Metadata, StoreError> {
    destination_security::require_private_protected(file)?;
    super::super::super::endpoint::connected_metadata(file, false)
        .map_err(|_| StoreError::IdentityChanged)?;
    Ok(Metadata {
        metadata: cap_primitives::fs::Metadata::from_file(file)?,
        id: native::identity(file)?,
    })
}
pub(in super::super) fn file_identity(metadata: &Metadata) -> format::FileIdentity {
    format::FileIdentity::Windows(metadata.id)
}
pub(in super::super) fn same_snapshot(a: &Metadata, b: &Metadata) -> bool {
    a.id == b.id && super::super::super::super::migration::same_snapshot(&a.metadata, &b.metadata)
}
pub(in super::super) fn open_file(
    parent: &Arc<Directory>,
    name: &str,
    create: bool,
) -> Result<File, StoreError> {
    let role = if name == "ownership.lock" {
        Role::Lock
    } else {
        Role::Writer
    };
    native::open_file(parent, name, role, create).map_err(|error| {
        if name == "ownership.lock" && error.raw_os_error() == Some(32) {
            StoreError::Busy
        } else {
            error.into()
        }
    })
}
pub(in super::super) fn read_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    Ok(native::open_file(parent, name, Role::StableRead, false)?)
}
pub(super) fn reread_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    Ok(native::open_file(parent, name, Role::Reread, false)?)
}
pub(super) fn mutation_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    Ok(native::open_file(parent, name, Role::RenameWriter, false)?)
}
pub(in super::super) fn validate_file(
    parent: &Arc<Directory>,
    name: &str,
    file: &File,
    expected: &Metadata,
) -> Result<(), StoreError> {
    native::validate_named(parent, name, file, expected.id)?;
    Ok(())
}
/// A verified reader's clone retains the same real guard. No raw/Deref/into-File
/// conversion is exposed: a permissive inspection handle cannot escape alone.
pub(in super::super) struct RecoveryReader {
    file: File,
    writer: Option<Arc<File>>,
}
impl RecoveryReader {
    pub(in super::super) fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            writer: self.writer.clone(),
        })
    }
}
impl Read for RecoveryReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buffer)
    }
}
impl std::io::Seek for RecoveryReader {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}
pub(in super::super) struct RecoveryWriterGuard {
    identity: format::FileIdentity,
    writer: std::sync::Weak<File>,
}
impl RecoveryWriterGuard {
    pub(in super::super) fn is_live(&self) -> bool {
        self.writer.strong_count() != 0
    }
}
pub(in super::super) fn preserved_reader(
    writer: File,
    reader: File,
) -> Result<(RecoveryReader, RecoveryWriterGuard), StoreError> {
    let identity = file_identity(&regular(&writer)?);
    if file_identity(&regular(&reader)?) != identity {
        return Err(StoreError::IdentityChanged);
    }
    let writer = Arc::new(writer);
    let guard = RecoveryWriterGuard {
        identity,
        writer: Arc::downgrade(&writer),
    };
    Ok((
        RecoveryReader {
            file: reader,
            writer: Some(writer),
        },
        guard,
    ))
}
pub(in super::super) fn guarded_reader(
    parent: &Arc<Directory>,
    name: &str,
    expected: format::FileIdentity,
    live: Option<&RecoveryWriterGuard>,
) -> Result<RecoveryReader, StoreError> {
    let writer = match live {
        Some(guard) => {
            if guard.identity != expected {
                return Err(StoreError::IdentityChanged);
            }
            guard.writer.upgrade()
        }
        None => None,
    };
    // Upgrade first: the original receipt may be dropped at any point after
    // this, but the actual deny-write/delete guard remains held before reopen.
    let role = if let Some(writer) = &writer {
        let metadata = regular(writer)?;
        if file_identity(&metadata) != expected {
            return Err(StoreError::IdentityChanged);
        }
        validate_file(parent, name, writer, &metadata)?;
        Role::Reread
    } else {
        Role::StableRead
    };
    let file = native::open_file(parent, name, role, false)?;
    Ok(RecoveryReader { file, writer })
}
pub(in super::super) fn recovery_metadata(reader: &RecoveryReader) -> Result<Metadata, StoreError> {
    regular(&reader.file)
}
pub(in super::super) fn validate_recovery_reader(
    parent: &Arc<Directory>,
    name: &str,
    reader: &RecoveryReader,
    expected: &Metadata,
) -> Result<(), StoreError> {
    validate_file(parent, name, &reader.file, expected)
}

impl Storage {
    pub fn open(_path: &Path) -> Result<(Self, Current), StoreError> {
        // No mkdir, device open, token change or caller/serialized privilege.
        Err(StoreError::UnsupportedDurability)
    }
    #[cfg(test)]
    fn open_native_fixture(path: &Path) -> Result<(Self, Current), StoreError> {
        let mut ancestor = path.to_path_buf();
        let mut missing = Vec::new();
        while !ancestor.try_exists()? {
            if missing.len() >= 64 {
                return Err(StoreError::LimitExceeded);
            }
            missing.push(ancestor.file_name().ok_or(StoreError::Corrupt)?.to_owned());
            ancestor = ancestor.parent().ok_or(StoreError::Corrupt)?.to_path_buf();
        }
        let fresh = !missing.is_empty();
        // The native fixture explicitly authenticates the real retained volume.
        // It does not create a production authority or publication receipt.
        let mut root = native::fixture_anchor(&ancestor)?;
        for name in missing.into_iter().rev() {
            let next = root.create_child(&name)?;
            sync_directory(&root)?;
            sync_directory(&next)?;
            root = next;
        }
        destination_security::require_private_protected(&root.file)?;
        let identity = attest(path)?;
        let lock = open_file(&root, "ownership.lock", fresh)?;
        let lock_identity = regular(&lock)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => StoreError::Busy,
            std::fs::TryLockError::Error(error) => StoreError::Persistence(error),
        })?;
        validate_file(&root, "ownership.lock", &lock, &lock_identity)?;
        if fresh {
            sync_file(&lock)?;
            sync_directory(&root)?;
        }
        let pages = child(&root, "pages", fresh)?;
        let recovery = child(&root, "recovery", fresh)?;
        let quarantine = child(&root, "quarantine", fresh)?;
        let storage = Self {
            read_budget: Arc::new(std::sync::Mutex::new(None)),
            root,
            pages,
            recovery,
            quarantine,
            path: path.to_owned(),
            identity,
            lock,
            lock_identity,
        };
        storage.validate()?;
        let current = if fresh {
            storage.initialize()?
        } else {
            let header: Header =
                serde_json::from_slice(&storage.read_bytes(&storage.root, "store-header", 4096)?)
                    .map_err(|_| StoreError::Corrupt)?;
            if header.schema != 1 || header.identity != storage.identity {
                return Err(StoreError::IdentityChanged);
            }
            let current: Current =
                serde_json::from_slice(&storage.read_bytes(&storage.root, "CURRENT", 4096)?)
                    .map_err(|_| StoreError::Corrupt)?;
            if current.schema != 1 || current.identity != storage.identity {
                return Err(StoreError::IdentityChanged);
            }
            current
        };
        Ok((storage, current))
    }
}
impl super::super::ConnectionStore {
    pub(in super::super) fn publish_current_with_context(
        &self,
        next: &Current,
        next_log: &File,
        next_log_identity: &Metadata,
        context: Option<&super::super::NativeReadContext<'_>>,
    ) -> Result<Metadata, StoreError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        self.validate_with_context(context)?;
        let serial = SERIAL
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| StoreError::LimitExceeded)?;
        // Explicit crash lifetime: retain a failed preparation for diagnosis.
        // Exclusive create refuses collisions; no destructor performs deletion.
        let name = format!("CURRENT-prepared-{}-{serial}", std::process::id());
        let mut pointer = native::open_file(&self.storage.root, &name, Role::RenameWriter, true)?;
        pointer.write_all(&format::encode(next)?)?;
        let identity = regular(&pointer)?;
        sync_file(&pointer)?;
        #[cfg(test)]
        {
            super::super::tests::fault()?;
            super::super::tests::checkpoint_boundary(
                super::super::tests::CheckpointBoundary::Prepared,
            )?;
        }
        validate_file(&self.storage.root, &name, &pointer, &identity)?;
        if !same_snapshot(&identity, &regular(&pointer)?) {
            return Err(StoreError::IdentityChanged);
        }
        self.validate_next_log(next, next_log, next_log_identity)?;
        // Both retained old CURRENT and old log are checked after all blocking
        // preparation. validate releases its CURRENT inspection before rename.
        self.validate_with_context(context)?;
        native::replace_current(
            &pointer,
            CurrentReplacement {
                root: &self.storage.root,
            },
        )?;
        #[cfg(test)]
        super::super::tests::fault()?;
        sync_directory(&self.storage.root)?;
        #[cfg(test)]
        super::super::tests::checkpoint_boundary(
            super::super::tests::CheckpointBoundary::Published,
        )?;
        // Independent reread permits the retained DELETE-capable writer guard.
        let mut reread = reread_file(&self.storage.root, "CURRENT")?;
        let bytes = super::read_metered(&mut reread, 4096, &|n| {
            self.storage.review_charge(n as u64, 0, 0, 0)?;
            if let Some(context) = context {
                context.charge(n as u64)?;
            }
            Ok(())
        })?;
        validate_file(&self.storage.root, "CURRENT", &reread, &identity)?;
        if bytes != format::encode(next)? {
            return Err(StoreError::IdentityChanged);
        }
        self.storage.validate()?;
        self.validate_next_log(next, next_log, next_log_identity)?;
        self.validate_log()?;
        Ok(identity)
    }
}
pub(super) fn capture(
    source: &File,
    parent: &Arc<Directory>,
    name: &str,
    quarantine: &Arc<Directory>,
) -> Result<(), StoreError> {
    let expected = regular(source)?;
    validate_file(parent, name, source, &expected)?;
    native::rename_noreplace(source, quarantine, "captured")?;
    sync_directory(parent)?;
    sync_directory(quarantine)?;
    validate_file(quarantine, "captured", source, &expected)?;
    require_absent(parent, name)
}
pub(super) fn require_absent(parent: &Arc<Directory>, name: &str) -> Result<(), StoreError> {
    match native::open_file(parent, name, Role::Named, false) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(StoreError::RecoveryRequired),
    }
}
// These capabilities have no public constructor. Only the locked-store paths
// above/below create them after validating the committed dependencies/intent.
pub(in crate::connection) struct CurrentReplacement<'a> {
    root: &'a Arc<Directory>,
}
impl CurrentReplacement<'_> {
    pub(in crate::connection) fn root(&self) -> &Directory {
        self.root
    }
}
pub(in crate::connection) struct GcDeletion<'a> {
    parent: &'a Arc<Directory>,
    name: &'static str,
    id: format::WindowsFileId,
}
impl GcDeletion<'_> {
    pub(in crate::connection) fn parts(&self) -> (&Directory, &str, format::WindowsFileId) {
        (self.parent, self.name, self.id)
    }
}
pub(super) fn delete(
    mut file: File,
    store: &super::super::ConnectionStore,
    name: &'static str,
    metadata: &Metadata,
    intent: &GcIntent,
    context: Option<&super::super::NativeReadContext<'_>>,
) -> Result<(), StoreError> {
    store.validate_with_context(context)?;
    let meter = |requested: usize| {
        store.storage.review_charge(requested as u64, 0, 0, 1)?;
        if let Some(context) = context {
            context.charge(requested as u64)?;
        }
        Ok(())
    };
    let parent = &store.storage.quarantine;
    validate_file(parent, name, &file, metadata)?;
    if intent.schema != 1
        || intent.identity != store.storage.identity
        || intent.generation > store.current.generation
        || intent.sequence > store.current.sequence
    {
        return Err(StoreError::RecoveryRequired);
    }
    let mut durable_intent = reread_file(parent, "intent")?;
    let bytes = format::encode(intent)?;
    let actual = file_digest(&mut durable_intent, 4096, &meter)?;
    meter(0)?;
    if actual != (format::digest(&bytes), bytes.len() as u64) {
        return Err(StoreError::RecoveryRequired);
    }
    drop(durable_intent);
    if intent.page {
        let hash = parse_hash(&intent.name).ok_or(StoreError::Corrupt)?;
        if hash != intent.digest || store.page_referenced_with_context(hash, context)? {
            return Err(StoreError::RecoveryRequired);
        }
    } else {
        let generation = intent
            .name
            .strip_prefix("log-")
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or(StoreError::Corrupt)?;
        if generation >= store.current.log || intent.name != format!("log-{generation}") {
            return Err(StoreError::RecoveryRequired);
        }
    }
    let (expected_digest, expected_length) = match name {
        "captured" if file_identity(metadata) == intent.file_id => (intent.digest, intent.length),
        "intent" => {
            let bytes = format::encode(intent)?;
            (format::digest(&bytes), bytes.len() as u64)
        }
        _ => return Err(StoreError::RecoveryRequired),
    };
    let actual = file_digest(&mut file, expected_length, &meter)?;
    meter(0)?;
    if actual != (expected_digest, expected_length) {
        return Err(StoreError::RecoveryRequired);
    }
    native::delete_captured(
        file,
        GcDeletion {
            parent,
            name,
            id: metadata.id,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        ConnectionId, ConnectionPolicy, ConnectionStore, EndpointBinding, EndpointIdentity,
        EndpointLocator, Lifecycle,
    };
    use super::*;
    use std::io::{Seek, SeekFrom};
    fn open(path: &Path) -> Result<ConnectionStore, StoreError> {
        let (storage, current) = Storage::open_native_fixture(path)?;
        ConnectionStore::from_storage(storage, current)
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, ConnectionStore) {
        let selected = std::env::var_os("QOS_CONNECTED_WINDOWS_FIXTURE")
            .expect("explicit disposable NTFS fixture path required");
        let temp = tempfile::tempdir_in(selected).unwrap();
        let path = temp.path().canonicalize().unwrap().join("connections");
        assert!(matches!(
            ConnectionStore::open_at(&path),
            Err(StoreError::UnsupportedDurability)
        ));
        assert!(!path.exists());
        let store = open(&path).unwrap();
        (temp, path, store)
    }
    fn fence(store: &mut ConnectionStore) -> ConnectionId {
        let id = ConnectionId([9; 16]);
        let binding = EndpointBinding {
            identity: EndpointIdentity {
                volume: [1; 32],
                root: [2; 32],
            },
            locator: EndpointLocator::new(&store.storage.path).unwrap(),
        };
        let mut destination = binding.clone();
        destination.identity.root = [3; 32];
        destination.locator =
            EndpointLocator::new(&store.storage.path.join("destination")).unwrap();
        let record = store
            .create_connection(
                id,
                binding,
                destination,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value;
        store.request_disconnect(id, record.revision).unwrap();
        id
    }
    #[test]
    #[ignore = "UNRUN: explicitly authorized administrative disposable Windows NTFS fixture required"]
    fn native_windows_permanent_lock_process_and_reopen() {
        let (_temp, path, store) = fixture();
        let id = native::identity(&store.storage.lock).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "connection::store::storage::windows::tests::native_windows_lock_child",
                "--nocapture",
            ])
            .env("QOS_CONNECTED_WINDOWS_LOCK_CHILD_PATH", &path)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(std::fs::rename(path.join("ownership.lock"), path.join("renamed-lock")).is_err());
        drop(store);
        let reopened = open(&path).unwrap();
        assert_eq!(id, native::identity(&reopened.storage.lock).unwrap());
    }
    #[test]
    #[ignore = "UNRUN: child entrypoint of authorized two-process permanent-lock fixture only"]
    fn native_windows_lock_child() {
        let path = std::env::var_os("QOS_CONNECTED_WINDOWS_LOCK_CHILD_PATH").unwrap();
        assert!(matches!(open(Path::new(&path)), Err(StoreError::Busy)));
    }
    #[test]
    #[ignore = "UNRUN: real Windows writer, independent reread and editor sharing APIs"]
    fn native_windows_recovery_reread_and_editor_roles() {
        let (_temp, path, store) = fixture();
        let mut writer =
            native::open_file(&store.storage.recovery, "bytes", Role::RenameWriter, true).unwrap();
        writer.write_all(b"preserved independently").unwrap();
        sync_file(&writer).unwrap();
        let id = regular(&writer).unwrap();
        let mut reader = reread_file(&store.storage.recovery, "bytes").unwrap();
        assert_eq!(
            file_identity(&regular(&reader).unwrap()),
            file_identity(&id)
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"preserved independently");
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(path.join("recovery/bytes"))
                .is_err()
        );
        validate_file(&store.storage.recovery, "bytes", &writer, &id).unwrap();
        drop(reader);
        sync_directory(&store.storage.recovery).unwrap();
    }
    #[test]
    #[ignore = "UNRUN: Windows full-ID CURRENT rename and reopen with durable fence"]
    fn native_windows_current_switch_and_fence_reopen() {
        let (_temp, path, mut store) = fixture();
        let id = fence(&mut store);
        let old = file_identity(&store.current_identity);
        store.checkpoint().unwrap();
        assert_ne!(old, file_identity(&store.current_identity));
        drop(store);
        let reopened = open(&path).unwrap();
        assert_eq!(
            reopened.load_connection(id).unwrap().lifecycle,
            Lifecycle::Disconnecting
        );
    }
    #[test]
    #[ignore = "UNRUN: real Windows no-clobber quarantine capture and handle disposition"]
    fn native_windows_quarantine_collision_and_disposition() {
        let (_temp, path, store) = fixture();
        store
            .storage
            .create_bytes(&store.storage.pages, "candidate", b"old page")
            .unwrap();
        store
            .storage
            .create_bytes(&store.storage.quarantine, "captured", b"keep occupant")
            .unwrap();
        let source = mutation_file(&store.storage.pages, "candidate").unwrap();
        assert!(
            capture(
                &source,
                &store.storage.pages,
                "candidate",
                &store.storage.quarantine
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(path.join("quarantine/captured")).unwrap(),
            b"keep occupant"
        );
        assert_eq!(
            std::fs::read(path.join("pages/candidate")).unwrap(),
            b"old page"
        );
        drop(source);
        // A separate exact committed intent exercises disposition and final
        // absence; the colliding occupant above is left untouched.
        let bytes = b"retired bytes";
        let candidate = format::hex(&format::digest(bytes));
        store
            .storage
            .create_bytes(&store.storage.pages, &candidate, bytes)
            .unwrap();
        let source = mutation_file(&store.storage.pages, &candidate).unwrap();
        let metadata = regular(&source).unwrap();
        let child = store
            .storage
            .quarantine
            .create_child(OsStr::new("collision-retained"))
            .unwrap();
        let occupant = mutation_file(&store.storage.quarantine, "captured").unwrap();
        native::rename_noreplace(&occupant, &child, "captured").unwrap();
        sync_directory(&store.storage.quarantine).unwrap();
        sync_directory(&child).unwrap();
        drop(occupant);
        let intent = GcIntent {
            schema: 1,
            identity: store.storage.identity.clone(),
            generation: store.current.generation,
            sequence: store.current.sequence,
            page: true,
            name: candidate.clone(),
            file_id: file_identity(&metadata),
            length: bytes.len() as u64,
            digest: format::digest(bytes),
        };
        store
            .storage
            .create_bytes(
                &store.storage.quarantine,
                "intent",
                &format::encode(&intent).unwrap(),
            )
            .unwrap();
        capture(
            &source,
            &store.storage.pages,
            &candidate,
            &store.storage.quarantine,
        )
        .unwrap();
        drop(source);
        let captured = mutation_file(&store.storage.quarantine, "captured").unwrap();
        delete(captured, &store, "captured", &metadata, &intent, None).unwrap();
        assert!(!path.join("quarantine/captured").exists());
        assert!(path.join("quarantine/intent").exists());
        let intent_file = mutation_file(&store.storage.quarantine, "intent").unwrap();
        let identity = regular(&intent_file).unwrap();
        delete(intent_file, &store, "intent", &identity, &intent, None).unwrap();
        assert!(!path.join("quarantine/intent").exists());
    }
    #[test]
    #[ignore = "UNRUN: actual short physical tail repair and complete-corruption refusal on Windows"]
    fn native_windows_tail_repair_preserves_fence() {
        let (_temp, path, mut store) = fixture();
        let id = fence(&mut store);
        let log = path.join(format!("log-{}", store.current.log));
        let length = store.log.metadata().unwrap().len();
        drop(store);
        let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(&[0, 0]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let reopened = open(&path).unwrap();
        assert_eq!(reopened.log.metadata().unwrap().len(), length);
        assert_eq!(
            reopened.load_connection(id).unwrap().lifecycle,
            Lifecycle::Disconnecting
        );
        drop(reopened);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&log)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"badmagic").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let corrupted = std::fs::read(&log).unwrap();
        assert!(open(&path).is_err());
        assert_eq!(std::fs::read(&log).unwrap(), corrupted);
    }
    #[test]
    #[ignore = "UNRUN: separately provisioned nearly-full disposable NTFS fixture; bounded 64 MiB maximum"]
    fn native_windows_disk_full_retains_pending_bytes() {
        let selected = std::env::var_os("QOS_CONNECTED_WINDOWS_DISK_FULL_FIXTURE")
            .expect("separately authorized nearly-full disposable filesystem");
        let temp = tempfile::tempdir_in(selected).unwrap();
        let path = temp.path().canonicalize().unwrap().join("connections");
        let store = open(&path).unwrap();
        store
            .storage
            .create_bytes(&store.storage.quarantine, "pending", b"must retain")
            .unwrap();
        let mut file = open_file(&store.storage.recovery, "space-test", true).unwrap();
        let bytes = vec![0x5a; 1024 * 1024];
        let mut failure = None;
        for _ in 0..64 {
            if let Err(error) = file
                .write_all(&bytes)
                .and_then(|()| native::flush_file(&file))
            {
                failure = Some(error);
                break;
            }
        }
        assert!(
            matches!(failure.and_then(|e| e.raw_os_error()), Some(39 | 112)),
            "fixture must reach actual disk-full within the bound"
        );
        assert_eq!(
            std::fs::read(path.join("quarantine/pending")).unwrap(),
            b"must retain"
        );
    }
    #[test]
    #[ignore = "UNRUN: failures injected only after real Windows flush/rename boundaries; persistent fence survives"]
    fn native_windows_checkpoint_failure_boundaries() {
        let mut failures = 0;
        for point in 0..12 {
            let (_temp, path, mut store) = fixture();
            let id = fence(&mut store);
            super::super::super::tests::native_fault_after(Some(point));
            let result = store.checkpoint();
            super::super::super::tests::native_fault_after(None);
            if result.is_err() {
                failures += 1;
                assert!(store.poisoned);
            }
            drop(store);
            let reopened = open(&path).unwrap();
            assert_eq!(
                reopened.load_connection(id).unwrap().lifecycle,
                Lifecycle::Disconnecting
            );
        }
        assert!(failures >= 6);
    }
    #[test]
    #[ignore = "UNRUN: CURRENT substitution after real blocking preparation must not be overwritten"]
    fn native_windows_prepared_checkpoint_revalidates_current() {
        let (_temp, path, mut store) = fixture();
        fence(&mut store);
        let current = path.join("CURRENT");
        let changed = current.clone();
        super::super::super::tests::native_checkpoint_hook(
            super::super::super::tests::CheckpointBoundary::Prepared,
            Box::new(move || {
                std::fs::write(changed, b"foreign pointer")?;
                Ok(())
            }),
        );
        assert!(store.checkpoint().is_err());
        assert_eq!(std::fs::read(current).unwrap(), b"foreign pointer");
        assert!(store.poisoned);
    }
    fn compatible_editor(path: &Path) -> std::io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(7)
            .open(path)
    }
    fn assert_mutations_blocked(path: &Path) {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::DELETE;
        assert!(compatible_editor(path).is_err());
        assert!(
            std::fs::OpenOptions::new()
                .access_mode(DELETE)
                .share_mode(7)
                .open(path)
                .is_err()
        );
        assert!(std::fs::remove_file(path).is_err());
        assert!(std::fs::rename(path, path.with_extension("must-not-rename")).is_err());
        assert!(path.exists());
    }
    #[test]
    #[ignore = "UNRUN: actual Windows recovery reservation, independent reread, retained guard and replay"]
    fn native_windows_recovery_ack_retains_verified_bytes() {
        use super::super::super::{
            Decision, EntryState, OperationIntent, RecoveryReason, RelativePath, Side,
        };
        use std::sync::atomic::AtomicBool;
        let (_temp, path, mut store) = fixture();
        let binding = EndpointBinding {
            identity: EndpointIdentity {
                volume: [1; 32],
                root: [2; 32],
            },
            locator: EndpointLocator::new(&path).unwrap(),
        };
        let mut destination = binding.clone();
        destination.identity.root = [3; 32];
        destination.locator =
            EndpointLocator::new(&store.storage.path.join("destination")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([8; 16]),
                binding,
                destination,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value;
        let content = b"native recovery bytes";
        let relative = RelativePath::windows(&"file".encode_utf16().collect::<Vec<_>>()).unwrap();
        let intent = store
            .begin_intent(OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: None,
                path: relative.clone(),
                source_before: Some(EntryState::File {
                    sha256: format::digest(content),
                    bytes: content.len() as u64,
                    executable: false,
                }),
                destination_before: None,
                decision: Decision::Propagate { from: Side::Source },
            })
            .unwrap();
        let reservation = store
            .reserve_recovery(
                &intent,
                Side::Source,
                relative,
                RecoveryReason::DisplacedVersion,
            )
            .unwrap();
        let source_path = path.parent().unwrap().join("source");
        std::fs::write(&source_path, content).unwrap();
        let mut source = File::open(source_path).unwrap();
        let mut preserved = store
            .preserve_file(reservation, &mut source, &AtomicBool::new(false))
            .unwrap();
        let id = preserved.value.id;
        let object_path = path
            .join("recovery")
            .join(format::hex(&id.operation.connection.0))
            .join(format!("{}-{}", id.operation.serial, id.slot));
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&object_path)
                .is_err()
        );
        let mut actual = Vec::new();
        preserved.value.file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, content);
        let mut key = format::key(2, id.operation.connection.0, id.operation.serial);
        key.extend(id.slot.to_be_bytes());
        let Some(format::Row::Recovery(entry)) = store.row(&key).unwrap() else {
            panic!("recovery row missing")
        };
        // Verify while preservation is still live: a new stable open would
        // self-conflict, so verification must retain its actual writer guard.
        let mut live_reader = store.verify_recovery(&entry).unwrap();
        let preserved_clone = preserved.value.file.try_clone().unwrap();
        drop(preserved);
        let live_clone = live_reader.try_clone().unwrap();
        let mut live_bytes = Vec::new();
        live_reader.read_to_end(&mut live_bytes).unwrap();
        assert_eq!(live_bytes, content);
        drop(live_reader);
        assert_mutations_blocked(&object_path);
        drop(live_clone);
        assert_mutations_blocked(&object_path); // the preserved-reader clone still guards
        drop(preserved_clone);
        compatible_editor(&object_path).unwrap(); // no ACL-only false positive
        drop(store);
        assert_reopened_recovery(&path, id, content);
    }
    fn assert_reopened_recovery(path: &Path, id: super::super::super::RecoveryId, content: &[u8]) {
        let object_path = path
            .join("recovery")
            .join(format::hex(&id.operation.connection.0))
            .join(format!("{}-{}", id.operation.serial, id.slot));
        let mut key = format::key(2, id.operation.connection.0, id.operation.serial);
        key.extend(id.slot.to_be_bytes());
        let reopened = open(path).unwrap();
        let Some(format::Row::Recovery(entry)) = reopened.row(&key).unwrap() else {
            panic!("recovery row missing")
        };
        assert!(matches!(
            entry.object.as_ref().unwrap().file_id,
            format::FileIdentity::Windows(_)
        ));
        let editor = compatible_editor(&object_path).unwrap();
        assert!(
            reopened.verify_recovery(&entry).is_err(),
            "stable admission must reject an already-open writer"
        );
        drop(editor);
        let mut verified = reopened.verify_recovery(&entry).unwrap();
        assert_mutations_blocked(&object_path);
        let verified_clone = verified.try_clone().unwrap();
        let mut bytes = Vec::new();
        verified.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, content);
        let metadata = recovery_metadata(&verified).unwrap();
        assert_eq!(
            file_identity(&metadata),
            entry.object.as_ref().unwrap().file_id
        );
        let parent_name = format::hex(&id.operation.connection.0);
        let named = super::super::metadata_nofollow(
            &reopened.storage.recovery.file,
            Path::new(&parent_name),
        )
        .unwrap();
        let parent = reopened
            .storage
            .recovery
            .open_child(OsStr::new(&parent_name), &named)
            .unwrap();
        validate_recovery_reader(
            &parent,
            &format!("{}-{}", id.operation.serial, id.slot),
            &verified,
            &metadata,
        )
        .unwrap();
        drop(verified);
        assert_mutations_blocked(&object_path);
        // The same lifetime collection used across live append preparation and
        // persistence retains actual readers, not past digest observations.
        let mut checks = super::super::super::RecoveryChecks::default();
        checks.verify(&reopened, &entry).unwrap();
        drop(verified_clone);
        assert_mutations_blocked(&object_path);
        drop(checks);
        let mut editor = compatible_editor(&object_path).unwrap();
        editor.write_all(content).unwrap();
        editor.sync_all().unwrap();
        drop(editor);
        let renamed = object_path.with_extension("rename-control");
        std::fs::rename(&object_path, &renamed).unwrap();
        std::fs::rename(&renamed, &object_path).unwrap();
        std::fs::remove_file(&object_path).unwrap();
    }
}
