//! Installed, explicitly authorized managed processes and shared supervision.
//! No ambient identity, package trust or process-global environment is changed.
use crate::manifest::ManifestView;
use package_store::InstalledPackage;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use tilo_app_package::PackageId;

pub const WORKSPACE_AGENT_PACKAGE_ID: &str = "so.tilo.agent";
/// Non-terminal managed launches one launcher holds. A started agent stays
/// non-terminal while its process lives (idle agents wait for messages), so
/// this must be at least the Store's live-agent limit
/// (`tilo_peer_state::owner_commit::MAX_LIVE_PRIVATE_AGENTS`, 16).
pub const MAX_LIVE_MANAGED_LAUNCHES: usize = 16;

/// Independently configured release trust. Embedded publisher keys are never
/// added here. The enrolled host supplies this from its qualified release config.
#[derive(Clone, Debug)]
pub struct PackageTrust {
    pub publishers: Vec<[u8; 32]>,
    pub attestors: Vec<tilo_app_package::trust::TrustedAttestor>,
    pub revoked: tilo_app_package::trust::RevocationList,
}

impl PackageTrust {
    /// Load the independently configured application trust used by the installer.
    /// The installed package's self-declared publisher never grants trust.
    ///
    /// # Errors
    /// Refuses unavailable configuration roots or malformed trust/revocation files.
    pub fn from_user_config() -> Result<Self, String> {
        let attestors = tilo_app_package::default_trusted_attestors_path()
            .ok_or("application trust configuration directory is unavailable")?;
        let revoked = tilo_app_package::default_revoked_keys_path()
            .ok_or("application revocation configuration directory is unavailable")?;
        Self::from_config_paths(&attestors, &revoked)
    }

    /// Load explicit host-owned paths; absent files mean no additional trust.
    ///
    /// # Errors
    /// Refuses malformed or unreadable files, including a broken revocation file.
    pub fn from_config_paths(attestors: &Path, revoked: &Path) -> Result<Self, String> {
        Ok(Self {
            publishers: Vec::new(),
            attestors: tilo_app_package::trust::TrustedAttestors::load(attestors)
                .map_err(|error| format!("application attestor configuration: {error}"))?
                .as_slice()
                .to_vec(),
            revoked: tilo_app_package::trust::RevocationList::load(revoked)
                .map_err(|error| format!("application revocation configuration: {error}"))?,
        })
    }
}

/// The registry row for the workspace agent installed at `payload_root`
/// with `manifest_path` under the signed package root `package_id_hex`, as an
/// enrolled host configures it outside the package store.
#[must_use]
pub fn agent_package_row(
    package_id_hex: String,
    payload_root: PathBuf,
    manifest_path: PathBuf,
) -> InstalledPackage {
    InstalledPackage {
        id: WORKSPACE_AGENT_PACKAGE_ID.into(),
        version: String::new(),
        package_id_hex,
        payload_root,
        manifest_path,
        publisher_key_hex: String::new(),
        pinned: false,
        installed_at: 0,
        last_used: 0,
        attested_by: String::new(),
        cli_commands: Vec::new(),
        runtime: String::new(),
        source: String::new(),
        held: false,
        payload_bytes: 0,
        system: false,
    }
}

