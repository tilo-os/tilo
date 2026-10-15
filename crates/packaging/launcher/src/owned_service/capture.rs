//! Host-only admission for a held migration subtree and a signed portable shim.
use super::{QuiescentExit, ServiceAuthority, ServiceCancellation, ServiceLaunch, ServiceLeaseKey};
use crate::run::RunError;
use std::{
    fs::File,
    io::{self, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

mod archive;
pub(in crate::owned_service) mod inventory_scratch;
pub use inventory_scratch::{INVENTORY_SCRATCH_BYTES_ENV, INVENTORY_SCRATCH_PATH};
mod flatpak;
mod openpgp;
pub use flatpak::{
    ORIGINAL_FLATPAK_INVENTORY_PATH, ORIGINAL_FLATPAK_SOURCE_PATH, OriginalFlatpakBinding,
    OriginalFlatpakProjection,
};
pub(crate) mod executable;
mod installation;
pub(crate) mod original;
pub use original::{
    OriginalMountedBinding, OriginalMountedIdentity, OriginalMountedInput,
    VerifiedOriginalMountedRoots, original_mounted_table_sha256, verify_original_mounted_roots,
};
mod installed_output;
pub use installed_output::{
    InstalledCopyOutputAdmission, InstalledCopyOutputBinding, InstalledCopyOutputCustody,
    InstalledCopyOutputFiles,
};
mod root_output;
pub use root_output::{
    OriginalOutputAdmission, OriginalOutputBinding, OriginalOutputCustody, OriginalOutputFiles,
};
mod output;
pub use output::CaptureOutputAdmission;

pub const OUTPUT_PATH: &str = "/run/tilo/migration/output";
pub(crate) const OUTPUT_FD: RawFd = 46;
pub const SOURCE_PATH: &str = "/run/tilo/migration/source";
pub(crate) const SOURCE_FD: RawFd = 44;
pub(crate) const SHIM_FD: RawFd = 45;
/// Sealed archive helper slots stay disjoint from dynamically assigned service slots.
pub(crate) fn archive_reserved_fd(fd: RawFd) -> bool {
    (512..=1152).contains(&fd)
        || openpgp::reserved_fd(fd)
        || fd == inventory_scratch::INVENTORY_SCRATCH_FD
        || original::reserved_fd(fd)
}
const SHIM_FILE: &str = "bin/tilo-launcher";

/// Native host custody, never a serialized app-selected path or capability.
pub struct CaptureAdmission {
    source: File,
    inventory_scratch: Option<inventory_scratch::Scratch>,
    inventory_scratch_custody: Option<Arc<dyn ServiceAuthority>>,
    source_identity: (u64, u64),
    output: Option<(File, (u64, u64))>,
    output_proof: Option<CaptureOutputAdmission>,
    original_output_proof: Option<OriginalOutputAdmission>,
    installed_copy_output_proof: Option<InstalledCopyOutputAdmission>,
    shim: File,
    bwrap: File,
    loader: File,
    archive: Option<archive::Archive>,
    flatpak: Option<flatpak::FlatpakSources>,
    openpgp: Option<openpgp::OpenPgp>,
    runtime: PathBuf,
    runtime_directory: inventory_scratch::ReadRoot,
    installation_directories: Vec<inventory_scratch::ReadRoot>,
    runtime_name: String,
    runtime_root_hash: String,
    installation: Option<package_store::InstalledPackage>,
    publisher: String,
    original_uid: u32,
    original_bootstrap: Option<original::Bootstrap>,
    original_custody: Option<Arc<dyn ServiceAuthority>>,
}
impl CaptureAdmission {
    /// Admit an independently trusted, signed runtime and the broker's held subtree.
    /// # Errors
    /// Refuses missing/substituted shim bytes, non-directory sources and changed credentials.
    pub fn verify(
        runtime: &Path,
        publisher: &str,
        source: File,
        original_uid: u32,
    ) -> io::Result<Self> {
        check_credentials(original_uid)?;
        let runtime_directory = inventory_scratch::ReadRoot::pin(runtime)?;
        let bundle = runtime_bundle::verify_bundle_with(
            runtime,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(publisher.into()),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.into()),
            },
        )
        .map_err(io::Error::other)?;
        let shim = executable::pin(runtime, &bundle, SHIM_FILE)?;
        let bwrap = executable::pin(runtime, &bundle, "bin/bwrap")?;
        executable::check_closure(runtime, &bundle, &bwrap)?;
        let interpreter = crate::loader_preflight::read_interpreter(Path::new(&format!(
            "/proc/self/fd/{}",
            bwrap.as_raw_fd()
        )))?
        .ok_or_else(|| io::Error::other("Portable sandbox has no signed dynamic loader"))?;
        if !crate::sandbox::CANONICAL_LOADER_PATHS.contains(&interpreter.as_str()) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let loader = executable::pin(runtime, &bundle, &format!("lib{interpreter}"))?;
        runtime_directory.check()?;
        let facts = source.metadata()?;
        if !facts.is_dir() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            source,
            inventory_scratch: None,
            inventory_scratch_custody: None,
            source_identity: (facts.dev(), facts.ino()),
            output: None,
            output_proof: None,
            original_output_proof: None,
            installed_copy_output_proof: None,
            shim,
            bwrap,
            loader,
            archive: None,
            flatpak: None,
            openpgp: None,
            runtime: runtime.into(),
            runtime_directory,
            installation_directories: Vec::new(),
            runtime_name: format!("tilo-runtime/{}", bundle.version),
            runtime_root_hash: bundle.root_hash,
            installation: None,
            publisher: publisher.into(),
            original_uid,
            original_bootstrap: None,
            original_custody: None,
        })
    }
    /// Root-service-only original-source reader admission. The trusted broker
    /// supplies facts from its protected SAME-source record after setup/source
    /// reattestation; serialized application `RunOptions` cannot select credentials.
    /// Credentials remain root here and change only inside the completed namespace.
    /// # Errors
    /// Refuses non-root callers, writable/mismatched source, invalid credential
    /// shape, or an untrusted runtime. This is not an offline export receipt.
    pub fn verify_original_source(
        runtime: &Path,
        publisher: &str,
        source: File,
        uid: u32,
        gid: u32,
        groups: &[u32],
        read_claim_digest: [u8; 32],
    ) -> io::Result<Self> {
        let bootstrap = original::Bootstrap::admit(&source, uid, gid, groups, read_claim_digest)?;
        let mut admission = Self::verify(runtime, publisher, source, 0)?;
        admission.original_uid = uid;
        admission.original_bootstrap = Some(bootstrap);
        admission.check()?;
        Ok(admission)
    }
    /// Root-broker-only mounted roots from its protected readonly source context.
    /// Does not alter original caller credentials or admit cross-volume roots.
    /// # Errors
    /// Refuses ordinary capture, writable inputs, overlap or unbounded placements.
    pub fn with_original_mounted_roots(
        mut self,
        inputs: Vec<OriginalMountedInput>,
    ) -> io::Result<Self> {
        let bootstrap = self
            .original_bootstrap
            .take()
            .ok_or(io::ErrorKind::PermissionDenied)?;
        self.original_bootstrap = Some(bootstrap.with_roots(inputs)?);
        self.check()?;
        Ok(self)
    }
    /// Exact protected native root table from the retained original controller.
    /// A metadata-only binding without that controller's independent record proof
    /// cannot authorize a foreign descriptor. Caller credentials stay original.
    /// # Errors
    /// Refuses a missing protected controller, changed claim/table, writable
    /// descriptors, substituted sources or an ordinary caller-made binding.
    pub fn with_original_mounted_roots_bound(
        mut self,
        inputs: Vec<OriginalMountedInput>,
        binding: OriginalMountedBinding,
    ) -> io::Result<Self> {
        check_credentials(0)?;
        let custody = self
            .original_custody
            .as_ref()
            .ok_or(io::ErrorKind::PermissionDenied)?;
        custody.check()?;
        if custody.original_mounted_binding()?.as_ref() != Some(&binding) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let bootstrap = self
            .original_bootstrap
            .take()
            .ok_or(io::ErrorKind::PermissionDenied)?;
        self.original_bootstrap = Some(bootstrap.with_bound_roots(inputs, &binding)?);
        self.check()?;
        Ok(self)
    }
    /// The immutable request binding, available only on original-source admission.
    pub fn original_read_claim_digest(&self) -> Option<[u8; 32]> {
        self.original_bootstrap
            .as_ref()
            .map(original::Bootstrap::read_claim_digest)
    }
    pub(in crate::owned_service) fn worker_uid(&self) -> u32 {
        self.original_uid
    }
    pub(in crate::owned_service) fn prepare_original_workspace(
        &self,
        root: &Path,
    ) -> io::Result<()> {
        if let Some(bootstrap) = &self.original_bootstrap {
            bootstrap.prepare_workspace(root)?;
        }
        Ok(())
    }
    pub(crate) fn original_sandbox_account_identity(&self) -> Option<(u32, u32)> {
        self.original_bootstrap
            .as_ref()
            .map(original::Bootstrap::sandbox_account_identity)
    }
    pub(crate) fn original_baseline_filter(
        &self,
        baseline: &OwnedFd,
    ) -> io::Result<Option<original::FilterLaunch>> {
        self.check_execution()?;
        let lease = self
            .original_bootstrap
            .as_ref()
            .map(|bootstrap| bootstrap.filter_for_launch(baseline))
            .transpose()?
            .flatten();
        // Revalidate original authorization independently of detached clone IDs.
        self.check_execution()?;
        Ok(lease)
    }
    pub(crate) fn splice_original_bootstrap(&self, argv: &mut Vec<String>) -> io::Result<()> {
        if let Some(bootstrap) = &self.original_bootstrap {
            bootstrap.splice(argv)?;
        }
        Ok(())
    }
    /// Admit the fixed signed GNU tar helper and its bounded sealed ELF closure.
    /// # Errors
    /// Refuses missing, changed or unsigned helper/dependency bytes.
    pub fn with_archive_helper(mut self) -> io::Result<Self> {
        self.check()?;
        let archive = archive::Archive::admit(&self)?;
        if let Some(openpgp) = &mut self.openpgp {
            openpgp.deduplicate_archive(&archive)?;
        }
        self.archive = Some(archive);
        Ok(self)
    }
    /// Admit the signed configured OSTree/OpenPGP closure for an original-source capsule.
    /// # Errors
    /// Refuses missing, unsigned or incompatible configured helper bytes.
    pub fn with_original_ostree_helper(mut self) -> io::Result<Self> {
        self.admit_original_ostree_helper()?;
        Ok(self)
    }
    fn admit_original_ostree_helper(&mut self) -> io::Result<()> {
        self.check()?;
        if self.original_bootstrap.is_none() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.openpgp.is_none() {
            let mut helper = openpgp::OpenPgp::admit(self)?;
            if let Some(archive) = &self.archive {
                helper.deduplicate_archive(archive)?;
            }
            self.openpgp = Some(helper);
        }
        Ok(())
    }
    pub(crate) fn splice_archive(&self, argv: &mut Vec<String>, ro_paths: &mut Vec<String>) {
        if let Some(scratch) = &self.inventory_scratch {
            scratch.splice(argv);
        }
        if let Some(archive) = &self.archive {
            archive.splice(argv, ro_paths, self.original_bootstrap.is_some());
        }
        if let Some(flatpak) = &self.flatpak {
            flatpak.splice(argv, ro_paths);
        }
        if let Some(openpgp) = &self.openpgp {
            openpgp.splice(argv, ro_paths);
        }
    }
    /// Grant one owner-private inventory directory on a supported local filesystem.
    /// The broker must independently establish backing; filesystem magic alone
    /// cannot distinguish a physical disk from a RAM-backed loop device.
    /// This native held descriptor cannot be selected by serialized launch options.
    /// # Errors
    /// Refuses non-private ownership, unsupported filesystems, source overlap,
    /// insufficient space, or duplicate attachment.
    pub fn with_inventory_scratch(
        mut self,
        directory: File,
        allocation_bytes: u64,
    ) -> io::Result<Self> {
        self.check()?;
        if self.inventory_scratch.is_some() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.inventory_scratch = Some(inventory_scratch::Scratch::admit(
            directory,
            allocation_bytes,
            self.original_uid,
            &self.source,
        )?);
        self.check()?;
        Ok(self)
    }
    /// Retain the broker's inventory storage lease without granting output writes.
    /// # Errors
    /// Refuses missing scratch, duplicate custody, retired authority or egress.
    pub fn retain_inventory_scratch_custody(
        mut self,
        custody: Arc<dyn ServiceAuthority>,
    ) -> io::Result<Self> {
        self.check()?;
        if self.inventory_scratch.is_none() || self.inventory_scratch_custody.is_some() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        custody.check()?;
        if !custody.network_hosts()?.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.inventory_scratch_custody = Some(custody);
        self.check()?;
        Ok(self)
    }
    /// Add only the admitted scratch path to the shim's no-execute write policy.
    pub(crate) fn splice_inventory_scratch_policy(&self, argv: &mut Vec<String>) -> io::Result<()> {
        if self.inventory_scratch.is_some() {
            let position = argv
                .iter()
                .position(|arg| arg == "__sandbox-exec")
                .ok_or_else(|| io::Error::other("Capture shim unavailable"))?;
            let insertion = position.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
            argv.splice(
                insertion..insertion,
                ["--rw-noexec".into(), INVENTORY_SCRATCH_PATH.into()],
            );
        }
        Ok(())
    }
    /// Add the broker's separately authorized output directory, without executable rights.
    /// # Errors
    /// Refuses inaccessible output or an ancestor/descendant of the selected source.
    pub fn with_output(mut self, output: File) -> io::Result<Self> {
        self.check()?;
        if self.original_bootstrap.is_some() {
            return Err(io::Error::other(
                "Original-source output requires its root transaction admission",
            ));
        }
        writable_directory(&output)?;
        if contains(&self.source, &output)? || contains(&output, &self.source)? {
            return Err(io::Error::other("Migration output overlaps its source"));
        }
        let facts = output.metadata()?;
        self.output = Some((output, (facts.dev(), facts.ino())));
        self.output_proof = None;
        Ok(self)
    }
    /// Add output approved by the live, authenticated root broker.
    /// # Errors
    /// Refuses a dead broker, substituted directories or changed original credentials.
    pub fn with_broker_output(
        mut self,
        output: File,
        proof: CaptureOutputAdmission,
    ) -> io::Result<Self> {
        self.check()?;
        if self.original_bootstrap.is_some() {
            return Err(io::Error::other(
                "Original-source output requires its root transaction admission",
            ));
        }
        proof.check(&self.source, &output, self.original_uid)?;
        let facts = output.metadata()?;
        self.output = Some((output, (facts.dev(), facts.ino())));
        self.output_proof = Some(proof);
        Ok(self)
    }
    /// Attach root-authenticated original output custody.
    /// # Errors
    /// Refuses missing original-source admission, mismatched custody or inaccessible output.
    pub fn with_original_output(mut self, proof: OriginalOutputAdmission) -> io::Result<Self> {
        self.check()?;
        let digest = self.original_read_claim_digest().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original output requires original-source reader bootstrap",
            )
        })?;
        proof.bind_source(&self.source, self.original_uid, digest)?;
        let output = proof.output()?;
        let facts = output.metadata()?;
        self.output = Some((output, (facts.dev(), facts.ino())));
        self.original_output_proof = Some(proof);
        self.check()?;
        Ok(self)
    }
    /// Attach a distinct installed transaction; never impersonate USB custody.
    /// # Errors
    /// Refuses existing output, missing original-source admission or mismatched custody.
    pub fn with_installed_copy_output(
        mut self,
        proof: InstalledCopyOutputAdmission,
    ) -> io::Result<Self> {
        self.check()?;
        if self.output.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let digest = self
            .original_read_claim_digest()
            .ok_or(io::ErrorKind::PermissionDenied)?;
        proof.bind_source(&self.source, self.original_uid, digest)?;
        let output = proof.output()?;
        let facts = output.metadata()?;
        self.output = Some((output, (facts.dev(), facts.ino())));
        self.installed_copy_output_proof = Some(proof);
        self.check()?;
        Ok(self)
    }
    /// Retain the root controller's original read context through namespace
    /// supervision and authenticated endpoint use. This is native-only and
    /// cannot be replaced or removed by the callback receiving this admission.
    /// # Errors
    /// Refuses ordinary captures, duplicate attachment, non-root controllers,
    /// retired authority or authority granting network access.
    pub fn retain_original_source_custody(
        mut self,
        custody: Arc<dyn ServiceAuthority>,
    ) -> io::Result<Self> {
        if self.original_bootstrap.is_none() || self.original_custody.is_some() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        check_credentials(0)?;
        custody.check()?;
        if !custody.network_hosts()?.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.original_custody = Some(custody);
        self.check()?;
        Ok(self)
    }
    pub(super) fn original_custody(&self) -> Option<Arc<dyn ServiceAuthority>> {
        self.original_custody.clone()
    }
    // Builder checks precede attachment. Execution has a stronger boundary:
    // every original reader must carry its root controller's retained context.
    pub(super) fn check_execution(&self) -> io::Result<()> {
        if self.inventory_scratch.is_some() && self.installation.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Inventory scratch execution requires its signed package roots",
            ));
        }
        if self.original_bootstrap.is_some() && self.original_custody.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original-source execution requires retained read custody",
            ));
        }
        self.check()
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if let Some(custody) = &self.inventory_scratch_custody {
            custody.check()?;
            if self.inventory_scratch.is_none() || !custody.network_hosts()?.is_empty() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        if let Some(custody) = &self.original_custody {
            custody.check()?;
            if !custody.network_hosts()?.is_empty() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        if let Some(scratch) = &self.inventory_scratch {
            scratch.check(self.original_uid, &self.source)?;
            self.runtime_directory.refuse_scratch(scratch)?;
            for root in &self.installation_directories {
                root.refuse_scratch(scratch)?;
            }
        }
        if let Some(flatpak) = &self.flatpak {
            flatpak.check()?;
            if let Some(scratch) = &self.inventory_scratch {
                flatpak.check_inventory_scratch(scratch)?;
            }
        }
        if let Some(bootstrap) = &self.original_bootstrap {
            bootstrap.check(&self.source)?;
            if let Some(scratch) = &self.inventory_scratch {
                bootstrap.check_inventory_scratch(scratch)?;
            }
        } else {
            check_credentials(self.original_uid)?;
        }
        let facts = self.source.metadata()?;
        if !facts.is_dir() || (facts.dev(), facts.ino()) != self.source_identity {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if let Some((output, identity)) = &self.output {
            writable_directory(output)?;
            if let Some(proof) = &self.original_output_proof {
                let digest = self
                    .original_read_claim_digest()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::PermissionDenied))?;
                proof.bind_source(&self.source, self.original_uid, digest)?;
            }
            if let Some(proof) = &self.installed_copy_output_proof {
                let digest = self
                    .original_read_claim_digest()
                    .ok_or(io::ErrorKind::PermissionDenied)?;
                proof.bind_source(&self.source, self.original_uid, digest)?;
            }
            if let Some(proof) = &self.output_proof {
                proof.check(&self.source, output, self.original_uid)?;
            }
            let facts = output.metadata()?;
            if (facts.dev(), facts.ino()) != *identity {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        Ok(())
    }
    pub(super) fn configure(&self, options: &mut crate::run::RunOptions) {
        options.bwrap_bin = Some(format!("/proc/self/fd/{}", executable::BWRAP_FD).into());
        options.runtime_root = Some(self.runtime.clone());
        options.require_runtime_publisher = Some(self.publisher.clone());
        options.sandbox_shim_bin = Some(format!("/proc/self/fd/{SHIM_FD}").into());
    }

    /// This independently admitted native runtime is not an installed OS
    /// runtime. Only this nonserializable token supplies its trust; ordinary
    /// `RunOptions` continue to require the host's runtime publisher policy.
    pub(crate) fn resolve_runtime(
        &self,
        manifest: &crate::manifest::ManifestView,
    ) -> Result<(PathBuf, String), RunError> {
        self.check()?;
        if manifest.runtime.as_deref() != Some(self.runtime_name.as_str()) {
            return Err(RunError::InvalidServiceLaunch(
                "native capture runtime declaration changed".into(),
            ));
        }
        let bundle = runtime_bundle::verify_bundle_with(
            &self.runtime,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(self.publisher.clone()),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.into()),
            },
        )
        .map_err(|_| io::Error::from(io::ErrorKind::PermissionDenied))?;
        if bundle.root_hash != self.runtime_root_hash
            || format!("tilo-runtime/{}", bundle.version) != self.runtime_name
        {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        self.check()?;
        Ok((self.runtime.clone(), bundle.version))
    }
    pub(crate) fn descriptor_pairs(&self) -> Vec<(RawFd, RawFd)> {
        let mut pairs = vec![
            (self.source.as_raw_fd(), SOURCE_FD),
            (self.shim.as_raw_fd(), SHIM_FD),
            (self.bwrap.as_raw_fd(), executable::BWRAP_FD),
            (self.loader.as_raw_fd(), executable::LOADER_FD),
        ];
        if let Some(scratch) = &self.inventory_scratch {
            pairs.push(scratch.descriptor_pair());
        }
        if let Some((output, _)) = &self.output {
            pairs.push((output.as_raw_fd(), OUTPUT_FD));
        }
        if let Some(archive) = &self.archive {
            pairs.extend(archive.descriptor_pairs());
        }
        if let Some(flatpak) = &self.flatpak {
            pairs.extend(flatpak.pairs());
        }
        if let Some(openpgp) = &self.openpgp {
            pairs.extend(openpgp.descriptor_pairs());
        }
        if let Some(bootstrap) = &self.original_bootstrap {
            pairs.push((bootstrap.descriptor(), original::BOOTSTRAP_FD));
            pairs.extend(bootstrap.root_pairs());
        }
        pairs
    }
    pub(crate) fn has_output(&self) -> bool {
        self.output.is_some()
    }
}
fn writable_directory(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let result = unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::R_OK | libc::W_OK | libc::X_OK,
            libc::AT_EMPTY_PATH | libc::AT_EACCESS,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn contains(ancestor: &File, child: &File) -> io::Result<bool> {
    let facts = ancestor.metadata()?;
    let wanted = (facts.dev(), facts.ino());
    let mut current = child.try_clone()?;
    for _ in 0..256 {
        let facts = current.metadata()?;
        let identity = (facts.dev(), facts.ino());
        if identity == wanted {
            return Ok(true);
        }
        let raw = unsafe {
            libc::openat(
                current.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let parent = unsafe { File::from_raw_fd(raw) };
        let facts = parent.metadata()?;
        if (facts.dev(), facts.ino()) == identity {
            return Ok(false);
        }
        current = parent;
    }
    Err(io::Error::other("Migration ancestry exceeds bound"))
}
fn sealed_shim(bytes: &[u8]) -> io::Result<File> {
    // Executable bytes are immutable even if the admitted runtime file is later replaced.
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-capture-launcher".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    file.rewind()?;
    file.set_permissions(std::fs::Permissions::from_mode(0o500))?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}
fn snapshot(value: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        value.dev(),
        value.ino(),
        value.len(),
        value.mtime(),
        value.mtime_nsec(),
        value.ctime(),
        value.ctime_nsec(),
    )
}
pub(super) fn check_credentials(original_uid: u32) -> io::Result<()> {
    // Querying with -1 leaves the process filesystem credential unchanged.
    let filesystem_uid = unsafe { libc::setfsuid(u32::MAX) };
    if rustix::process::getuid().as_raw() != original_uid
        || rustix::process::geteuid().as_raw() != original_uid
        || u32::try_from(filesystem_uid).ok() != Some(original_uid)
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
/// Run through the canonical signed service pipeline with one read-only subtree.
/// # Errors
/// Refuses network authority, changed native custody, or unavailable confinement.
pub fn run_capture(
    launch: ServiceLaunch,
    admission: CaptureAdmission,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
) -> Result<QuiescentExit, RunError> {
    run_capture_connected(launch, admission, authority, cancellation, key, None)
}
/// Connect only to the authenticated peer of this exact capture namespace.
/// # Errors
/// As [`run_capture`], including refusal of substituted provider endpoints.
pub fn run_capture_connected(
    launch: ServiceLaunch,
    admission: CaptureAdmission,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<super::ConnectedService>,
) -> Result<QuiescentExit, RunError> {
    admission.check()?;
    if !authority.network_hosts()?.is_empty() {
        return Err(RunError::InvalidServiceLaunch(
            "Migration capture must remain offline".into(),
        ));
    }
    super::run_admitted(
        launch,
        authority,
        cancellation,
        key,
        connected,
        Some(admission),
    )
}

/// Observe canonical teardown even when cancellation/provider failure refuses
/// the successful checkpoint. Cleanup receipt cannot authorize publication.
pub fn run_capture_connected_observed(
    launch: ServiceLaunch,
    admission: CaptureAdmission,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<super::ConnectedService>,
) -> super::SupervisedRun {
    let reviewed = admission.check().and_then(|()| {
        if authority.network_hosts()?.is_empty() {
            Ok(())
        } else {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    });
    if let Err(error) = reviewed {
        return super::SupervisedRun {
            result: Err(error.into()),
            shutdown: None,
        };
    }
    super::run_admitted_observed(
        launch,
        authority,
        cancellation,
        key,
        connected,
        Some(admission),
    )
}

pub(crate) fn splice(argv: &mut Vec<String>, ro_paths: &mut Vec<String>) {
    argv.splice(
        0..0,
        [
            "--dir".into(),
            "/run/tilo/migration".into(),
            "--ro-bind-fd".into(),
            SOURCE_FD.to_string(),
            SOURCE_PATH.into(),
        ],
    );
    ro_paths.push(SOURCE_PATH.into());
}

pub(crate) fn splice_shim_data(argv: &mut Vec<String>) -> io::Result<()> {
    let path = format!("/proc/self/fd/{SHIM_FD}");
    let position = argv
        .windows(3)
        .position(|args| {
            args.first().is_some_and(|arg| arg == "--ro-bind")
                && args.get(1) == Some(&path)
                && args
                    .get(2)
                    .is_some_and(|arg| arg == crate::run::SHIM_MOUNT_PATH)
        })
        .ok_or_else(|| io::Error::other("Capture shim bind is missing"))?;
    // bwrap consumes this held FD itself. A late /proc/self/fd source is invalid
    // once bwrap has mounted the private PID namespace's proc filesystem.
    argv.splice(
        position..position.checked_add(3).ok_or(io::ErrorKind::InvalidInput)?,
        [
            "--perms".into(),
            "0500".into(),
            "--ro-bind-data".into(),
            SHIM_FD.to_string(),
            crate::run::SHIM_MOUNT_PATH.into(),
        ],
    );
    Ok(())
}

pub(crate) fn splice_output(argv: &mut Vec<String>) {
    argv.splice(
        0..0,
        [
            "--dir".into(),
            "/run/tilo/migration".into(),
            "--bind-fd".into(),
            OUTPUT_FD.to_string(),
            OUTPUT_PATH.into(),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::process::CommandExt};

    fn signed_capture_runtime(root: &Path) -> (PathBuf, runtime_bundle::Identity) {
        use std::os::unix::fs::PermissionsExt;
        let launcher = root.join("tilo-launcher");
        std::fs::write(&launcher, b"fixture signed executable bytes").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = root.join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let runtime = root.join("runtime");
        let dependency_output = std::process::Command::new("ldd")
            .args(["/usr/bin/bwrap", "/usr/bin/tar"])
            .output()
            .unwrap();
        assert!(dependency_output.status.success());
        let text = String::from_utf8(dependency_output.stdout).unwrap();
        assert!(!text.contains("not found"));
        let libraries: Vec<PathBuf> = text
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|part| part.starts_with('/') && !part.ends_with(':'))
                    .map(PathBuf::from)
            })
            .collect();
        runtime_bundle::build_bundle_with(
            &runtime,
            "capture-fixture",
            &runtime_bundle::BuildOptions {
                identity: Some(&identity_path),
                extra_bins: &[
                    launcher,
                    PathBuf::from("/usr/bin/bwrap"),
                    PathBuf::from("/usr/bin/tar"),
                ],
                extra_libs: &libraries,
                ..runtime_bundle::BuildOptions::default()
            },
        )
        .unwrap();
        (runtime, identity)
    }

    fn assert_archive_custody(admission: &CaptureAdmission, runtime: &Path) {
        use std::io::{Seek, SeekFrom};
        let mut archive = archive::Archive::admit(admission).unwrap();
        assert!(!archive.libraries.is_empty());
        let original_tar = std::fs::read(runtime.join("bin/tar")).unwrap();
        let mut signed_tar = Vec::new();
        archive.tar.read_to_end(&mut signed_tar).unwrap();
        assert_eq!(signed_tar, original_tar);
        std::fs::write(runtime.join("bin/tar"), b"substituted archive helper").unwrap();
        assert!(archive::Archive::admit(admission).is_err());
        archive.tar.seek(SeekFrom::Start(0)).unwrap();
        let mut held_tar = Vec::new();
        archive.tar.read_to_end(&mut held_tar).unwrap();
        assert_eq!(
            held_tar, original_tar,
            "signed helper custody survives path replacement"
        );
        std::fs::write(runtime.join("bin/tar"), &original_tar).unwrap();
        let missing = runtime.join("bin/tar-missing");
        std::fs::rename(runtime.join("bin/tar"), &missing).unwrap();
        assert!(archive::Archive::admit(admission).is_err());
        std::fs::rename(missing, runtime.join("bin/tar")).unwrap();
        let (name, library) = archive.libraries.first().unwrap();
        let original_library = std::fs::read(runtime.join("lib").join(name)).unwrap();
        std::fs::write(
            runtime.join("lib").join(name),
            b"substituted archive dependency",
        )
        .unwrap();
        assert!(archive::Archive::admit(admission).is_err());
        assert_eq!(
            library.metadata().unwrap().len(),
            original_library.len() as u64
        );
        std::fs::write(runtime.join("lib").join(name), original_library).unwrap();
    }

    #[test]
    fn signed_portable_shim_admission_rejects_wrong_publisher_and_mutated_bytes() {
        use std::io::{Seek, SeekFrom};
        let root = tempfile::tempdir().unwrap();
        let (runtime, identity) = signed_capture_runtime(root.path());
        let selected = root.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let mut admission = CaptureAdmission::verify(
            &runtime,
            &identity.public_key_hex(),
            File::open(&selected).unwrap(),
            uid,
        )
        .unwrap();
        assert_archive_custody(&admission, &runtime);
        let alien = runtime_bundle::Identity::generate();
        // A portable native host authenticates its own runtime. It must not
        // borrow or alter policy for ordinary installed GUI/CLI/services.
        let mut env = tilo_test_env::ScopedEnv::new();
        let trust = root.path().join("host-publishers.txt");
        std::fs::write(&trust, format!("{}\n", alien.public_key_hex())).unwrap();
        env.set(crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV, &trust);
        let mut manifest: crate::manifest::ManifestView =
            toml::from_str(include_str!("../../signal-provider.toml")).unwrap();
        manifest.runtime = Some("tilo-runtime/capture-fixture".into());
        for mode in [
            crate::run::LaunchMode::Gui,
            crate::run::LaunchMode::Cli {
                command: "fixture".into(),
                passthrough_argv: Vec::new(),
            },
            crate::run::LaunchMode::Service {
                passthrough_argv: Vec::new(),
            },
        ] {
            let options = crate::run::RunOptions {
                mode,
                runtime_root: Some(runtime.clone()),
                require_runtime_publisher: Some(identity.public_key_hex()),
                ..Default::default()
            };
            assert!(matches!(
                crate::run::resolve_runtime(&manifest, &options),
                Err(RunError::RuntimeBundleSignature { .. })
            ));
        }
        assert_eq!(
            admission.resolve_runtime(&manifest).unwrap(),
            (runtime.clone(), "capture-fixture".into())
        );
        let admitted_root = admission.runtime_root_hash.clone();
        admission.runtime_root_hash = "0".repeat(64);
        assert!(admission.resolve_runtime(&manifest).is_err());
        admission.runtime_root_hash = admitted_root;
        manifest.runtime = Some("tilo-runtime/substituted-abi".into());
        assert!(admission.resolve_runtime(&manifest).is_err());
        manifest.runtime = Some("tilo-runtime/capture-fixture".into());
        assert!(
            CaptureAdmission::verify(
                &runtime,
                &alien.public_key_hex(),
                File::open(&selected).unwrap(),
                uid
            )
            .is_err()
        );
        std::fs::write(runtime.join(SHIM_FILE), b"replacement signed executable??").unwrap();
        assert!(admission.resolve_runtime(&manifest).is_err());
        assert!(
            CaptureAdmission::verify(
                &runtime,
                &identity.public_key_hex(),
                File::open(&selected).unwrap(),
                uid
            )
            .is_err()
        );
        // An already admitted launch holds immutable authenticated bytes, never the replacement.
        let mut pinned = admission.shim.try_clone().unwrap();
        pinned.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        pinned.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"fixture signed executable bytes");
        let renamed = root.path().join("renamed");
        std::fs::rename(&selected, &renamed).unwrap();
        admission.check().unwrap();
        assert_eq!(
            admission.source.metadata().unwrap().ino(),
            std::fs::metadata(renamed).unwrap().ino()
        );
    }

    #[test]
    fn admitted_launcher_copy_cannot_be_modified_through_its_held_descriptor() {
        let mut file = sealed_shim(b"verified launcher bytes").unwrap();
        assert!(file.write_all(b"substitution").is_err());
        assert!(file.set_len(0).is_err());
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(seals & libc::F_SEAL_WRITE, libc::F_SEAL_WRITE);
    }

    #[test]
    fn original_filesystem_credentials_are_required_without_changing_them() {
        let original = rustix::process::geteuid().as_raw();
        check_credentials(original).unwrap();
        assert!(check_credentials(original.wrapping_add(1)).is_err());
        assert_eq!(rustix::process::geteuid().as_raw(), original);
        check_credentials(original).unwrap();
    }

    #[test]
    fn actual_capture_projection_is_readonly_and_exposes_no_source_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        std::fs::write(selected.join("document"), b"original data").unwrap();
        std::fs::write(root.path().join("outside-secret"), b"unselected").unwrap();
        let held = File::open(&selected).unwrap();
        let host_network = std::fs::read_link("/proc/self/ns/net").unwrap();
        let mut argv = vec![
            "--unshare-all".into(),
            "--die-with-parent".into(),
            "--ro-bind".into(),
            "/usr".into(),
            "/usr".into(),
            "--ro-bind".into(),
            "/lib".into(),
            "/lib".into(),
            "--ro-bind".into(),
            "/lib64".into(),
            "/lib64".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--tmpfs".into(),
            "/tmp".into(),
            "--dir".into(),
            "/run/tilo".into(),
        ];
        splice(&mut argv, &mut Vec::new());
        let script = format!(
            "set -eu; test \"$(cat {SOURCE_PATH}/document)\" = 'original data'; \
            if printf changed > {SOURCE_PATH}/document; then exit 31; fi; \
            test ! -e '{}'; test ! -e {SOURCE_PATH}/../outside-secret; \
            readlink /proc/self/ns/net",
            root.path().join("outside-secret").display()
        );
        argv.extend(["/usr/bin/sh".into(), "-c".into(), script]);
        let mut command = std::process::Command::new("/usr/bin/bwrap");
        command.args(argv);
        let raw = held.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(raw, SOURCE_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "actual confinement unavailable or violated: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_ne!(
            String::from_utf8(output.stdout).unwrap().trim(),
            host_network.to_str().unwrap()
        );
        assert_eq!(
            std::fs::read(selected.join("document")).unwrap(),
            b"original data"
        );
    }
}
