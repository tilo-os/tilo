//! Installed, explicitly authorized managed processes and shared supervision.
//! No ambient identity, package trust or process-global environment is changed.
use crate::{manifest::MinimalManifest, registry::InstalledPackage};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use tilo_app_package::{BlobStore, PackageId};

pub const WORKSPACE_AGENT_PACKAGE_ID: &str = "so.tilo.agent";

/// Independently configured release trust. Embedded publisher keys are never
/// added here. The enrolled host supplies this from its qualified release config.
#[derive(Clone, Debug)]
pub struct PackageTrust {
    pub publishers: Vec<[u8; 32]>,
    pub attestors: Vec<tilo_app_package::trust::TrustedAttestor>,
    pub revoked: tilo_app_package::trust::RevocationList,
}

#[derive(Clone, Debug)]
pub struct InstalledManagedPackage {
    installed: InstalledPackage,
    data_root: PathBuf,
    id: PackageId,
}
impl InstalledManagedPackage {
    pub fn verify(
        installed: InstalledPackage,
        data_root: PathBuf,
        id: PackageId,
        trust: &PackageTrust,
    ) -> Result<Self, String> {
        if installed.id != WORKSPACE_AGENT_PACKAGE_ID || installed.package_id_hex != id.to_hex() {
            return Err("managed package must be so.tilo.agent".into());
        }
        let this = Self {
            installed: InstalledPackage {
                id: installed.id,
                package_id_hex: installed.package_id_hex,
                payload_root: installed
                    .payload_root
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
                manifest_path: installed
                    .manifest_path
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
            },
            data_root: data_root.canonicalize().map_err(|e| e.to_string())?,
            id,
        };
        this.revalidate(trust)?;
        Ok(this)
    }
    pub fn manifest_path(&self) -> &Path {
        &self.installed.manifest_path
    }
    pub fn package_id(&self) -> PackageId {
        self.id
    }
    pub fn payload_root(&self) -> &Path {
        &self.installed.payload_root
    }
    pub fn revalidate(&self, trust: &PackageTrust) -> Result<MinimalManifest, String> {
        self.verified_manifest(trust).map(|(manifest, _)| manifest)
    }
    fn verified_manifest(
        &self,
        trust: &PackageTrust,
    ) -> Result<(MinimalManifest, tilo_app_package::BlobHash), String> {
        let evidence = install::integrity::record_dir(&self.data_root, WORKSPACE_AGENT_PACKAGE_ID);
        let signed = std::fs::read(evidence.join("signed-root")).map_err(|e| e.to_string())?;
        let root = tilo_app_package::SignedRoot::decode(&signed).map_err(|e| e.to_string())?;
        let manifest = std::fs::read(&self.installed.manifest_path).map_err(|e| e.to_string())?;
        let recorded = std::fs::read(evidence.join("manifest.toml")).map_err(|e| e.to_string())?;
        if manifest != recorded {
            return Err("installed manifest differs from signed evidence".into());
        }
        let collection_bytes =
            std::fs::read(evidence.join("collection.qcl")).map_err(|e| e.to_string())?;
        let collection =
            tilo_app_package::Collection::decode(&collection_bytes).map_err(|e| e.to_string())?;
        let mut blobs = tilo_app_package::MemoryBlobStore::new();
        for bytes in [&signed, &manifest, &collection_bytes] {
            blobs.put(bytes).map_err(|e| e.to_string())?;
        }
        for (relative, _) in &collection.entries {
            let path = self.installed.payload_root.join(relative);
            if Path::new(relative)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || !path
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .starts_with(&self.installed.payload_root)
                || !std::fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .is_file()
            {
                return Err("installed payload path escapes its verified package".into());
            }
            blobs
                .put(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        let verified = match tilo_app_package::verify_with_trust(
            self.id,
            &blobs,
            &trust.publishers,
            &trust.attestors,
            &trust.revoked,
        ) {
            tilo_app_package::Verification::Trusted { manifest }
            | tilo_app_package::Verification::AttestedBy { manifest, .. } => manifest,
            failure => return Err(format!("managed package verification failed: {failure:?}")),
        };
        if verified.package.id != WORKSPACE_AGENT_PACKAGE_ID
            || tilo_app_package::BlobHash::of(&manifest) != root.manifest_hash
        {
            return Err("managed package identity or manifest mismatch".into());
        }
        // The existing integrity walker follows directories. Refuse links and
        // unusual files before entering it so unknown links cannot escape or
        // recurse forever, including links absent from the signed collection.
        let mut directories = vec![self.installed.payload_root.clone()];
        let mut entries = 0usize;
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                entries += 1;
                if entries > 65_536 {
                    return Err("managed payload has too many filesystem entries".into());
                }
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                if kind.is_dir() {
                    directories.push(entry.path());
                } else if !kind.is_file() {
                    return Err("managed payload contains a link or special file".into());
                }
            }
        }
        let report = install::integrity::verify(
            &self.data_root,
            WORKSPACE_AGENT_PACKAGE_ID,
            &self.id.to_hex(),
            &self.installed.payload_root,
        )
        .map_err(|e| e.to_string())?;
        if !report.is_intact() || !report.unexpected.is_empty() {
            return Err("installed managed payload is not intact".into());
        }
        // Parse the exact bytes already verified above, never reread a path
        // that could have changed after signature/integrity validation.
        let manifest: MinimalManifest =
            toml::from_str(std::str::from_utf8(&manifest).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if manifest.kind != crate::manifest::KIND_SERVICE
            || manifest.entrypoint.cli_bin().is_none()
            || !manifest
                .sandbox
                .services
                .get(crate::services::COLLAB_SERVICE)
                .is_some_and(|value| {
                    crate::manifest::ServiceReq::parse(value).is_ok_and(|req| req.is_required())
                })
        {
            return Err("managed package requires a headless service and collaboration".into());
        }
        if manifest.sandbox.net.mode != "off"
            || !manifest
                .sandbox
                .services
                .get(crate::services::MODEL_GATEWAY_SERVICE)
                .is_some_and(|value| {
                    crate::manifest::ServiceReq::parse(value).is_ok_and(|req| req.is_required())
                })
        {
            return Err("managed agent requires network off and model gateway".into());
        }
        let entry = manifest
            .entrypoint
            .cli_bin()
            .ok_or("missing managed executable")?;
        let digest = collection
            .entries
            .iter()
            .find(|(path, _)| path.as_str() == entry)
            .map(|(_, digest)| *digest)
            .ok_or("managed executable is not signed payload")?;
        Ok((manifest, digest))
    }
}

/// Labels supplied by a committed host owner. These carry no launch authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedBinding {
    pub room: String,
    pub app: String,
    pub agent: String,
    pub task: String,
    pub run: String,
    pub launch: String,
    pub generation: u64,
}
impl ManagedBinding {
    pub fn validate(&self) -> Result<(), String> {
        if self.app != WORKSPACE_AGENT_PACKAGE_ID
            || self.generation == 0
            || uuid::Uuid::parse_str(&self.room).is_err()
            || [&self.agent, &self.task, &self.run, &self.launch]
                .iter()
                .any(|value| !valid_label(value))
        {
            return Err("invalid managed binding".into());
        }
        Ok(())
    }
}
fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start: u64,
}
impl ProcessIdentity {
    pub fn read(pid: u32) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            if pid == 0 {
                return Err("invalid process".into());
            }
            let stat =
                std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
            let fields: Vec<_> = stat
                .rsplit_once(") ")
                .ok_or("invalid process stat")?
                .1
                .split_whitespace()
                .collect();
            if fields.first() == Some(&"Z") {
                return Err("process exited".into());
            }
            let start = fields
                .get(19)
                .ok_or("missing process start")?
                .parse()
                .map_err(|_| "invalid start")?;
            Ok(Self { pid, start })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            Err("kernel managed custody requires Linux".into())
        }
    }
    pub fn is_current(self) -> bool {
        Self::read(self.pid).is_ok_and(|current| current == self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedRuntimeFacts {
    pub runtime_epoch: String,
    pub package_id: String,
    pub package_hash: String,
    pub executable_hash: String,
    pub runtime_hash: String,
}
#[derive(Clone, Debug)]
pub struct ManagedRuntime {
    pub bundle_root: PathBuf,
    pub runtime_publisher: String,
    pub sandbox_shim: PathBuf,
    pub bwrap: PathBuf,
}
struct RuntimeOwner {
    package: InstalledManagedPackage,
    trust: PackageTrust,
    runtime: ManagedRuntime,
    facts: ManagedRuntimeFacts,
    live: std::sync::atomic::AtomicBool,
}
impl RuntimeOwner {
    fn revalidate(&self) -> Result<(), String> {
        if !self.live.load(std::sync::atomic::Ordering::Acquire) {
            return Err("runtime owner stopped".into());
        }
        let (manifest, executable) = self.package.verified_manifest(&self.trust)?;
        let bundle = verify_bundle(&manifest, &self.runtime)?;
        if bundle.root_hash != self.facts.runtime_hash
            || executable.to_hex() != self.facts.executable_hash
        {
            return Err("managed runtime identity changed".into());
        }
        Ok(())
    }
}
/// A live, nonserializable owner lease; recorded facts cannot recreate it.
#[derive(Clone)]
pub struct VerifiedManagedRuntime(Weak<RuntimeOwner>);
impl VerifiedManagedRuntime {
    pub fn revalidate(&self) -> Result<ManagedRuntimeFacts, String> {
        let owner = self.0.upgrade().ok_or("runtime owner lost")?;
        owner.revalidate()?;
        Ok(owner.facts.clone())
    }
}
fn verify_bundle(
    manifest: &MinimalManifest,
    runtime: &ManagedRuntime,
) -> Result<runtime_bundle::BundleManifest, String> {
    let (_, version) = manifest
        .runtime_ref()
        .ok_or("managed package requires runtime")?;
    let bundle = runtime_bundle::verify_bundle_with(
        &runtime.bundle_root,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(runtime.runtime_publisher.clone()),
            require_signature: true,
            require_arch: Some(std::env::consts::ARCH.into()),
        },
    )
    .map_err(|e| format!("runtime verification: {e}"))?;
    if bundle.version != version {
        return Err("runtime version differs from package".into());
    }
    Ok(bundle)
}
/// Implemented only by the installed durable host owner, never by a wire peer.
pub trait ManagedLaunchAuthority: Send + Sync {
    /// Consume the exact durable host launch permit once. An error never authorizes retry.
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String>;
    /// Check retained binding and current policy without consuming/recreating a claim.
    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String>;
}
/// A single reservation in one owning runtime. Intentionally neither Clone nor Serde.
pub struct VerifiedLaunchAuthority {
    owner: Weak<RuntimeOwner>,
    binding: ManagedBinding,
    channel_id: Option<String>,
    authority: Arc<dyn ManagedLaunchAuthority>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
impl VerifiedLaunchAuthority {
    /// Repeatable current-policy check, independent of the consumed host claim.
    pub fn revalidate(&self) -> Result<(), String> {
        let owner = self.owner.upgrade().ok_or("runtime owner lost")?;
        owner.revalidate()?;
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("managed launch stopped".into());
        }
        self.authority.revalidate_launch(&self.binding)
    }
}
struct ManagedAttempt {
    binding: ManagedBinding,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    terminal: Arc<std::sync::atomic::AtomicBool>,
}
#[derive(Default, Clone)]
pub struct ProcessRegistry(Arc<Mutex<HashMap<ProcessIdentity, Arc<ProcessRecord>>>>);
struct ProcessRecord {
    binding: ManagedBinding,
    runtime: VerifiedManagedRuntime,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "linux")]
    kernel: KernelProcess,
    #[cfg(target_os = "linux")]
    admission_attempted: Mutex<bool>,
    #[cfg(target_os = "linux")]
    agent: Option<channel::ProcessEvidence>,
}
#[derive(Clone)]
pub struct VerifiedManagedSupervisor {
    registry: Weak<Mutex<HashMap<ProcessIdentity, Arc<ProcessRecord>>>>,
    record: Arc<ProcessRecord>,
    process: ProcessIdentity,
}
impl VerifiedManagedSupervisor {
    pub fn binding(&self) -> &ManagedBinding {
        &self.record.binding
    }
    pub fn process(&self) -> ProcessIdentity {
        self.process
    }
    pub fn runtime_facts(&self) -> Result<ManagedRuntimeFacts, String> {
        self.record.runtime.revalidate()
    }
    pub fn revalidate(
        &self,
        binding: &ManagedBinding,
        process: ProcessIdentity,
    ) -> Result<(), String> {
        self.record.runtime.revalidate()?;
        if self
            .record
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
            || binding != self.binding()
            || process != self.process
            || !process.is_current()
        {
            return Err("managed process no longer current".into());
        }
        let registry = self.registry.upgrade().ok_or("process owner lost")?;
        if !registry
            .lock()
            .map_err(|_| "process registry poisoned")?
            .get(&process)
            .is_some_and(|record| Arc::ptr_eq(record, &self.record))
        {
            return Err("managed process custody retired".into());
        }
        #[cfg(target_os = "linux")]
        if let Some(agent) = &self.record.agent {
            agent.revalidate(process)?;
        }
        Ok(())
    }
}
impl ProcessRegistry {
    #[cfg(target_os = "linux")]
    fn register(
        &self,
        process: ProcessIdentity,
        binding: ManagedBinding,
        runtime: VerifiedManagedRuntime,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<VerifiedManagedSupervisor, String> {
        binding.validate()?;
        runtime.revalidate()?;
        let kernel = KernelProcess::open(process)?;
        let mut records = self.0.lock().map_err(|_| "process registry poisoned")?;
        if records.contains_key(&process) || cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("process already bound or cancelled".into());
        }
        let record = Arc::new(ProcessRecord {
            binding,
            runtime,
            cancelled,
            kernel,
            admission_attempted: Mutex::new(false),
            agent: None,
        });
        records.insert(process, record.clone());
        Ok(VerifiedManagedSupervisor {
            registry: Arc::downgrade(&self.0),
            record,
            process,
        })
    }
    #[cfg(target_os = "linux")]
    fn remove(&self, process: ProcessIdentity) {
        if let Ok(mut records) = self.0.lock() {
            records.remove(&process);
            records.retain(|_, record| {
                !record
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.supervisor == process)
            });
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessEvent {
    Exited,
    Failed { code: Option<i32> },
    WaitFailed { detail: String },
}
#[derive(Clone)]
pub enum ManagedLaunchEvent {
    Started(VerifiedManagedSupervisor),
    Finished {
        binding: ManagedBinding,
        process: Option<ProcessIdentity>,
        event: ProcessEvent,
        exit: VerifiedManagedExit,
    },
    Failed {
        binding: ManagedBinding,
        process: Option<ProcessIdentity>,
        terminal: bool,
        reason: String,
    },
}
/// Owns one bounded set of launch attempts and exact child reapers. It never recovers a child by PID.
pub struct ManagedLauncher {
    owner: Arc<RuntimeOwner>,
    authority: Arc<dyn ManagedLaunchAuthority>,
    registry: ProcessRegistry,
    attempts: HashMap<String, ManagedAttempt>,
    events: Arc<Mutex<VecDeque<ManagedLaunchEvent>>>,
    threads: Vec<(ManagedBinding, std::thread::JoinHandle<()>)>,
}
impl ManagedLauncher {
    pub fn new(
        package: InstalledManagedPackage,
        trust: PackageTrust,
        mut runtime: ManagedRuntime,
        authority: Arc<dyn ManagedLaunchAuthority>,
    ) -> Result<Self, String> {
        let (manifest, executable) = package.verified_manifest(&trust)?;
        runtime.bundle_root = runtime
            .bundle_root
            .canonicalize()
            .map_err(|e| e.to_string())?;
        runtime.sandbox_shim = runtime
            .sandbox_shim
            .canonicalize()
            .map_err(|e| e.to_string())?;
        runtime.bwrap = runtime.bwrap.canonicalize().map_err(|e| e.to_string())?;
        let bundle = verify_bundle(&manifest, &runtime)?;
        let facts = ManagedRuntimeFacts {
            runtime_epoch: uuid::Uuid::new_v4().to_string(),
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
            package_hash: package.id.to_hex(),
            executable_hash: executable.to_hex(),
            runtime_hash: bundle.root_hash,
        };
        Ok(Self {
            owner: Arc::new(RuntimeOwner {
                package,
                trust,
                runtime,
                facts,
                live: true.into(),
            }),
            authority,
            registry: ProcessRegistry::default(),
            attempts: HashMap::new(),
            events: Default::default(),
            threads: vec![],
        })
    }
    pub fn runtime_lease(&self) -> VerifiedManagedRuntime {
        VerifiedManagedRuntime(Arc::downgrade(&self.owner))
    }
    pub fn authorize(
        &mut self,
        binding: ManagedBinding,
    ) -> Result<VerifiedLaunchAuthority, String> {
        binding.validate()?;
        // Reserve before runtime checks, the one-use host claim, descriptors or workers.
        // Even a refused/ambiguous claim retains this identity and cannot be attempted again.
        if self.attempts.contains_key(&binding.launch) {
            return Err("launch already reserved; original status only".into());
        }
        if self.attempts.len() >= 1024
            || self
                .attempts
                .values()
                .filter(|attempt| !attempt.terminal.load(std::sync::atomic::Ordering::Acquire))
                .count()
                >= 4
        {
            return Err("managed launch capacity exhausted".into());
        }
        if self.events.lock().map_err(|_| "event owner lost")?.len() > 8 {
            return Err("managed event queue full".into());
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.attempts.insert(
            binding.launch.clone(),
            ManagedAttempt {
                binding: binding.clone(),
                cancelled: cancelled.clone(),
                terminal: Arc::new(false.into()),
            },
        );
        self.owner.revalidate()?;
        self.authority.claim_launch(&binding)?;
        let permit = VerifiedLaunchAuthority {
            owner: Arc::downgrade(&self.owner),
            binding,
            channel_id: None,
            authority: self.authority.clone(),
            cancelled,
        };
        permit.revalidate()?;
        Ok(permit)
    }
    pub fn drain_events(&mut self) -> Result<Vec<ManagedLaunchEvent>, String> {
        let mut index = 0;
        while index < self.threads.len() {
            if self.threads[index].1.is_finished() {
                let (binding, thread) = self.threads.swap_remove(index);
                if thread.join().is_err() {
                    if let Ok(mut events) = self.events.lock() {
                        events.push_back(ManagedLaunchEvent::Failed {
                            binding,
                            process: None,
                            terminal: false,
                            reason: "managed worker panicked; outcome uncertain".into(),
                        });
                    }
                }
            } else {
                index += 1;
            }
        }
        self.events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .map_err(|_| "managed event owner poisoned; outcome uncertain".into())
    }
    pub fn stop(&self, binding: &ManagedBinding) -> Result<(), String> {
        let attempt = self
            .attempts
            .get(&binding.launch)
            .filter(|a| &a.binding == binding)
            .ok_or("not an owned launch")?;
        attempt
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        #[cfg(target_os = "linux")]
        for record in self
            .registry
            .0
            .lock()
            .map_err(|_| "process owner poisoned")?
            .values()
            .filter(|r| &r.binding == binding)
        {
            record.kernel.terminate()?;
        }
        Ok(())
    }
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.owner
            .live
            .store(false, std::sync::atomic::Ordering::Release);
        let mut failed = None;
        for attempt in self.attempts.values() {
            attempt
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
        }
        #[cfg(target_os = "linux")]
        if let Ok(records) = self.registry.0.lock() {
            for record in records.values() {
                if let Err(error) = record.kernel.terminate() {
                    failed.get_or_insert(error);
                }
            }
        } else {
            failed = Some("process owner poisoned".into());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !self.threads.is_empty() {
            let mut index = 0;
            while index < self.threads.len() {
                if self.threads[index].1.is_finished() {
                    let (_, thread) = self.threads.swap_remove(index);
                    if thread.join().is_err() {
                        failed.get_or_insert("managed worker panicked; outcome uncertain".into());
                    }
                } else {
                    index += 1;
                }
            }
            if self.threads.is_empty() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                failed.get_or_insert("managed cleanup deadline exceeded; exact child remains with original worker, outcome uncertain".into());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match self.registry.0.lock() {
            Ok(mut records) => records.clear(),
            Err(_) => {
                failed.get_or_insert("process registry poisoned".into());
            }
        }
        failed.map_or(Ok(()), Err)
    }
    #[cfg(not(target_os = "linux"))]
    pub fn launch(
        &mut self,
        permit: VerifiedLaunchAuthority,
        services: crate::services::EnrolledServiceContext,
    ) -> Result<(), String> {
        let _ = (&permit.owner, &permit.binding, &permit.channel_id, services);
        Err("managed execution requires Linux".into())
    }
}
impl Drop for ManagedLauncher {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
#[cfg(target_os = "linux")]
struct KernelProcess(std::fs::File);
#[cfg(target_os = "linux")]
impl KernelProcess {
    fn open(process: ProcessIdentity) -> Result<Self, String> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, process.pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let handle = Self(unsafe { std::fs::File::from_raw_fd(fd as i32) });
        if !process.is_current() {
            return Err("process changed before kernel capture".into());
        }
        Ok(handle)
    }
    fn terminate(&self) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
#[path = "managed_channel.rs"]
pub mod channel;

/// Proven wait result from this launch owner's exact Child handle. Diagnostic
/// PID labels or process absence cannot construct this witness.
#[derive(Clone)]
pub struct VerifiedManagedExit {
    binding: ManagedBinding,
    runtime: ManagedRuntimeFacts,
    supervisor: ProcessIdentity,
    status: std::process::ExitStatus,
}
impl VerifiedManagedExit {
    pub fn binding(&self) -> &ManagedBinding {
        &self.binding
    }
    pub fn runtime_facts(&self) -> &ManagedRuntimeFacts {
        &self.runtime
    }
    pub fn supervisor(&self) -> ProcessIdentity {
        self.supervisor
    }
    pub fn status(&self) -> std::process::ExitStatus {
        self.status
    }
}
#[cfg(target_os = "linux")]
pub(crate) trait EnrolledProcessObserver {
    fn before_spawn(&self) -> std::io::Result<()>;
    fn child_created(&self);
    fn supervise(&self, child: std::process::Child) -> std::io::Result<std::process::ExitStatus>;
}
#[cfg(target_os = "linux")]
pub(crate) fn verify_runtime(
    manifest: &MinimalManifest,
    opts: &crate::run::RunOptions,
) -> Result<(PathBuf, String), String> {
    let root = opts
        .runtime_root
        .as_ref()
        .ok_or("explicit runtime missing")?;
    let publisher = opts
        .require_runtime_publisher
        .as_ref()
        .ok_or("explicit runtime publisher missing")?;
    let bundle = runtime_bundle::verify_bundle_with(
        root,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(publisher.clone()),
            require_signature: true,
            require_arch: Some(std::env::consts::ARCH.into()),
        },
    )
    .map_err(|e| e.to_string())?;
    if manifest
        .runtime_ref()
        .is_none_or(|(_, version)| version != bundle.version)
    {
        return Err("runtime does not match package".into());
    }
    Ok((root.clone(), bundle.version))
}
#[cfg(target_os = "linux")]
impl ManagedLauncher {
    pub fn launch(
        &mut self,
        permit: VerifiedLaunchAuthority,
        mut services: crate::services::EnrolledServiceContext,
    ) -> Result<(), String> {
        if !Weak::ptr_eq(&permit.owner, &Arc::downgrade(&self.owner)) {
            return Err("permit belongs to another runtime".into());
        }
        permit.revalidate()?;
        if services.binding() != &permit.binding
            || services.runtime_epoch() != self.owner.facts.runtime_epoch
            || permit.channel_id.as_deref() != Some(services.channel_id())
        {
            return Err("managed channel/runtime/launch binding mismatch".into());
        }
        let attempt = self
            .attempts
            .get(&permit.binding.launch)
            .ok_or("launch reservation missing")?;
        if attempt.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("managed launch stopped".into());
        }
        let opts = crate::run::RunOptions {
            runtime_root: Some(self.owner.runtime.bundle_root.clone()),
            require_runtime_publisher: Some(self.owner.runtime.runtime_publisher.clone()),
            sandbox_shim_bin: Some(self.owner.runtime.sandbox_shim.clone()),
            bwrap_bin: Some(self.owner.runtime.bwrap.clone()),
            workspace_id: Some(permit.binding.room.clone()),
            app_id: Some(permit.binding.app.clone()),
            mode: crate::run::LaunchMode::Service {
                passthrough_argv: vec![
                    "--task-runs".into(),
                    "--workspace".into(),
                    permit.binding.room.clone(),
                    "--agent-id".into(),
                    permit.binding.agent.clone(),
                    "--name".into(),
                    permit.binding.agent.clone(),
                ],
            },
            ..Default::default()
        };
        let binding = permit.binding.clone();
        let worker_binding = binding.clone();
        let owner = self.owner.clone();
        let registry = self.registry.clone();
        let events = self.events.clone();
        let cancelled = attempt.cancelled.clone();
        let terminal = attempt.terminal.clone();
        let thread_failure = terminal.clone();
        let thread = std::thread::Builder::new()
            .name("tilo-managed-agent".into())
            .spawn(move || {
                struct Observer {
                    permit: VerifiedLaunchAuthority,
                    binding: ManagedBinding,
                    owner: Arc<RuntimeOwner>,
                    registry: ProcessRegistry,
                    events: Arc<Mutex<VecDeque<ManagedLaunchEvent>>>,
                    cancelled: Arc<std::sync::atomic::AtomicBool>,
                    terminal: Arc<std::sync::atomic::AtomicBool>,
                    spawned: std::sync::atomic::AtomicBool,
                    process: Mutex<Option<ProcessIdentity>>,
                }
                impl EnrolledProcessObserver for Observer {
                    fn before_spawn(&self) -> std::io::Result<()> {
                        self.permit.revalidate().map_err(std::io::Error::other)
                    }
                    fn child_created(&self) {
                        self.spawned
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    fn supervise(
                        &self,
                        child: std::process::Child,
                    ) -> std::io::Result<std::process::ExitStatus> {
                        let mut child = OwnedChild(child);
                        let process = ProcessIdentity::read(child.id());
                        let proof = process.and_then(|process| {
                            *self
                                .process
                                .lock()
                                .map_err(|_| "process observer poisoned")? = Some(process);
                            self.registry.register(
                                process,
                                self.binding.clone(),
                                VerifiedManagedRuntime(Arc::downgrade(&self.owner)),
                                self.cancelled.clone(),
                            )
                        });
                        let bind_error = match proof {
                            Ok(proof) => {
                                if let Ok(mut events) = self.events.lock() {
                                    events.push_back(ManagedLaunchEvent::Started(proof));
                                    None
                                } else {
                                    let _ = child.kill();
                                    Some("managed event owner poisoned".to_string())
                                }
                            }
                            Err(error) => {
                                let _ = child.kill();
                                Some(error)
                            }
                        };
                        let status = loop {
                            match child.try_wait() {
                                Ok(Some(status)) => break Ok(status),
                                Ok(None) => {}
                                Err(error) => {
                                    let _ = child.kill();
                                    break Err(error);
                                }
                            }
                            if self.cancelled.load(std::sync::atomic::Ordering::Acquire)
                                || !self.owner.live.load(std::sync::atomic::Ordering::Acquire)
                            {
                                // Child owns the original unreaped process; no PID lookup is used.
                                if let Err(error) = child.kill() {
                                    if error.kind() != std::io::ErrorKind::InvalidInput {
                                        break Err(error);
                                    }
                                }
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        };
                        let process = self.process.lock().ok().and_then(|p| *p);
                        if let Ok(status) = status {
                            self.terminal
                                .store(true, std::sync::atomic::Ordering::Release);
                            if let Some(process) = process {
                                self.registry.remove(process);
                                let exit = VerifiedManagedExit {
                                    binding: self.binding.clone(),
                                    runtime: self.owner.facts.clone(),
                                    supervisor: process,
                                    status,
                                };
                                if let Ok(mut events) = self.events.lock() {
                                    events.push_back(ManagedLaunchEvent::Finished {
                                        binding: self.binding.clone(),
                                        process: Some(process),
                                        event: if status.success() {
                                            ProcessEvent::Exited
                                        } else {
                                            ProcessEvent::Failed {
                                                code: status.code(),
                                            }
                                        },
                                        exit,
                                    });
                                }
                            }
                        }
                        if let Some(error) = bind_error {
                            return Err(std::io::Error::other(error));
                        }
                        status
                    }
                }
                let observer = Observer {
                    permit,
                    binding,
                    owner,
                    registry,
                    events,
                    cancelled,
                    terminal,
                    spawned: false.into(),
                    process: Mutex::new(None),
                };
                let result = (|| {
                    observer.permit.revalidate()?;
                    if observer
                        .cancelled
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        return Err("launch stopped before spawn".into());
                    }
                    let manifest = observer.owner.package.revalidate(&observer.owner.trust)?;
                    crate::run::run_enrolled(
                        &manifest,
                        observer.owner.package.payload_root(),
                        &opts,
                        &mut services,
                        &observer,
                    )
                    .map_err(|e| e.to_string())
                })();
                if let Err(reason) = result {
                    if !observer.spawned.load(std::sync::atomic::Ordering::Acquire) {
                        observer
                            .terminal
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    if let Ok(mut events) = observer.events.lock() {
                        events.push_back(ManagedLaunchEvent::Failed {
                            binding: observer.binding.clone(),
                            process: observer.process.lock().ok().and_then(|p| *p),
                            terminal: observer.terminal.load(std::sync::atomic::Ordering::Acquire),
                            reason,
                        });
                    }
                }
            })
            .map_err(|e| {
                thread_failure.store(true, std::sync::atomic::Ordering::Release);
                format!("managed worker unavailable; attempt retained: {e}")
            })?;
        self.threads.push((worker_binding, thread));
        Ok(())
    }
}

/// Created only from actual kernel sender evidence and successful canonical binding.
#[derive(Clone)]
pub struct VerifiedManagedProcess(VerifiedManagedSupervisor);
impl VerifiedManagedProcess {
    #[cfg(target_os = "linux")]
    pub fn facts(&self) -> Result<ChannelProcessFacts, String> {
        self.revalidate(self.binding(), self.process())?;
        let evidence = self.0.record.agent.as_ref().ok_or("not an agent process")?;
        Ok(ChannelProcessFacts {
            binding: self.binding().clone(),
            runtime: self.0.runtime_facts()?,
            process: self.process(),
            supervisor: evidence.supervisor,
            channel_id: evidence.channel_id.clone(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    pub fn facts(&self) -> Result<ChannelProcessFacts, String> {
        Err("managed process custody requires Linux".into())
    }
    pub fn binding(&self) -> &ManagedBinding {
        self.0.binding()
    }
    pub fn process(&self) -> ProcessIdentity {
        self.0.process()
    }
    pub fn revalidate(
        &self,
        binding: &ManagedBinding,
        process: ProcessIdentity,
    ) -> Result<(), String> {
        self.0.revalidate(binding, process)
    }
}

/// Descriptive durable evidence. Only VerifiedManagedProcess confers live custody.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelProcessFacts {
    pub binding: ManagedBinding,
    pub runtime: ManagedRuntimeFacts,
    pub process: ProcessIdentity,
    pub supervisor: ProcessIdentity,
    pub channel_id: String,
}

/// Authenticated process proof and the exact socket admitted alongside it.
/// The broker consumes this wrapper; callers cannot replace its socket.
pub struct AdmittedManagedConnection {
    process: VerifiedManagedProcess,
    stream: std::os::unix::net::UnixStream,
}
impl AdmittedManagedConnection {
    pub fn process(&self) -> &VerifiedManagedProcess {
        &self.process
    }
    pub fn into_parts(self) -> (VerifiedManagedProcess, std::os::unix::net::UnixStream) {
        (self.process, self.stream)
    }
}
#[cfg(not(target_os = "linux"))]
pub mod channel {
    pub use super::{AdmittedManagedConnection, ChannelProcessFacts, VerifiedManagedProcess};
    pub trait ManagedProcessAuthority {
        fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String>;
    }
}
#[cfg(target_os = "linux")]
struct OwnedChild(std::process::Child);
#[cfg(target_os = "linux")]
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[cfg(target_os = "linux")]
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
#[cfg(target_os = "linux")]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