#[derive(Clone, Debug)]
pub struct InstalledManagedPackage {
    installed: InstalledPackage,
    data_root: PathBuf,
    id: PackageId,
}
impl InstalledManagedPackage {
    /// Bind an installed agent package and verify it against `trust`.
    ///
    /// # Errors
    ///
    /// Returns a description when the package is not the workspace agent, a path cannot be
    /// canonicalized, or verification fails.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public constructor shared with the cloud host; the owned root mirrors `installed`"
    )]
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
                payload_root: installed
                    .payload_root
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
                manifest_path: installed
                    .manifest_path
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
                ..installed
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
    /// Re-verify the installed package and return its signed manifest.
    ///
    /// # Errors
    ///
    /// Returns a description of the first verification failure.
    pub fn revalidate(&self, trust: &PackageTrust) -> Result<ManifestView, String> {
        self.verified_manifest(trust).map(|(manifest, _)| manifest)
    }
    fn verified_manifest(
        &self,
        trust: &PackageTrust,
    ) -> Result<(ManifestView, tilo_app_package::BlobHash), String> {
        let evidence = self.record_dir();
        let signed = std::fs::read(evidence.join("signed-root")).map_err(|e| e.to_string())?;
        let root = tilo_app_package::SignedRoot::decode(&signed).map_err(|e| e.to_string())?;
        let manifest = std::fs::read(&self.installed.manifest_path).map_err(|e| e.to_string())?;
        let recorded = std::fs::read(evidence.join("manifest.toml")).map_err(|e| e.to_string())?;
        if manifest != recorded {
            return Err("installed manifest differs from signed evidence".into());
        }
        let collection_bytes =
            std::fs::read(evidence.join("collection.qcl")).map_err(|e| e.to_string())?;
        // The signed root must name this package and commit to exactly the
        // installed manifest and the recorded payload collection.
        let meta = tilo_app_package::PackageMeta::from_parts(
            self.id,
            &signed,
            manifest.clone(),
            collection_bytes,
        )
        .map_err(|e| e.to_string())?;
        let collection = &meta.collection;
        for (relative, expected) in &collection.entries {
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
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            if tilo_app_package::BlobHash::of(&bytes) != *expected {
                return Err("installed managed payload is not intact".into());
            }
        }
        let verified = match tilo_app_package::verify_with_trust(
            &meta,
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
        refuse_unusual_entries(&self.installed.payload_root)?;
        // Replay the recorded chain (identity, signature, both covered
        // hashes); every named payload file was hashed above, so what is left
        // is a file the signed collection does not name.
        package_store::integrity::read_record(
            &evidence,
            WORKSPACE_AGENT_PACKAGE_ID,
            &self.id.to_hex(),
        )
        .map_err(|e| e.to_string())?;
        // Legacy installs copied the manifest into the payload. The current
        // content-addressed store keeps metadata outside its signed payload
        // tree; its authoritative manifest was checked above. Never require
        // a new, unsigned file in that tree merely to launch a managed app.
        let payload_manifest = self.installed.payload_root.join(MANIFEST_FILE);
        if payload_manifest.exists() || evidence == managed_record_dir(&self.data_root) {
            let installed_manifest = std::fs::read(payload_manifest).map_err(|e| e.to_string())?;
            if installed_manifest != recorded {
                return Err("installed manifest differs from signed evidence".into());
            }
        }
        refuse_unexpected_files(&self.installed.payload_root, collection)?;
        // Parse the exact bytes already verified above, never reread a path
        // that could have changed after signature/integrity validation.
        let manifest: ManifestView =
            toml::from_str(std::str::from_utf8(&manifest).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        require_managed_service(&manifest)?;
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

    fn record_dir(&self) -> PathBuf {
        let recorded = package_store::integrity::record_dir(&self.installed);
        if self
            .installed
            .manifest_path
            .parent()
            .is_some_and(|parent| parent == recorded)
        {
            recorded
        } else {
            managed_record_dir(&self.data_root)
        }
    }
}

/// Where the managed agent's signed install evidence lives under its
/// explicitly configured data root (the pre-content-store installer layout).
fn managed_record_dir(data_root: &Path) -> PathBuf {
    package_store::integrity::legacy_record_dir(data_root, WORKSPACE_AGENT_PACKAGE_ID)
}

/// The manifest copy the installer writes at the payload root.
const MANIFEST_FILE: &str = "manifest.toml";

/// Refuse a regular file under `root` that neither the signed `collection`
/// names nor is the installer's manifest copy.
fn refuse_unexpected_files(
    root: &Path,
    collection: &tilo_app_package::Collection,
) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if !collection.entries.contains_key(&rel) && rel != MANIFEST_FILE {
                return Err("installed managed payload is not intact".into());
            }
        }
    }
    Ok(())
}

/// The existing integrity walker follows directories. Refuse links and unusual
/// files before entering it so unknown links cannot escape or recurse forever,
/// including links absent from the signed collection.
fn refuse_unusual_entries(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            entries = entries.saturating_add(1);
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
    Ok(())
}
/// A managed agent is a headless service with required collaboration and
/// model gateway services and no network.
fn require_managed_service(manifest: &ManifestView) -> Result<(), String> {
    let requires = |service: &str| {
        manifest.sandbox.services.get(service).is_some_and(|value| {
            crate::manifest::ServiceReq::parse(value).is_ok_and(|req| req.is_required())
        })
    };
    if manifest.kind != crate::manifest::KIND_SERVICE
        || manifest.entrypoint.cli_bin().is_none()
        || !requires(crate::services::COLLAB_SERVICE)
    {
        return Err("managed package requires a headless service and collaboration".into());
    }
    if manifest.sandbox.net.mode != "off" || !requires(crate::services::MODEL_GATEWAY_SERVICE) {
        return Err("managed agent requires network off and model gateway".into());
    }
    Ok(())
}

