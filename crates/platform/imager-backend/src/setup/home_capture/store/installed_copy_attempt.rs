//! Installed-copy durable intent, never media or verified/adopted output authority.
//! Incomplete and unknown launches remain quarantined. Terminal material removal
//! requires the actual private completed importer and verifier custody.
use super::*;
mod cleanup;
pub(in crate::setup::home_capture) use cleanup::CompletedCleanup;
mod recovery;
use crate::setup::home_capture::installed_copy_output::{
    InstalledCopyCompletion, InstalledVerifiedArtifactFacts, InstalledVerifiedArtifacts,
    InstalledVerifiedExport,
};
use crate::setup::home_capture::{
    InstalledCopyScratch, InstalledCopyScratchFacts,
    original_copy_read::{OriginalCopyAttemptScope, OriginalCopyReadAdmission},
    original_worker::{OriginalWorkerOwner, OriginalWorkerRun},
};
use crate::setup::migration_handoff::{InstalledScratchFacts, InstalledScratchIdentity};
use launcher::owned_service::ServiceEndpoint;
pub use recovery::InstalledCopyRecovery;
use recovery::{same_payload, validate_artifacts};
use runtime_portal::process_binding::ExecutingPeer;
use sha2::Digest;
use std::{os::unix::fs::FileExt, sync::Mutex};
use zeroize::Zeroizing;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    operation: String,
    declaration: [u8; 32],
    person: [u8; 32],
    setup_binding: String,
    source_uid: u32,
    source_gid: u32,
    target_uid: u32,
    reservation: u64,
}
impl Binding {
    fn actual(scope: &OriginalCopyAttemptScope, target: &ExecutingPeer) -> io::Result<Self> {
        if !target.current() || target.uid() == 0 {
            return Err(failure("Installed copy target peer retired"));
        }
        Ok(Self {
            operation: scope.operation.clone(),
            declaration: scope.declaration,
            person: scope.person,
            setup_binding: scope.setup_binding.clone(),
            source_uid: scope.caller.0,
            source_gid: scope.caller.1,
            target_uid: target.uid(),
            reservation: scope.reservation,
        })
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Intended,
    ScratchReady,
    Launching,
    Exported,
    Verifying,
    Sealing,
    RecoveryPrepared,
    RecoveryVerifying,
    ReceiverPending,
    Cancelled,
    Complete,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Secret {
    dev: u64,
    ino: u64,
    len: u64,
    sha256: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TargetStorage {
    directory: (u64, u64),
    mount: u64,
    namespace: (u64, u64),
    partition_rdev: u64,
    disk_rdev: u64,
    volume_sha256: [u8; 32],
}
impl TargetStorage {
    fn actual(f: InstalledScratchFacts) -> io::Result<Self> {
        Ok(Self {
            directory: f.directory,
            mount: f.mount_id,
            namespace: f.namespace,
            partition_rdev: f.partition_rdev,
            disk_rdev: f.disk_rdev,
            volume_sha256: sha2::Sha256::digest(serde_json::to_vec(&f.volume)?).into(),
        })
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Scratch {
    stage_name: String,
    stage: (u64, u64),
    output: (u64, u64),
    inventory: (u64, u64),
    allowance: u64,
    parent: (u64, u64),
    mount: u64,
    namespace: (u64, u64),
    partition_rdev: u64,
    disk_rdev: u64,
    volume_sha256: [u8; 32],
}
impl Scratch {
    fn actual(f: &InstalledCopyScratchFacts, binding: &Binding, attempt: &str) -> io::Result<Self> {
        if f.operation != binding.operation
            || f.caller != (binding.source_uid, binding.source_gid)
            || f.reservation != binding.reservation
            || f.stage_name != format!("installed-copy-{}-{attempt}", binding.operation)
        {
            return Err(failure("Installed scratch/protected intent differs"));
        }
        Ok(Self {
            stage_name: f.stage_name.clone(),
            stage: f.stage,
            output: f.output,
            inventory: f.inventory,
            allowance: f.allowance,
            parent: f.filesystem.directory,
            mount: f.filesystem.mount_id,
            namespace: f.filesystem.namespace,
            partition_rdev: f.filesystem.partition_rdev,
            disk_rdev: f.filesystem.disk_rdev,
            volume_sha256: sha2::Sha256::digest(serde_json::to_vec(&f.filesystem.volume)?).into(),
        })
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Namespace {
    init_pid: u32,
    start_ticks: u64,
    namespace_dev: u64,
    namespace_ino: u64,
    custody: super::super::process::Custody,
}
impl Namespace {
    fn actual(endpoint: &ServiceEndpoint) -> io::Result<Self> {
        let a = endpoint.custody()?;
        if a.init_pid <= 1 || a.start_ticks == 0 || a.namespace_ino == 0 {
            return Err(failure("Installed copy namespace unavailable"));
        }
        // Reuse canonical boot/process-incarnation recovery. The UID is an
        // observation of THIS held endpoint's init, never a requested target UID.
        let uid = std::fs::metadata(format!("/proc/{}", a.init_pid))?.uid();
        let host = super::super::process::Incarnation::capture(a.init_pid, uid)?;
        let namespace = super::super::host_runner::Namespace {
            init_pid: a.init_pid,
            start_ticks: a.start_ticks,
            namespace_dev: a.namespace_dev,
            namespace_ino: a.namespace_ino,
        };
        let custody = super::super::process::Custody {
            host,
            namespace: Some(namespace),
            projection: None,
            launch_acknowledged: true,
            inventory_scratch: None,
        };
        if a != endpoint.custody()? {
            return Err(failure("Installed copy namespace changed"));
        }
        Ok(Self {
            init_pid: a.init_pid,
            start_ticks: a.start_ticks,
            namespace_dev: a.namespace_dev,
            namespace_ino: a.namespace_ino,
            custody,
        })
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receiver {
    pid: u32,
    start: u64,
    uid: u32,
    custody: super::super::process::Custody,
    expectations_digest: Option<[u8; 32]>,
}
impl Receiver {
    fn actual(peer: &ExecutingPeer, digest: Option<[u8; 32]>) -> io::Result<Self> {
        if !peer.current() {
            return Err(failure("Installed importer peer retired"));
        }
        let start = peer.start_time()?;
        let host = super::super::process::Incarnation::capture(peer.pid(), peer.uid())?;
        if !peer.current() || peer.start_time()? != start {
            return Err(failure("Installed importer incarnation changed"));
        }
        Ok(Self {
            pid: peer.pid(),
            start,
            uid: peer.uid(),
            custody: super::super::process::Custody {
                host,
                namespace: None,
                projection: None,
                launch_acknowledged: false,
                inventory_scratch: None,
            },
            expectations_digest: digest,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    binding: Binding,
    attempt: String,
    secret: Secret,
    target_storage: TargetStorage,
    phase: Phase,
    scratch: Option<Scratch>,
    read_claim: Option<[u8; 32]>,
    export_namespace: Option<Namespace>,
    verify_namespace: Option<Namespace>,
    // Worker response observations are never a Verified artifact token.
    exported_response_sha256: Option<[u8; 32]>,
    #[serde(default)]
    sealing: Option<InstalledVerifiedArtifactFacts>,
    #[serde(default)]
    verified: Option<InstalledVerifiedArtifactFacts>,
    #[serde(default)]
    recovery_namespace: Option<Namespace>,
    #[serde(default)]
    receiver: Option<Receiver>,
    #[serde(default)]
    material_retired: bool,
    #[serde(default)]
    metadata_quarantined: bool,
}
fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}
fn record_name(operation: &str) -> String {
    format!("{operation}.installed-copy-attempt")
}
fn secret_name(operation: &str) -> String {
    format!("{operation}.installed-copy-secret")
}
impl Record {
    fn validate(&self) -> io::Result<()> {
        operation(&self.binding.operation)?;
        if self.schema != 1
            || !canonical_uuid(&self.attempt)
            || self.binding.declaration == [0; 32]
            || self.binding.person == [0; 32]
            || self.binding.setup_binding.is_empty()
            || self.binding.setup_binding.len() > 256
            || self.binding.setup_binding.chars().any(char::is_control)
            || self.binding.source_uid == 0
            || self.binding.target_uid == 0
            || self.binding.reservation == 0
            || self.secret.ino == 0
            || !(32..=128).contains(&self.secret.len)
            || self.secret.sha256 == [0; 32]
        {
            return Err(failure("Installed copy durable binding invalid"));
        }
        let target = &self.target_storage;
        if [
            target.directory.1,
            target.mount,
            target.namespace.1,
            target.partition_rdev,
            target.disk_rdev,
        ]
        .contains(&0)
            || target.volume_sha256 == [0; 32]
        {
            return Err(failure("Installed copy target storage checkpoint invalid"));
        }
        if let Some(s) = &self.scratch {
            if (
                s.parent,
                s.mount,
                s.namespace,
                s.partition_rdev,
                s.disk_rdev,
                s.volume_sha256,
            ) != (
                target.directory,
                target.mount,
                target.namespace,
                target.partition_rdev,
                target.disk_rdev,
                target.volume_sha256,
            ) {
                return Err(failure(
                    "Installed scratch target physical identity differs",
                ));
            }
            if s.stage_name != format!("installed-copy-{}-{}", self.binding.operation, self.attempt)
                || [
                    s.stage.1,
                    s.output.1,
                    s.inventory.1,
                    s.parent.1,
                    s.mount,
                    s.namespace.1,
                    s.partition_rdev,
                    s.disk_rdev,
                    s.allowance,
                ]
                .contains(&0)
                || s.volume_sha256 == [0; 32]
            {
                return Err(failure("Installed copy scratch checkpoint invalid"));
            }
        }
        for n in [
            &self.export_namespace,
            &self.verify_namespace,
            &self.recovery_namespace,
        ]
        .into_iter()
        .flatten()
        {
            let expected = super::super::host_runner::Namespace {
                init_pid: n.init_pid,
                start_ticks: n.start_ticks,
                namespace_dev: n.namespace_dev,
                namespace_ino: n.namespace_ino,
            };
            if n.init_pid <= 1
                || n.start_ticks == 0
                || n.namespace_ino == 0
                || n.custody.host.pid != n.init_pid
                || n.custody.namespace != Some(expected)
                || !n.custody.launch_acknowledged
                || n.custody.projection.is_some()
                || n.custody.inventory_scratch.is_some()
            {
                return Err(failure("Installed copy namespace checkpoint invalid"));
            }
        }
        let shape = match self.phase {
            Phase::Intended => {
                self.scratch.is_none()
                    && self.read_claim.is_none()
                    && self.export_namespace.is_none()
                    && self.verify_namespace.is_none()
                    && self.exported_response_sha256.is_none()
            }
            Phase::ScratchReady => {
                self.scratch.is_some()
                    && self.read_claim.is_none()
                    && self.export_namespace.is_none()
                    && self.verify_namespace.is_none()
                    && self.exported_response_sha256.is_none()
            }
            Phase::Launching => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.verify_namespace.is_none()
                    && self.exported_response_sha256.is_none()
            }
            Phase::Exported => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.export_namespace.is_some()
                    && self.verify_namespace.is_none()
                    && self.exported_response_sha256.is_some()
            }
            Phase::Verifying => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.export_namespace.is_some()
                    && self.exported_response_sha256.is_some()
            }
            Phase::Sealing => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.export_namespace.is_some()
                    && self.verify_namespace.is_some()
                    && self.sealing.is_some()
                    && self.verified.is_none()
            }
            Phase::RecoveryPrepared | Phase::RecoveryVerifying => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.export_namespace.is_some()
                    && self.sealing.is_some()
            }
            Phase::ReceiverPending => {
                self.scratch.is_some()
                    && self.read_claim.is_some()
                    && self.export_namespace.is_some()
                    && self.verified.is_some()
            }
            Phase::Cancelled => self.scratch.is_some() && self.receiver.is_some(),
            // A serialized phase or claimed Complete message cannot mint success.
            Phase::Complete => {
                self.scratch.is_some() && self.verified.is_some() && self.receiver.is_some()
            }
        };
        if let Some(facts) = &self.sealing {
            validate_artifacts(facts, &self.binding, &self.target_storage, "output/")?;
        }
        if let Some(facts) = &self.verified {
            validate_artifacts(facts, &self.binding, &self.target_storage, "artifacts/")?;
        }
        if let (Some(a), Some(b)) = (&self.sealing, &self.verified) {
            same_payload(a, b)?;
        }
        if let Some(receiver) = &self.receiver {
            if receiver.pid <= 1
                || receiver.start == 0
                || receiver.uid != self.binding.target_uid
                || receiver.custody.host.pid != receiver.pid
                || receiver.custody.namespace.is_some()
                || receiver.custody.launch_acknowledged
                || receiver.custody.projection.is_some()
                || receiver.custody.inventory_scratch.is_some()
                || match receiver.expectations_digest {
                    Some(digest) => self
                        .verified
                        .as_ref()
                        .is_none_or(|f| f.expectations_digest != digest),
                    None => self.verified.is_some(),
                }
            {
                return Err(failure("Installed importer checkpoint invalid"));
            }
        }
        if !matches!(
            self.phase,
            Phase::Sealing
                | Phase::RecoveryPrepared
                | Phase::RecoveryVerifying
                | Phase::ReceiverPending
                | Phase::Cancelled
                | Phase::Complete
        ) && (self.sealing.is_some()
            || self.verified.is_some()
            || self.recovery_namespace.is_some())
        {
            return Err(failure("Installed artifact phase checkpoint shape invalid"));
        }
        if self.receiver.is_some() && matches!(self.phase, Phase::Intended | Phase::ScratchReady) {
            return Err(failure("Installed importer before launch intent"));
        }
        if (self.material_retired && self.phase != Phase::Complete)
            || (self.metadata_quarantined && !self.material_retired)
        {
            return Err(failure("Installed material retirement before completion"));
        }
        if !shape
            || self.read_claim == Some([0; 32])
            || self.exported_response_sha256 == Some([0; 32])
        {
            return Err(failure("Installed copy durable phase shape invalid"));
        }
        Ok(())
    }
}
fn load(store: &Store) -> io::Result<Option<Record>> {
    store.check_original_output_operation(&store.operation)?;
    let directory = storage(&store.records)?;
    let mut file = match directory.open_read(
        OsStr::new(&record_name(&store.operation)),
        Privacy::OwnerOnly,
    ) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() > MAX_RECORD {
        return Err(failure("Installed copy journal exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err(failure("Installed copy journal exceeds bounds"));
    }
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| failure("Installed copy journal invalid"))?;
    record.validate()?;
    if record.binding.operation != store.operation {
        return Err(failure("Installed copy journal operation differs"));
    }
    store.check_original_output_operation(&store.operation)?;
    Ok(Some(record))
}
fn save(store: &Store, record: &Record, mode: PublishMode) -> io::Result<()> {
    record.validate()?;
    store.check_original_output_operation(&record.binding.operation)?;
    let bytes = serde_json::to_vec(record)?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err(failure("Installed copy journal exceeds bounds"));
    }
    storage(&store.records)?
        .publish(
            OsStr::new(&record_name(&record.binding.operation)),
            &bytes,
            mode,
            Privacy::OwnerOnly,
        )
        .map_err(|e| e.into_io_error())?;
    store.check_original_output_operation(&record.binding.operation)
}
fn secret_facts(file: &File) -> io::Result<Secret> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != 0
        || m.gid() != 0
        || m.mode() & 0o7777 != 0o600
        || !(32..=128).contains(&m.len())
    {
        return Err(failure("Installed copy secret profile invalid"));
    }
    let mut bytes = Zeroizing::new(vec![0; m.len() as usize]);
    file.read_exact_at(&mut bytes, 0)?;
    let result = Secret {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        sha256: sha2::Sha256::digest(&*bytes).into(),
    };
    let after = file.metadata()?;
    if (
        after.dev(),
        after.ino(),
        after.len(),
        after.uid(),
        after.gid(),
        after.mode(),
        after.nlink(),
    ) != (
        m.dev(),
        m.ino(),
        m.len(),
        m.uid(),
        m.gid(),
        m.mode(),
        m.nlink(),
    ) {
        return Err(failure("Installed copy secret changed"));
    }
    Ok(result)
}
/// Retains actual operation lease, reviewed original sources and exact target PID.
/// Raw JSON and Secret/Scratch DTOs cannot construct this object.
pub struct InstalledCopyAttempt {
    store: Arc<Mutex<Store>>,
    source: Arc<OriginalCopyReadAdmission>,
    target: Arc<ExecutingPeer>,
    binding: Binding,
    attempt: String,
    secret: File,
    secret_identity: Secret,
    physical: InstalledScratchIdentity,
    target_storage: TargetStorage,
    receiver_peer: Mutex<Option<Arc<ExecutingPeer>>>,
}
impl InstalledCopyAttempt {
    /// Route selection only. An existing record still needs typed recovery,
    /// independently stopped processes and a new authentic full verifier.
    pub(in crate::setup::home_capture) fn exists(store: &Store) -> io::Result<bool> {
        store.installed_copy_snapshots(&store.operation)?;
        let present = load(store)?.is_some();
        store.check_original_output_operation(&store.operation)?;
        Ok(present)
    }
    /// Fresh intent is durable before any scratch mkdir or signed worker launch.
    pub(in crate::setup::home_capture) fn prepare(
        store: Arc<Mutex<Store>>,
        source: Arc<OriginalCopyReadAdmission>,
        target: Arc<ExecutingPeer>,
        secret: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let binding = Binding::actual(&source.output_attempt_scope(cancel)?, &target)?;
        let lock = store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let snapshots = lock.installed_copy_snapshots(&binding.operation)?;
        let physical = InstalledScratchIdentity::review(
            migration_durability::readable_directory(&snapshots.file)?,
            source.clone(),
            cancel,
        )?;
        let target_storage = TargetStorage::actual(physical.facts(cancel)?)?;
        if load(&lock)?.is_some() {
            return Err(failure(
                "Installed copy attempt requires authenticated recovery; automatic retry refused",
            ));
        }
        if !(32..=128).contains(&secret.len()) {
            return Err(failure("Installed copy secret size invalid"));
        }
        let directory = storage(&lock.records)?;
        physical.revalidate(cancel)?;
        directory
            .publish(
                OsStr::new(&secret_name(&binding.operation)),
                &secret,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(|e| e.into_io_error())?;
        let held = directory.open_read(
            OsStr::new(&secret_name(&binding.operation)),
            Privacy::OwnerOnly,
        )?;
        let secret_identity = secret_facts(&held)?;
        let published_digest: [u8; 32] = sha2::Sha256::digest(&*secret).into();
        if secret_identity.sha256 != published_digest {
            return Err(failure("Installed copy secret publication differs"));
        }
        let attempt = uuid::Uuid::new_v4().to_string();
        let record = Record {
            schema: 1,
            binding: binding.clone(),
            attempt: attempt.clone(),
            secret: secret_identity.clone(),
            target_storage: target_storage.clone(),
            phase: Phase::Intended,
            scratch: None,
            read_claim: None,
            export_namespace: None,
            verify_namespace: None,
            exported_response_sha256: None,
            sealing: None,
            verified: None,
            recovery_namespace: None,
            receiver: None,
            material_retired: false,
            metadata_quarantined: false,
        };
        if Binding::actual(&source.output_attempt_scope(cancel)?, &target)? != binding {
            return Err(failure("Installed copy protected binding changed"));
        }
        if TargetStorage::actual(physical.facts(cancel)?)? != target_storage {
            return Err(failure(
                "Installed copy target changed before journal publication",
            ));
        }
        save(&lock, &record, PublishMode::IfAbsent)?;
        drop(lock);
        let value = Self {
            store,
            source,
            target,
            binding,
            attempt,
            secret: held,
            secret_identity,
            physical,
            target_storage,
            receiver_peer: Mutex::new(None),
        };
        value.check(cancel)?;
        Ok(value)
    }
    pub fn attempt(&self) -> &str {
        &self.attempt
    }
    pub(in crate::setup::home_capture) fn store(&self) -> Arc<Mutex<Store>> {
        self.store.clone()
    }
    pub(in crate::setup::home_capture) fn source(&self) -> Arc<OriginalCopyReadAdmission> {
        self.source.clone()
    }
    pub(in crate::setup::home_capture) fn check_prepared(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.phase != Phase::Intended
        {
            return Err(failure(
                "Installed copy scratch requires the durable prepared intent",
            ));
        }
        drop(store);
        self.check(cancel)
    }
    pub fn target(&self) -> Arc<ExecutingPeer> {
        self.target.clone()
    }
    pub fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        if Binding::actual(&self.source.output_attempt_scope(cancel)?, &self.target)?
            != self.binding
        {
            return Err(failure("Installed copy source/target binding changed"));
        }
        if TargetStorage::actual(self.physical.facts(cancel)?)? != self.target_storage {
            return Err(failure("Installed copy held target storage changed"));
        }
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        store.installed_copy_snapshots(&self.binding.operation)?;
        let record =
            load(&store)?.ok_or_else(|| failure("Installed copy durable intent missing"))?;
        if record.binding != self.binding
            || record.attempt != self.attempt
            || record.secret != self.secret_identity
            || record.target_storage != self.target_storage
        {
            return Err(failure("Installed copy durable intent changed"));
        }
        let named = storage(&store.records)?.open_read(
            OsStr::new(&secret_name(&self.binding.operation)),
            Privacy::OwnerOnly,
        )?;
        if secret_facts(&named)? != self.secret_identity
            || secret_facts(&self.secret)? != self.secret_identity
        {
            return Err(failure("Installed copy durable secret changed"));
        }
        if matches!(record.phase, Phase::Cancelled | Phase::Complete) {
            return Err(failure("Installed copy attempt retired"));
        }
        let read_claim = record.read_claim;
        drop(store);
        if let Some(digest) = read_claim {
            if self.source.output_read_claim_digest(cancel)? != digest {
                return Err(failure("Installed copy current read claim changed"));
            }
        }
        self.source.revalidate(cancel)?;
        if !self.target.current() {
            return Err(failure("Installed copy target peer retired"));
        }
        Ok(())
    }
    fn scratch(&self, scratch: &InstalledCopyScratch, cancel: &AtomicBool) -> io::Result<Scratch> {
        self.check(cancel)?;
        if !scratch.matches_source(&self.source) {
            return Err(failure(
                "Installed scratch belongs to another source allocation",
            ));
        }
        Scratch::actual(&scratch.facts(cancel)?, &self.binding, &self.attempt)
    }
    pub fn bind_scratch(
        &self,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let facts = self.scratch(scratch, cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.phase != Phase::Intended
            || record.scratch.is_some()
            || record.attempt != self.attempt
            || record.binding != self.binding
        {
            return Err(failure("Installed scratch checkpoint state differs"));
        }
        record.scratch = Some(facts);
        record.phase = Phase::ScratchReady;
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)
    }
    pub fn mark_launching(
        &self,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<[u8; 32]> {
        let facts = self.scratch(scratch, cancel)?;
        let digest = self.source.output_read_claim_digest(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.phase != Phase::ScratchReady
            || record.scratch.as_ref() != Some(&facts)
            || record.attempt != self.attempt
            || record.binding != self.binding
        {
            return Err(failure("Installed copy launch checkpoint differs"));
        }
        record.phase = Phase::Launching;
        record.read_claim = Some(digest);
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)?;
        Ok(digest)
    }
    /// Called by the shared signed runner BEFORE provider request/secret bytes.
    pub fn record_namespace(
        &self,
        scratch: &InstalledCopyScratch,
        endpoint: &ServiceEndpoint,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let facts = self.scratch(scratch, cancel)?;
        let namespace = Namespace::actual(endpoint)?;
        let current_claim = self.source.output_read_claim_digest(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.read_claim != Some(current_claim)
            || record.scratch.as_ref() != Some(&facts)
        {
            return Err(failure("Installed copy namespace intent differs"));
        }
        let destination = match record.phase {
            Phase::Launching => &mut record.export_namespace,
            Phase::Verifying => &mut record.verify_namespace,
            Phase::RecoveryVerifying => &mut record.recovery_namespace,
            _ => return Err(failure("Installed copy namespace phase differs")),
        };
        if destination.is_some() {
            return Err(failure("Installed copy namespace already recorded"));
        }
        *destination = Some(namespace);
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)
    }
    /// Full response is persisted as an observation, never a Verified token.
    pub(in crate::setup::home_capture) fn record_exported<O: OriginalWorkerOwner>(
        &self,
        scratch: &InstalledCopyScratch,
        owner: &Arc<O>,
        run: &OriginalWorkerRun<O>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let facts = self.scratch(scratch, cancel)?;
        owner.check()?;
        if !run.belongs_to(owner) || owner.operation() != self.binding.operation {
            return Err(failure("Installed copy export owner differs"));
        }
        let response = run.quiescent_response()?;
        let sealed = match response {
            super::super::worker::Response::ExportOriginal { home, applications } => {
                home.validate()?;
                if home.setup_binding != self.binding.setup_binding {
                    return Err(failure("Installed copy export setup differs"));
                }
                let mut total = home.sealed_bytes;
                if let Some(applications) = applications {
                    applications.validate()?;
                    if applications.setup_binding != self.binding.setup_binding {
                        return Err(failure("Installed applications setup differs"));
                    }
                    total = total
                        .checked_add(applications.sealed_bytes)
                        .ok_or_else(|| failure("Installed copy exported size overflows"))?;
                }
                total
            }
            super::super::worker::Response::ExportOriginalApplications { applications } => {
                applications.validate()?;
                if applications.setup_binding != self.binding.setup_binding {
                    return Err(failure("Installed applications setup differs"));
                }
                applications.sealed_bytes
            }
            _ => return Err(failure("Installed copy export response differs")),
        };
        if sealed > self.binding.reservation
            || self.source.output_read_claim_digest(cancel)? != owner.read_claim_digest()
        {
            return Err(failure(
                "Installed copy export budget/current claim differs",
            ));
        }
        let hash = sha2::Sha256::digest(serde_json::to_vec(response)?).into();
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.phase != Phase::Launching
            || record.export_namespace.is_none()
            || record.read_claim != Some(owner.read_claim_digest())
            || record.scratch.as_ref() != Some(&facts)
        {
            return Err(failure("Installed copy export checkpoint differs"));
        }
        record.exported_response_sha256 = Some(hash);
        record.phase = Phase::Exported;
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)
    }
    pub fn mark_verifying(
        &self,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<[u8; 32]> {
        let facts = self.scratch(scratch, cancel)?;
        let digest = self.source.output_read_claim_digest(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.phase != Phase::Exported
            || record.read_claim != Some(digest)
            || record.scratch.as_ref() != Some(&facts)
        {
            return Err(failure("Installed copy verify checkpoint differs"));
        }
        record.phase = Phase::Verifying;
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)?;
        Ok(digest)
    }
    /// Cleanup-only recovery observation; does not authenticate artifacts,
    /// authorize retry, publication, namespace signalling or stage removal.
    pub fn require_namespaces_retired(&self) -> io::Result<()> {
        // Retirement observation survives cancellation, revoked source authority
        // and target-peer exit. It grants no new IO/launch/publication capability.
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        store.installed_copy_snapshots(&self.binding.operation)?;
        let record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.secret != self.secret_identity
            || record.target_storage != self.target_storage
            || !matches!(
                record.phase,
                Phase::Launching
                    | Phase::Exported
                    | Phase::Verifying
                    | Phase::Sealing
                    | Phase::RecoveryPrepared
                    | Phase::RecoveryVerifying
                    | Phase::ReceiverPending
                    | Phase::Cancelled
            )
        {
            return Err(failure(
                "Installed copy has no launched namespace retirement proof",
            ));
        }
        let named = storage(&store.records)?.open_read(
            OsStr::new(&secret_name(&self.binding.operation)),
            Privacy::OwnerOnly,
        )?;
        if secret_facts(&named)? != self.secret_identity
            || secret_facts(&self.secret)? != self.secret_identity
        {
            return Err(failure("Installed copy retirement secret custody changed"));
        }
        let before = serde_json::to_vec(&record)?;
        record
            .export_namespace
            .as_ref()
            .ok_or_else(|| failure("Unknown launch cannot guess namespace absence"))?
            .custody
            .require_stopped()?;
        if matches!(
            record.phase,
            Phase::Verifying
                | Phase::Sealing
                | Phase::RecoveryPrepared
                | Phase::RecoveryVerifying
                | Phase::ReceiverPending
                | Phase::Cancelled
        ) {
            record
                .verify_namespace
                .as_ref()
                .ok_or_else(|| failure("Unknown verifier launch cannot guess namespace absence"))?
                .custody
                .require_stopped()?;
        }
        if record.phase == Phase::RecoveryVerifying && record.recovery_namespace.is_none() {
            return Err(failure(
                "Unknown recovery verifier cannot guess namespace absence",
            ));
        }
        if let Some(namespace) = &record.recovery_namespace {
            namespace.custody.require_stopped()?;
        }
        let after = load(&store)?
            .ok_or_else(|| failure("Installed copy retirement checkpoint disappeared"))?;
        if serde_json::to_vec(&after)? != before {
            return Err(failure("Installed copy retirement checkpoint changed"));
        }
        store.check_original_output_operation(&self.binding.operation)
    }
    /// Same exact held/named root-private secret; only durable launch phases may
    /// release bytes to the controller's existing authenticated provider flow.
    pub fn secret(&self, cancel: &AtomicBool) -> io::Result<Zeroizing<Vec<u8>>> {
        self.check(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if !matches!(
            record.phase,
            Phase::Launching | Phase::Verifying | Phase::RecoveryVerifying
        ) || record.read_claim != Some(self.source.output_read_claim_digest(cancel)?)
        {
            return Err(failure("Installed copy secret release phase differs"));
        }
        drop(store);
        let mut bytes = Zeroizing::new(vec![0; self.secret_identity.len as usize]);
        self.secret.read_exact_at(&mut bytes, 0)?;
        self.check(cancel)?;
        Ok(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Record {
        Record {
            schema: 1,
            binding: Binding {
                operation: "14fb49d2-1f60-466f-afb5-9fdc3e3d417d".into(),
                declaration: [1; 32],
                person: [2; 32],
                setup_binding: "setup".into(),
                source_uid: 1000,
                source_gid: 1000,
                target_uid: 1001,
                reservation: 1,
            },
            attempt: "75f0f98f-c7c7-4615-aede-74473918e6cb".into(),
            secret: Secret {
                dev: 1,
                ino: 1,
                len: 32,
                sha256: [3; 32],
            },
            target_storage: TargetStorage {
                directory: (1, 2),
                mount: 3,
                namespace: (4, 5),
                partition_rdev: 6,
                disk_rdev: 7,
                volume_sha256: [8; 32],
            },
            phase: Phase::Intended,
            scratch: None,
            read_claim: None,
            export_namespace: None,
            verify_namespace: None,
            exported_response_sha256: None,
            sealing: None,
            verified: None,
            recovery_namespace: None,
            receiver: None,
            material_retired: false,
            metadata_quarantined: false,
        }
    }
    #[test]
    fn journal_refuses_outside_secret_bounds_and_noncanonical_attempt() {
        for len in [0, 31, 129, u64::MAX] {
            let mut record = fixture();
            record.secret.len = len;
            assert!(record.validate().is_err());
        }
        let mut record = fixture();
        record.attempt = "75F0F98F-C7C7-4615-AEDE-74473918E6CB".into();
        assert!(record.validate().is_err());
    }
    #[test]
    fn journal_cannot_turn_an_expectation_or_phase_into_verified_authority() {
        let mut record = fixture();
        record.phase = Phase::ReceiverPending;
        assert!(record.validate().is_err());
        let mut record = fixture();
        record.phase = Phase::Exported;
        record.exported_response_sha256 = Some([4; 32]);
        assert!(record.validate().is_err());
        let mut record = fixture();
        record.phase = Phase::Launching;
        record.read_claim = Some([0; 32]);
        assert!(record.validate().is_err());
    }
    #[test]
    fn intended_attempt_cannot_claim_export_or_launch_evidence() {
        let mut record = fixture();
        record.read_claim = Some([4; 32]);
        assert!(record.validate().is_err());
        let mut record = fixture();
        record.exported_response_sha256 = Some([4; 32]);
        assert!(record.validate().is_err());
    }
    #[test]
    fn unverified_phase_labels_do_not_supply_closed_artifacts_or_completion() {
        for phase in [
            Phase::Sealing,
            Phase::RecoveryPrepared,
            Phase::RecoveryVerifying,
            Phase::ReceiverPending,
            Phase::Complete,
        ] {
            let mut record = fixture();
            record.phase = phase;
            assert!(record.validate().is_err());
        }
    }
    #[test]
    fn material_retirement_labels_require_terminal_completion() {
        let mut record = fixture();
        record.material_retired = true;
        assert!(record.validate().is_err());
        let mut record = fixture();
        record.metadata_quarantined = true;
        assert!(record.validate().is_err());
    }
}