/// Labels supplied by a committed host owner. These carry no launch authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedLaunchScope {
    /// A retained collaborator process, independent of any queued task.
    AgentSession,
    /// A process reserved for an existing owner-committed task run.
    Task { task: String, run: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedBinding {
    pub room: String,
    pub app: String,
    pub agent: String,
    pub scope: ManagedLaunchScope,
    pub launch: String,
    pub generation: u64,
}
impl ManagedBinding {
    /// Check the binding's package, generation and label syntax.
    ///
    /// # Errors
    ///
    /// Returns a description when any label is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.app != WORKSPACE_AGENT_PACKAGE_ID
            || self.generation == 0
            || uuid::Uuid::parse_str(&self.room).is_err()
            || [&self.agent, &self.launch]
                .iter()
                .any(|value| !valid_label(value))
            || matches!(&self.scope, ManagedLaunchScope::Task { task, run }
                if !valid_label(task) || !valid_label(run))
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
    /// Read the kernel start time identifying a live process.
    ///
    /// # Errors
    ///
    /// Returns a description when the process is absent, exited, unreadable, or when kernel
    /// custody is unsupported on this platform.
    pub fn read(pid: u32) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            if pid == 0 {
                return Err("invalid process".into());
            }
            let proc_stat =
                std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
            let fields: Vec<_> = proc_stat
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
impl ManagedRuntime {
    /// Resolve the normal declared runtime against independent host trust.
    /// Unlike an ordinary developer launch, a managed agent cannot accept an
    /// arbitrary signed publisher or run without its declared runtime.
    /// `sandbox_shim` and `bwrap` are selected by the owning host, not the app.
    ///
    /// # Errors
    /// Refuses missing trust, policy bypasses, or an untrusted/missing runtime.
    pub fn from_launch_options(
        manifest: &ManifestView,
        options: &crate::run::RunOptions,
        sandbox_shim: PathBuf,
        bwrap: PathBuf,
    ) -> Result<Self, String> {
        if options.allow_unsigned_runtime || options.allow_missing_runtime {
            return Err("managed runtime cannot bypass signature or availability policy".into());
        }
        let publishers = crate::run::load_trusted_publisher_list()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        if options.require_runtime_publisher.is_none() && publishers.is_empty() {
            return Err("managed runtime requires independently configured publisher trust".into());
        }
        let (bundle_root, _) = crate::run::resolve_runtime(manifest, options)
            .map_err(|error| error.to_string())?
            .ok_or("managed agent requires a declared runtime")?;
        let bundle = runtime_bundle::verify_bundle_with(
            &bundle_root,
            &runtime_bundle::VerifyOptions {
                require_publisher: options.require_runtime_publisher.clone(),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
        let runtime_publisher = bundle.publisher_hex.ok_or("managed runtime is unsigned")?;
        if options.require_runtime_publisher.is_none()
            && !publishers
                .iter()
                .any(|publisher| publisher.eq_ignore_ascii_case(&runtime_publisher))
        {
            return Err("managed runtime publisher is not in host trust configuration".into());
        }
        let runtime = Self {
            bundle_root,
            runtime_publisher,
            sandbox_shim,
            bwrap,
        };
        // Also check the app's exact declared runtime name/version. Never use
        // an explicit root override to bypass that managed launch contract.
        verify_bundle(manifest, &runtime)?;
        Ok(runtime)
    }
}
struct RuntimeOwner {
    package: InstalledManagedPackage,
    trust: PackageTrust,
    runtime: ManagedRuntime,
    facts: ManagedRuntimeFacts,
    live: std::sync::atomic::AtomicBool,
    /// Last full verification and the exact filesystem stamp it covered.
    verified: Mutex<Option<(std::time::Instant, TreeStamp)>>,
}
/// Longest interval a full package/runtime verification is reused while no
/// verified file or directory changed. The full check re-reads, re-hashes
/// and signature-verifies every payload and runtime byte (tens of
/// milliseconds), and it ran on every process-fact read: dozens of times per
/// agent step.
const RUNTIME_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);
/// Most filesystem entries one stamp covers; larger trees always verify fully.
const STAMP_ENTRIES: usize = 65_536;
/// Metadata of every entry under the verified roots (payload, installed
/// manifest, integrity evidence, runtime bundle), in walk order. Any write,
/// replace, rename, link, chmod or new entry changes a file's or its parent
/// directory's inode, length, mtime or ctime (ctime cannot be set by users),
/// so an equal stamp means no verified byte or name changed since it was
/// taken. It is only a reuse hint; `RUNTIME_RECHECK` bounds its age.
#[derive(PartialEq, Eq, Debug)]
struct TreeStamp(Vec<(PathBuf, [i64; 8])>);
impl TreeStamp {
    #[cfg(unix)]
    fn of(roots: &[&Path]) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let mut entries = vec![];
        let mut pending: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
        while let Some(path) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            entries.push((
                path.clone(),
                [
                    metadata.dev().cast_signed(),
                    metadata.ino().cast_signed(),
                    metadata.len().cast_signed(),
                    i64::from(metadata.mode()),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ],
            ));
            if entries.len() > STAMP_ENTRIES {
                return None;
            }
            if metadata.is_dir() {
                let mut children = std::fs::read_dir(&path)
                    .ok()?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                children.sort();
                pending.extend(children);
            }
        }
        Some(Self(entries))
    }
    #[cfg(not(unix))]
    fn of(_: &[&Path]) -> Option<Self> {
        None
    }
}
impl RuntimeOwner {
    fn stamp(&self) -> Option<TreeStamp> {
        let evidence = self.package.record_dir();
        TreeStamp::of(&[
            &self.package.installed.payload_root,
            &self.package.installed.manifest_path,
            &evidence,
            &self.runtime.bundle_root,
        ])
    }
    fn revalidate(&self) -> Result<(), String> {
        if !self.live.load(std::sync::atomic::Ordering::Acquire) {
            return Err("runtime owner stopped".into());
        }
        let reusable = self
            .verified
            .lock()
            .map_err(|_| "runtime verification cache poisoned")?
            .as_ref()
            .filter(|(at, _)| at.elapsed() < RUNTIME_RECHECK)
            .is_some_and(|(_, stamp)| self.stamp().is_some_and(|now| &now == stamp));
        if reusable {
            return Ok(());
        }
        // Stamp before and after the full check: only an unchanged tree
        // across the whole verification is retained.
        let before = self.stamp();
        let began = std::time::Instant::now();
        self.revalidate_fully()?;
        let after = self.stamp();
        let retained = match (before, after) {
            (Some(before), Some(after)) if before == after => Some((began, after)),
            _ => None,
        };
        *self
            .verified
            .lock()
            .map_err(|_| "runtime verification cache poisoned")? = retained;
        Ok(())
    }
    fn revalidate_fully(&self) -> Result<(), String> {
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
    /// Re-verify the owning runtime and return its recorded facts.
    ///
    /// # Errors
    ///
    /// Returns a description when the owner stopped or verification fails.
    pub fn revalidate(&self) -> Result<ManagedRuntimeFacts, String> {
        let owner = self.0.upgrade().ok_or("runtime owner lost")?;
        owner.revalidate()?;
        Ok(owner.facts.clone())
    }
}
fn verify_bundle(
    manifest: &ManifestView,
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
    ///
    /// # Errors
    ///
    /// Returns a description when the host refuses or cannot record the claim.
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String>;
    /// Check retained binding and current policy without consuming/recreating a claim.
    ///
    /// # Errors
    ///
    /// Returns a description when the binding is no longer permitted.
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
    ///
    /// # Errors
    ///
    /// Returns a description when the runtime, cancellation or host policy refuses the launch.
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
    /// Rung on cancellation; the launch's supervisor sleeps on it and the
    /// child's exit instead of polling both.
    #[cfg(target_os = "linux")]
    wake: Arc<bounded_command::WakeFd>,
}
impl ManagedAttempt {
    fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        #[cfg(target_os = "linux")]
        self.wake.ring();
    }
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
    /// Re-verify the runtime and return its recorded facts.
    ///
    /// # Errors
    ///
    /// As [`VerifiedManagedRuntime::revalidate`].
    pub fn runtime_facts(&self) -> Result<ManagedRuntimeFacts, String> {
        self.record.runtime.revalidate()
    }
    /// Check that `binding` and `process` are still this supervisor's live custody.
    ///
    /// # Errors
    ///
    /// Returns a description when the runtime, binding, process or custody is no longer current.
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
        drop(records);
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
#[expect(
    clippy::large_enum_variant,
    reason = "a few launch events are queued at a time; boxing would reshape the public variants hosts match"
)]
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
    /// Verify the package and runtime once and become their single owner.
    ///
    /// # Errors
    ///
    /// Returns a description when the package, runtime paths or runtime bundle fail verification.
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
                verified: Mutex::new(None),
            }),
            authority,
            registry: ProcessRegistry::default(),
            attempts: HashMap::new(),
            events: Arc::default(),
            threads: vec![],
        })
    }
    pub fn runtime_lease(&self) -> VerifiedManagedRuntime {
        VerifiedManagedRuntime(Arc::downgrade(&self.owner))
    }
    /// Reserve `binding.launch` and consume its host launch claim.
    ///
    /// # Errors
    ///
    /// Returns a description for an invalid or reused binding, exhausted capacity, or a refused
    /// runtime check or host claim; the reservation is retained either way.
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
                >= MAX_LIVE_MANAGED_LAUNCHES
        {
            return Err("managed launch capacity exhausted".into());
        }
        if self.events.lock().map_err(|_| "event owner lost")?.len() > 8 {
            return Err("managed event queue full".into());
        }
        #[cfg(target_os = "linux")]
        let wake = Arc::new(
            bounded_command::WakeFd::new()
                .map_err(|e| format!("managed launch wake unavailable: {e}"))?,
        );
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.attempts.insert(
            binding.launch.clone(),
            ManagedAttempt {
                binding: binding.clone(),
                cancelled: cancelled.clone(),
                terminal: Arc::new(false.into()),
                #[cfg(target_os = "linux")]
                wake,
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
    /// Reap finished workers and take the queued launch events.
    ///
    /// # Errors
    ///
    /// Returns a description when the event owner is poisoned; outcomes are then uncertain.
    pub fn drain_events(&mut self) -> Result<Vec<ManagedLaunchEvent>, String> {
        let mut index = 0;
        while index < self.threads.len() {
            if self
                .threads
                .get(index)
                .is_some_and(|(_, thread)| thread.is_finished())
            {
                let (binding, thread) = self.threads.swap_remove(index);
                if thread.join().is_err()
                    && let Ok(mut events) = self.events.lock()
                {
                    events.push_back(ManagedLaunchEvent::Failed {
                        binding,
                        process: None,
                        terminal: false,
                        reason: "managed worker panicked; outcome uncertain".into(),
                    });
                }
            } else {
                index = index.saturating_add(1);
            }
        }
        self.events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .map_err(|_| "managed event owner poisoned; outcome uncertain".into())
    }
    /// Cancel an owned launch and kill its live processes.
    ///
    /// # Errors
    ///
    /// Returns a description for a launch this owner does not hold or a failed kill.
    pub fn stop(&self, binding: &ManagedBinding) -> Result<(), String> {
        let attempt = self
            .attempts
            .get(&binding.launch)
            .filter(|a| &a.binding == binding)
            .ok_or("not an owned launch")?;
        attempt.cancel();
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
    /// Stop every launch and wait a bounded time for their workers.
    ///
    /// # Errors
    ///
    /// Returns the first cleanup failure; outcomes of affected launches are uncertain.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.owner
            .live
            .store(false, std::sync::atomic::Ordering::Release);
        let mut failed = None;
        // Cancelling every attempt also wakes its supervisor for the
        // `live` store above, which has no wake of its own.
        for attempt in self.attempts.values() {
            attempt.cancel();
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
        let began = std::time::Instant::now();
        let deadline = began
            .checked_add(std::time::Duration::from_secs(2))
            .unwrap_or(began);
        while !self.threads.is_empty() {
            let mut index = 0;
            while index < self.threads.len() {
                if self
                    .threads
                    .get(index)
                    .is_some_and(|(_, thread)| thread.is_finished())
                {
                    let (_, thread) = self.threads.swap_remove(index);
                    if thread.join().is_err() {
                        failed.get_or_insert_with(|| {
                            "managed worker panicked; outcome uncertain".into()
                        });
                    }
                } else {
                    index = index.saturating_add(1);
                }
            }
            if self.threads.is_empty() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                failed.get_or_insert_with(|| "managed cleanup deadline exceeded; exact child remains with original worker, outcome uncertain".into());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match self.registry.0.lock() {
            Ok(mut records) => records.clear(),
            Err(_) => {
                failed.get_or_insert_with(|| "process registry poisoned".into());
            }
        }
        failed.map_or(Ok(()), Err)
    }
    /// Managed execution is Linux-only.
    ///
    /// # Errors
    ///
    /// Always refuses on this platform.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "mirrors the Linux signature, which consumes the permit and services"
    )]
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
        let fd = i32::try_from(fd).map_err(|_| "pidfd out of range")?;
        let handle = Self(unsafe { std::fs::File::from_raw_fd(fd) });
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

/// Signed package/runtime setup for explicitly opted-in kernel qualifications.
#[cfg(all(target_os = "linux", feature = "managed-tree-test"))]
#[path = "managed_test_support.rs"]
pub mod test_support;

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
    fn reviewed_network_hosts(&self) -> std::io::Result<Option<Vec<String>>> {
        Ok(None)
    }

    fn namespace_init_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }
    fn owns_process_group(&self) -> bool {
        false
    }
    fn before_spawn(&self) -> std::io::Result<()>;
    fn child_created(&self);
    fn supervise(&self, child: std::process::Child) -> std::io::Result<std::process::ExitStatus>;
}
#[cfg(target_os = "linux")]
pub(crate) fn verify_runtime(
    manifest: &ManifestView,
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
struct LaunchObserver {
    permit: VerifiedLaunchAuthority,
    binding: ManagedBinding,
    owner: Arc<RuntimeOwner>,
    registry: ProcessRegistry,
    events: Arc<Mutex<VecDeque<ManagedLaunchEvent>>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<bounded_command::WakeFd>,
    terminal: Arc<std::sync::atomic::AtomicBool>,
    spawned: std::sync::atomic::AtomicBool,
    process: Mutex<Option<ProcessIdentity>>,
}
#[cfg(target_os = "linux")]
impl EnrolledProcessObserver for LaunchObserver {
    fn before_spawn(&self) -> std::io::Result<()> {
        self.permit.revalidate().map_err(std::io::Error::other)
    }
    fn child_created(&self) {
        self.spawned
            .store(true, std::sync::atomic::Ordering::Release);
    }
    fn supervise(&self, child: std::process::Child) -> std::io::Result<std::process::ExitStatus> {
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
        // Sleep on exit and on the attempt's cancellation ring; `stop` and
        // `shutdown` ring after storing `cancelled` (and `live`).
        let status = loop {
            match bounded_command::wait_child_or(&mut child, Some(self.wake.fd()), None) {
                Ok(bounded_command::Wakeup::Exited(status)) => break Ok(status),
                // Drain before checking, so a cancel racing the check rings again.
                Ok(bounded_command::Wakeup::Woken | bounded_command::Wakeup::Deadline) => {
                    self.wake.drain();
                }
                Err(error) => {
                    let _ = child.kill();
                    break Err(error);
                }
            }
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire)
                || !self.owner.live.load(std::sync::atomic::Ordering::Acquire)
            {
                // Child owns the original unreaped process; no PID lookup is used.
                if let Err(error) = child.kill()
                    && error.kind() != std::io::ErrorKind::InvalidInput
                {
                    break Err(error);
                }
            }
        };
        let process = self.process.lock().ok().and_then(|p| *p);
        // Operator visibility: a signal (for example a seccomp kill)
        // is otherwise reported only as a failure without a code.
        tracing::info!(agent = %self.binding.agent, ?status, "managed child exited");
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
#[cfg(target_os = "linux")]
impl LaunchObserver {
    /// Revalidate, spawn and supervise the agent on this worker; a failure
    /// becomes a launch event.
    fn run(
        self,
        opts: &crate::run::RunOptions,
        services: &mut crate::services::EnrolledServiceContext,
    ) {
        let result = (|| {
            self.permit.revalidate()?;
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err("launch stopped before spawn".into());
            }
            let manifest = self.owner.package.revalidate(&self.owner.trust)?;
            crate::run::run_enrolled(
                &manifest,
                self.owner.package.payload_root(),
                opts,
                services,
                &self,
            )
            .map_err(|e| e.to_string())
        })();
        if let Err(reason) = result {
            if !self.spawned.load(std::sync::atomic::Ordering::Acquire) {
                self.terminal
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            if let Ok(mut events) = self.events.lock() {
                events.push_back(ManagedLaunchEvent::Failed {
                    binding: self.binding.clone(),
                    process: self.process.lock().ok().and_then(|p| *p),
                    terminal: self.terminal.load(std::sync::atomic::Ordering::Acquire),
                    reason,
                });
            }
        }
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn managed_service_argv(binding: &ManagedBinding) -> Vec<String> {
    vec![
        "--task-runs".into(),
        "--workspace".into(),
        binding.room.clone(),
        "--app".into(),
        binding.app.clone(),
        "--agent-id".into(),
        binding.agent.clone(),
        "--name".into(),
        binding.agent.clone(),
    ]
}

#[cfg(target_os = "linux")]
impl ManagedLauncher {
    /// Start the permitted launch's sandboxed agent on a supervising worker.
    ///
    /// # Errors
    ///
    /// Returns a description for a permit or channel of another runtime, a stopped launch, or
    /// an unavailable worker; spawn failures are reported as events instead.
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
                passthrough_argv: managed_service_argv(&permit.binding),
            },
            ..Default::default()
        };
        let binding = permit.binding.clone();
        let worker_binding = binding.clone();
        let owner = self.owner.clone();
        let registry = self.registry.clone();
        let events = self.events.clone();
        let cancelled = attempt.cancelled.clone();
        let wake = attempt.wake.clone();
        let terminal = attempt.terminal.clone();
        let thread_failure = terminal.clone();
        let thread = std::thread::Builder::new()
            .name("tilo-managed-agent".into())
            .spawn(move || {
                LaunchObserver {
                    permit,
                    binding,
                    owner,
                    registry,
                    events,
                    cancelled,
                    wake,
                    terminal,
                    spawned: false.into(),
                    process: Mutex::new(None),
                }
                .run(&opts, &mut services);
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
    /// Current descriptive facts for this live agent process.
    ///
    /// # Errors
    ///
    /// Returns a description when custody is no longer current or this is not an agent process.
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
    /// Managed process custody is Linux-only.
    ///
    /// # Errors
    ///
    /// Always refuses on this platform.
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
    /// Check that `binding` and `process` are still this process's live custody.
    ///
    /// # Errors
    ///
    /// As [`VerifiedManagedSupervisor::revalidate`].
    pub fn revalidate(
        &self,
        binding: &ManagedBinding,
        process: ProcessIdentity,
    ) -> Result<(), String> {
        self.0.revalidate(binding, process)
    }
}

/// Descriptive durable evidence. Only `VerifiedManagedProcess` confers live custody.
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
        /// Durably bind the exact admitted agent process.
        ///
        /// # Errors
        ///
        /// Returns a description when the binding cannot be made durable.
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

#[cfg(test)]
#[cfg(unix)]
mod stamp_tests {
    use super::TreeStamp;
    #[test]
    fn managed_runtime_stamp_observes_writes_replacements_and_new_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("payload");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/agent"), b"one").unwrap();
        // Filesystem timestamps come from a coarse clock (several ms on
        // Linux), so a rewrite right after the first write can share its
        // mtime and ctime. Date the installed file back as a real payload is.
        std::fs::File::options()
            .write(true)
            .open(root.join("bin/agent"))
            .unwrap()
            .set_modified(
                std::time::SystemTime::now()
                    .checked_sub(std::time::Duration::from_secs(60))
                    .unwrap(),
            )
            .unwrap();
        let first = TreeStamp::of(&[&root]).unwrap();
        assert_eq!(TreeStamp::of(&[&root]), Some(TreeStamp(first.0.clone())));
        // Same-length rewrite: mtime/ctime change.
        std::fs::write(root.join("bin/agent"), b"two").unwrap();
        let rewritten = TreeStamp::of(&[&root]).unwrap();
        assert_ne!(rewritten, first);
        // Atomic replacement: new inode.
        std::fs::write(root.join("bin/agent.new"), b"two").unwrap();
        let with_new = TreeStamp::of(&[&root]).unwrap();
        assert_ne!(with_new, rewritten, "a new entry changes the stamp");
        std::fs::rename(root.join("bin/agent.new"), root.join("bin/agent")).unwrap();
        assert_ne!(TreeStamp::of(&[&root]).unwrap(), with_new);
        // A missing root cannot be stamped (always verify fully).
        assert!(TreeStamp::of(&[&dir.path().join("absent")]).is_none());
    }
}
