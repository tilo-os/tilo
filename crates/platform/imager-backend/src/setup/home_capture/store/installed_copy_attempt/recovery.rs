//! Recovery inputs stay unverified until a new canonical signed full verifier.
use super::*;
use crate::setup::{migration::PinnedDirectory, nofollow};
use std::{os::fd::AsRawFd, path::Path};
pub(super) fn validate_artifacts(
    f: &InstalledVerifiedArtifactFacts,
    b: &Binding,
    target: &TargetStorage,
    prefix: &str,
) -> io::Result<()> {
    let digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(&(
        f.home.clone(),
        f.applications.clone(),
    ))?)
    .into();
    if f.expectations_digest != digest
        || f.files.is_empty()
        || f.files.len() > 2
        || f.files.len() != usize::from(f.home.is_some()) + usize::from(f.applications.is_some())
    {
        return Err(failure("Installed closed artifact checkpoint invalid"));
    }
    let mut expected = Vec::new();
    let mut total = 0u64;
    if let Some(home) = &f.home {
        home.validate()?;
        if home.operation != b.operation || home.setup_binding != b.setup_binding {
            return Err(failure("Installed Home checkpoint binding differs"));
        }
        total = home.sealed_bytes;
        expected.push((&home.filename, home.sealed_bytes, home.ciphertext_sha256));
    }
    if let Some(apps) = &f.applications {
        apps.validate()?;
        if apps.setup_binding != b.setup_binding {
            return Err(failure("Installed applications checkpoint binding differs"));
        }
        total = total
            .checked_add(apps.sealed_bytes)
            .ok_or_else(|| failure("Installed artifact capacity overflows"))?;
        expected.push((&apps.filename, apps.sealed_bytes, apps.ciphertext_sha256));
    }
    if total == 0 || total > b.reservation {
        return Err(failure("Installed artifact capacity differs"));
    }
    for (name, len, hash) in expected {
        if name.is_empty()
            || Path::new(name).components().count() != 1
            || name == "."
            || name == ".."
        {
            return Err(failure("Installed artifact leaf invalid"));
        }
        let matches: Vec<_> = f
            .files
            .iter()
            .filter(|file| file.name == format!("{prefix}{name}"))
            .collect();
        let [file] = matches.as_slice() else {
            return Err(failure("Installed artifact names missing or ambiguous"));
        };
        if file.dev != target.directory.0
            || file.ino == 0
            || file.len != len
            || file.ciphertext_digest != hash
        {
            return Err(failure(
                "Installed artifact native/digest checkpoint differs",
            ));
        }
    }
    Ok(())
}
pub(super) fn same_payload(
    a: &InstalledVerifiedArtifactFacts,
    b: &InstalledVerifiedArtifactFacts,
) -> io::Result<()> {
    if a.expectations_digest != b.expectations_digest || a.files.len() != b.files.len() {
        return Err(failure("Installed sealing expectations differ"));
    }
    for old in &a.files {
        let leaf = old
            .name
            .strip_prefix("output/")
            .ok_or_else(|| failure("Installed sealing source name differs"))?;
        let new = b
            .files
            .iter()
            .find(|f| f.name == format!("artifacts/{leaf}"))
            .ok_or_else(|| failure("Installed sealing destination missing"))?;
        if (old.dev, old.ino, old.len, old.ciphertext_digest)
            != (new.dev, new.ino, new.len, new.ciphertext_digest)
        {
            return Err(failure("Installed sealing substituted native files"));
        }
    }
    Ok(())
}
fn peer_stopped(peer: &ExecutingPeer) -> io::Result<()> {
    let mut poll = libc::pollfd {
        fd: peer.pidfd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let count = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count != 1
        || poll.revents & libc::POLLIN == 0
        || poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0
    {
        return Err(failure("Installed importer retirement not observed"));
    }
    Ok(())
}
fn require_old_stopped(record: &Record) -> io::Result<()> {
    record
        .export_namespace
        .as_ref()
        .ok_or_else(|| failure("Unknown export launch cannot be recovered"))?
        .custody
        .require_stopped()?;
    record
        .verify_namespace
        .as_ref()
        .ok_or_else(|| failure("Unknown verifier launch cannot be recovered"))?
        .custody
        .require_stopped()?;
    if let Some(n) = &record.recovery_namespace {
        n.custody.require_stopped()?;
    }
    if let Some(receiver) = &record.receiver {
        receiver.custody.require_stopped()?;
    }
    Ok(())
}
impl InstalledCopyAttempt {
    pub fn additional_output_reservation(&self, cancel: &AtomicBool) -> io::Result<u64> {
        self.check(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        match record.phase {
            Phase::Launching => Ok(self.binding.reservation),
            Phase::Verifying | Phase::Sealing | Phase::RecoveryVerifying
                if record.export_namespace.is_some()
                    && record.exported_response_sha256.is_some() =>
            {
                Ok(0)
            }
            _ => Err(failure("Installed output reservation phase differs")),
        }
    }
    pub fn mark_sealing(
        &self,
        export: &InstalledVerifiedExport,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let native = self.scratch(scratch, cancel)?;
        let facts = export.journal_facts(self, scratch, cancel)?;
        validate_artifacts(&facts, &self.binding, &self.target_storage, "output/")?;
        self.require_namespaces_retired()?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.scratch.as_ref() != Some(&native)
            || !matches!(record.phase, Phase::Verifying | Phase::RecoveryVerifying)
        {
            return Err(failure("Installed sealing state differs"));
        }
        if let Some(previous) = &record.sealing {
            if serde_json::to_vec(previous)? != serde_json::to_vec(&facts)? {
                return Err(failure("Recovered sealing payload changed"));
            }
        }
        record.sealing = Some(facts);
        record.verified = None;
        record.phase = Phase::Sealing;
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        export.journal_facts(self, scratch, cancel)?;
        self.check(cancel)
    }
    pub fn mark_receiver_pending(
        &self,
        verified: &InstalledVerifiedArtifacts,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let native = self.scratch(scratch, cancel)?;
        let facts = verified.journal_facts(self, scratch, cancel)?;
        validate_artifacts(&facts, &self.binding, &self.target_storage, "artifacts/")?;
        self.require_namespaces_retired()?;
        let importer = self
            .receiver_peer
            .lock()
            .map_err(|_| failure("Installed importer custody poisoned"))?
            .clone();
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.scratch.as_ref() != Some(&native)
            || record.phase != Phase::Sealing
        {
            return Err(failure("Installed verified pending phase differs"));
        }
        same_payload(
            record
                .sealing
                .as_ref()
                .ok_or_else(|| failure("Installed sealing intent missing"))?,
            &facts,
        )?;
        if let Some(receiver) = &mut record.receiver {
            let peer = importer
                .as_ref()
                .ok_or_else(|| failure("Installed pending importer custody absent"))?;
            if !peer.current() || peer.pid() != receiver.pid || peer.uid() != receiver.uid {
                return Err(failure("Installed pending importer retired or changed"));
            }
            receiver.expectations_digest = Some(facts.expectations_digest);
        }
        record.verified = Some(facts);
        record.phase = Phase::ReceiverPending;
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        verified.journal_facts(self, scratch, cancel)?;
        self.check(cancel)
    }
    pub fn mark_receiver_started(
        &self,
        peer: Arc<ExecutingPeer>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check(cancel)?;
        if peer.uid() != self.binding.target_uid || peer.pid() == self.target.pid() {
            return Err(failure("Installed importer executing peer differs"));
        }
        let receiver = Receiver::actual(&peer, None)?;
        let mut held = self
            .receiver_peer
            .lock()
            .map_err(|_| failure("Installed importer custody poisoned"))?;
        if held.is_some() {
            return Err(failure("Installed importer already held"));
        }
        *held = Some(peer);
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if !matches!(record.phase, Phase::Launching | Phase::RecoveryVerifying)
            || record.receiver.is_some()
            || record.attempt != self.attempt
            || record.binding != self.binding
        {
            return Err(failure("Installed importer launch checkpoint differs"));
        }
        record.receiver = Some(receiver);
        save(&store, &record, PublishMode::Replace)
    }
    pub fn importer(&self) -> io::Result<Arc<ExecutingPeer>> {
        self.receiver_peer
            .lock()
            .map_err(|_| failure("Installed importer custody poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| failure("Installed importer held peer absent"))
    }
    /// Captured while the SAME held executing importer was live; no stopped /proc read.
    pub fn importer_start(&self) -> io::Result<u64> {
        let peer = self.importer()?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        store.installed_copy_snapshots(&self.binding.operation)?;
        let record = load(&store)?.ok_or_else(|| failure("Installed importer intent missing"))?;
        let receiver = record
            .receiver
            .as_ref()
            .ok_or_else(|| failure("Installed importer checkpoint missing"))?;
        if record.attempt != self.attempt
            || record.binding != self.binding
            || record.secret != self.secret_identity
            || receiver.pid != peer.pid()
            || receiver.uid != peer.uid()
        {
            return Err(failure("Installed importer held checkpoint differs"));
        }
        let named = storage(&store.records)?.open_read(
            OsStr::new(&secret_name(&self.binding.operation)),
            Privacy::OwnerOnly,
        )?;
        if secret_facts(&named)? != self.secret_identity
            || secret_facts(&self.secret)? != self.secret_identity
        {
            return Err(failure("Installed importer durable secret differs"));
        }
        Ok(receiver.start)
    }
    /// Actual pidfd death can cancel; it cannot mint successful native adoption.
    pub fn record_receiver_cancelled(&self, _cancel: &AtomicBool) -> io::Result<()> {
        let held = self
            .receiver_peer
            .lock()
            .map_err(|_| failure("Installed importer custody poisoned"))?;
        let peer = held
            .as_ref()
            .ok_or_else(|| failure("Installed importer pidfd missing"))?;
        peer_stopped(peer)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        store.installed_copy_snapshots(&self.binding.operation)?;
        let mut record = load(&store)?.ok_or_else(|| failure("Installed copy intent missing"))?;
        if !matches!(
            record.phase,
            Phase::Launching
                | Phase::Exported
                | Phase::Verifying
                | Phase::Sealing
                | Phase::ReceiverPending
        ) || record.attempt != self.attempt
            || record.binding != self.binding
            || record.secret != self.secret_identity
        {
            return Err(failure("Installed importer cancellation state differs"));
        }
        let named = storage(&store.records)?.open_read(
            OsStr::new(&secret_name(&self.binding.operation)),
            Privacy::OwnerOnly,
        )?;
        if secret_facts(&named)? != self.secret_identity
            || secret_facts(&self.secret)? != self.secret_identity
        {
            return Err(failure("Installed cancellation secret changed"));
        }
        let receiver = record
            .receiver
            .as_ref()
            .ok_or_else(|| failure("Installed importer checkpoint missing"))?;
        if receiver.pid != peer.pid() || receiver.uid != peer.uid() {
            return Err(failure("Installed importer held identity differs"));
        }
        receiver.custody.require_stopped()?;
        record
            .export_namespace
            .as_ref()
            .ok_or_else(|| failure("Unknown export launch cannot retire"))?
            .custody
            .require_stopped()?;
        if matches!(
            record.phase,
            Phase::Verifying | Phase::Sealing | Phase::ReceiverPending
        ) {
            record
                .verify_namespace
                .as_ref()
                .ok_or_else(|| failure("Unknown verifier launch cannot retire"))?
                .custody
                .require_stopped()?;
        }
        if let Some(namespace) = &record.recovery_namespace {
            namespace.custody.require_stopped()?;
        }
        record.phase = Phase::Cancelled;
        save(&store, &record, PublishMode::Replace)
    }
    pub fn record_receiver_complete(
        &self,
        completion: &InstalledCopyCompletion,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let facts = completion.journal_facts(self, cancel)?;
        self.check(cancel)?;
        self.require_namespaces_retired()?;
        let peer = self.importer()?;
        peer_stopped(&peer)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record =
            load(&store)?.ok_or_else(|| failure("Installed copy completion intent missing"))?;
        let receiver = record
            .receiver
            .as_ref()
            .ok_or_else(|| failure("Installed importer checkpoint missing"))?;
        let verified = record
            .verified
            .as_ref()
            .ok_or_else(|| failure("Installed verified checkpoint missing"))?;
        if record.phase != Phase::ReceiverPending
            || record.attempt != self.attempt
            || record.binding != self.binding
            || facts.receiver_pid != receiver.pid
            || facts.receiver_start != receiver.start
            || peer.pid() != receiver.pid
            || facts.expectations_digest != verified.expectations_digest
            || receiver.expectations_digest != Some(facts.expectations_digest)
            || facts.home_sha256 != verified.home.as_ref().map(|e| e.ciphertext_sha256)
            || facts.applications_sha256
                != verified.applications.as_ref().map(|e| e.ciphertext_sha256)
        {
            return Err(failure("Installed actual completion binding differs"));
        }
        receiver.custody.require_stopped()?;
        peer_stopped(&peer)?;
        record.phase = Phase::Complete;
        save(&store, &record, PublishMode::Replace)
    }
}
/// Owns real unverified ciphertext descriptors. It is not a Verified token.
pub struct InstalledCopyRecovery {
    attempt: Arc<InstalledCopyAttempt>,
    stage: Arc<PinnedDirectory>,
    output: Arc<PinnedDirectory>,
    artifacts: Option<Arc<PinnedDirectory>>,
    facts: InstalledVerifiedArtifactFacts,
    files: Vec<(File, String)>,
}
impl InstalledCopyRecovery {
    pub fn attempt(&self) -> Arc<InstalledCopyAttempt> {
        self.attempt.clone()
    }
    pub(in crate::setup::home_capture) fn prior_export_facts(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledVerifiedArtifactFacts> {
        self.check(cancel)?;
        Ok(self.facts.clone())
    }
    pub(in crate::setup::home_capture) fn empty_sealed_parent(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<Option<Arc<PinnedDirectory>>> {
        self.check(cancel)?;
        if let Some(pin) = &self.artifacts {
            let directory = cap_std::fs::Dir::from_std_file(pin.file.try_clone()?);
            if directory.entries()?.next().transpose()?.is_some() {
                return Err(failure("Installed recovered sealed parent is not empty"));
            }
        }
        self.check(cancel)?;
        Ok(self.artifacts.clone())
    }
    pub fn expectations(
        &self,
    ) -> (
        &Option<crate::setup::portable_home_media::Expectation>,
        &Option<crate::setup::flatpak_transfer::portable::Expectation>,
    ) {
        (&self.facts.home, &self.facts.applications)
    }
}
fn opened_child(
    parent: &Arc<PinnedDirectory>,
    name: &str,
    id: (u64, u64),
    owner: (u32, u32),
) -> io::Result<Arc<PinnedDirectory>> {
    parent.validate()?;
    let metadata = nofollow::metadata_nofollow(&parent.file, Path::new(name))?;
    let child = parent.open_child(OsStr::new(name), &metadata)?;
    let held = child.file.metadata()?;
    if !held.is_dir()
        || (held.dev(), held.ino()) != id
        || held.uid() != owner.0
        || held.gid() != owner.1
        || held.mode() & 0o7777 != 0o700
    {
        return Err(failure("Installed recovery directory profile differs"));
    }
    child.validate()?;
    Ok(child)
}
fn artifact_file(parent: &File, leaf: &str) -> io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::OpenOptionsExt;
    let mut options = cap_primitives::fs::OpenOptions::new();
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No)
        .custom_flags(libc::O_NONBLOCK);
    cap_primitives::fs::open(parent, Path::new(leaf), &options)
}
fn measured_file(
    file: &File,
    facts: &crate::setup::home_capture::installed_copy_output::InstalledVerifiedFileFacts,
    owner: (u32, u32),
) -> io::Result<()> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.nlink() != 1
        || m.mode() & 0o7777 != 0o600
        || m.uid() != owner.0
        || m.gid() != owner.1
        || (m.dev(), m.ino(), m.len()) != (facts.dev, facts.ino, facts.len)
    {
        return Err(failure(
            "Installed recovery artifact native profile differs",
        ));
    }
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(failure("Installed recovery artifact is not readonly"));
    }
    let mut hash = sha2::Sha256::new();
    let mut offset = 0u64;
    let mut buffer = [0u8; 65536];
    while offset < m.len() {
        let count = (m.len() - offset).min(buffer.len() as u64) as usize;
        file.read_exact_at(&mut buffer[..count], offset)?;
        hash.update(&buffer[..count]);
        offset += count as u64;
    }
    let mut extra = [0];
    if file.read_at(&mut extra, m.len())? != 0 {
        return Err(failure("Installed recovery artifact grew"));
    }
    let digest: [u8; 32] = hash.finalize().into();
    let after = file.metadata()?;
    if digest != facts.ciphertext_digest
        || (
            after.dev(),
            after.ino(),
            after.len(),
            after.uid(),
            after.gid(),
            after.mode(),
            after.nlink(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) != (
            m.dev(),
            m.ino(),
            m.len(),
            m.uid(),
            m.gid(),
            m.mode(),
            m.nlink(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    {
        return Err(failure("Installed recovery artifact changed"));
    }
    Ok(())
}
fn exact_payload_names(parent: &File, facts: &InstalledVerifiedArtifactFacts) -> io::Result<()> {
    let directory = cap_std::fs::Dir::from_std_file(parent.try_clone()?);
    let expected: Vec<_> = facts
        .files
        .iter()
        .map(|f| {
            f.name
                .strip_prefix("output/")
                .ok_or_else(|| failure("Installed closed file name invalid"))
        })
        .collect::<io::Result<_>>()?;
    let mut count = 0usize;
    for entry in directory.entries()? {
        count += 1;
        let entry = entry?;
        if count > 2
            || !expected
                .iter()
                .any(|name| entry.file_name() == OsStr::new(name))
        {
            return Err(failure("Installed recovery contains unspecified files"));
        }
    }
    Ok(())
}
fn maybe_file(parent: &File, leaf: &str) -> io::Result<Option<File>> {
    match artifact_file(parent, leaf) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
impl InstalledCopyAttempt {
    /// Existing protected checkpoints grant only recovery input, never delivery.
    pub(in crate::setup::home_capture) fn prepare_recovery(
        store: Arc<Mutex<Store>>,
        source: Arc<OriginalCopyReadAdmission>,
        target: Arc<ExecutingPeer>,
        cancel: &AtomicBool,
    ) -> io::Result<InstalledCopyRecovery> {
        let binding = Binding::actual(&source.output_attempt_scope(cancel)?, &target)?;
        let lock = store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record =
            load(&lock)?.ok_or_else(|| failure("Installed recovery intent missing"))?;
        if record.binding != binding
            || !matches!(
                record.phase,
                Phase::Sealing
                    | Phase::ReceiverPending
                    | Phase::Cancelled
                    | Phase::RecoveryPrepared
                    | Phase::RecoveryVerifying
            )
        {
            return Err(failure(
                "Installed recovery protected binding/phase differs",
            ));
        }
        if record.phase == Phase::RecoveryVerifying && record.recovery_namespace.is_none() {
            return Err(failure("Unknown recovery launch cannot be adopted"));
        }
        require_old_stopped(&record)?;
        let snapshots = lock.installed_copy_snapshots(&binding.operation)?;
        let physical = InstalledScratchIdentity::review(
            migration_durability::readable_directory(&snapshots.file)?,
            source.clone(),
            cancel,
        )?;
        let actual = TargetStorage::actual(physical.facts(cancel)?)?;
        let old = &record.target_storage;
        // Mount IDs and namespace change after a legitimate reboot. The native
        // filesystem, selected block geometry and exact Store inode cannot.
        if (
            actual.directory,
            actual.partition_rdev,
            actual.disk_rdev,
            actual.volume_sha256,
        ) != (
            old.directory,
            old.partition_rdev,
            old.disk_rdev,
            old.volume_sha256,
        ) {
            return Err(failure(
                "Installed recovery target physical identity differs",
            ));
        }
        let mut scratch = record
            .scratch
            .clone()
            .ok_or_else(|| failure("Installed recovery scratch missing"))?;
        let stage = opened_child(&snapshots, &scratch.stage_name, scratch.stage, (0, 0))?;
        let output = opened_child(
            &stage,
            "output",
            scratch.output,
            (binding.source_uid, binding.source_gid),
        )?;
        let artifacts = match nofollow::metadata_nofollow(&stage.file, Path::new("artifacts")) {
            Ok(metadata) => {
                let pin = stage.open_child(OsStr::new("artifacts"), &metadata)?;
                pin.validate_destination()?;
                let m = pin.file.metadata()?;
                if !m.is_dir() || m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o700 {
                    return Err(failure(
                        "Installed recovery artifacts parent is not private",
                    ));
                }
                Some(pin)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let facts = record
            .sealing
            .clone()
            .ok_or_else(|| failure("Installed recovery closed sealing intent missing"))?;
        validate_artifacts(&facts, &binding, &actual, "output/")?;
        exact_payload_names(&output.file, &facts)?;
        if let Some(pin) = &artifacts {
            exact_payload_names(&pin.file, &facts)?;
        }
        let mut files = Vec::new();
        for fact in &facts.files {
            let leaf = fact
                .name
                .strip_prefix("output/")
                .ok_or_else(|| failure("Installed recovery canonical output name invalid"))?;
            let out = maybe_file(&output.file, leaf)?;
            let sealed = artifacts
                .as_ref()
                .map(|p| maybe_file(&p.file, leaf))
                .transpose()?
                .flatten();
            let (file, location) = match (out, sealed) {
                (Some(f), None) => (f, "output"),
                (None, Some(f)) => (f, "artifacts"),
                _ => return Err(failure("Installed recovery artifact missing or aliased")),
            };
            let m = file.metadata()?;
            let owner = if m.uid() == 0 {
                (0, 0)
            } else {
                (binding.source_uid, binding.source_gid)
            };
            measured_file(&file, fact, owner)?;
            physical.revalidate(cancel)?;
            if fs_native::snapshot_lease::mount_id(&file)? != actual.mount {
                return Err(failure("Installed recovery file mount differs"));
            }
            files.push((file, format!("{location}/{leaf}")));
        }
        let secret = storage(&lock.records)?.open_read(
            OsStr::new(&secret_name(&binding.operation)),
            Privacy::OwnerOnly,
        )?;
        if secret_facts(&secret)? != record.secret {
            return Err(failure("Installed recovery protected secret differs"));
        }
        physical.revalidate(cancel)?;
        stage.validate_destination()?;
        output.validate()?;
        if Binding::actual(&source.output_attempt_scope(cancel)?, &target)? != binding {
            return Err(failure("Installed recovery admission changed"));
        }
        require_old_stopped(&record)?;
        scratch.mount = actual.mount;
        scratch.namespace = actual.namespace;
        record.target_storage = actual.clone();
        record.scratch = Some(scratch);
        record.read_claim = Some(source.output_read_claim_digest(cancel)?);
        record.phase = Phase::RecoveryPrepared;
        record.verified = None;
        record.receiver = None;
        // Intent precedes any ownership change or rename. Older recovery
        // namespace remains held in the journal until the next explicit launch.
        save(&lock, &record, PublishMode::Replace)?;
        let attempt = record.attempt.clone();
        let secret_identity = record.secret.clone();
        drop(lock);
        let attempt = Arc::new(Self {
            store,
            source,
            target,
            binding,
            attempt,
            secret,
            secret_identity,
            physical,
            target_storage: actual,
            receiver_peer: Mutex::new(None),
        });
        let value = InstalledCopyRecovery {
            attempt,
            stage,
            output,
            artifacts,
            facts,
            files,
        };
        value.check(cancel)?;
        Ok(value)
    }
    pub fn mark_recovery_verifying(
        &self,
        scratch: &InstalledCopyScratch,
        cancel: &AtomicBool,
    ) -> io::Result<[u8; 32]> {
        let native = self.scratch(scratch, cancel)?;
        self.require_namespaces_retired()?;
        let digest = self.source.output_read_claim_digest(cancel)?;
        let store = self
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let mut record =
            load(&store)?.ok_or_else(|| failure("Installed recovery journal missing"))?;
        if record.phase != Phase::RecoveryPrepared
            || record.attempt != self.attempt
            || record.binding != self.binding
            || record.scratch.as_ref() != Some(&native)
        {
            return Err(failure("Installed recovery verifier intent differs"));
        }
        record.phase = Phase::RecoveryVerifying;
        record.recovery_namespace = None;
        record.read_claim = Some(digest);
        save(&store, &record, PublishMode::Replace)?;
        drop(store);
        self.check(cancel)?;
        Ok(digest)
    }
}
impl InstalledCopyRecovery {
    pub fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.attempt.check(cancel)?;
        let (home, applications) = self.expectations();
        if let Some(home) = home {
            home.validate()?;
        }
        if let Some(applications) = applications {
            applications.validate()?;
        }
        self.stage.validate_destination()?;
        self.output.validate()?;
        let store = self
            .attempt
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record =
            load(&store)?.ok_or_else(|| failure("Installed recovery journal disappeared"))?;
        if record.phase != Phase::RecoveryPrepared
            || record.attempt != self.attempt.attempt
            || record.binding != self.attempt.binding
            || serde_json::to_vec(
                record
                    .sealing
                    .as_ref()
                    .ok_or_else(|| failure("Installed recovery closed expectations missing"))?,
            )? != serde_json::to_vec(&self.facts)?
        {
            return Err(failure("Installed recovery intent changed"));
        }
        require_old_stopped(&record)?;
        let expected = record
            .scratch
            .as_ref()
            .ok_or_else(|| failure("Installed recovery scratch missing"))?;
        for (pin, id, owner) in [
            (&self.stage, expected.stage, (0, 0)),
            (
                &self.output,
                expected.output,
                (record.binding.source_uid, record.binding.source_gid),
            ),
        ] {
            let m = pin.file.metadata()?;
            if (m.dev(), m.ino()) != id
                || m.uid() != owner.0
                || m.gid() != owner.1
                || m.mode() & 0o7777 != 0o700
            {
                return Err(failure("Installed recovery directory custody changed"));
            }
            self.attempt.physical.validate_child(
                &migration_durability::readable_directory(&pin.file)?,
                cancel,
            )?;
        }
        if let Some(pin) = &self.artifacts {
            pin.validate_destination()?;
            let m = pin.file.metadata()?;
            if m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7777 != 0o700 {
                return Err(failure("Installed recovery sealed parent changed"));
            }
            self.attempt.physical.validate_child(
                &migration_durability::readable_directory(&pin.file)?,
                cancel,
            )?;
        }
        exact_payload_names(&self.output.file, &self.facts)?;
        if let Some(pin) = &self.artifacts {
            exact_payload_names(&pin.file, &self.facts)?;
        }
        Ok(())
    }
    fn file_check(&self, index: usize, location: &str, cancel: &AtomicBool) -> io::Result<()> {
        self.check(cancel)?;
        let fact = &self.facts.files[index];
        let leaf = fact
            .name
            .strip_prefix("output/")
            .ok_or_else(|| failure("Installed recovery leaf differs"))?;
        let (file, _) = &self.files[index];
        let parent = match location {
            "output" => &self.output,
            "artifacts" => self
                .artifacts
                .as_ref()
                .ok_or_else(|| failure("Installed recovery sealed parent missing"))?,
            _ => return Err(failure("Installed recovery location differs")),
        };
        let opposite = if location == "output" {
            self.artifacts.as_ref()
        } else {
            Some(&self.output)
        };
        if let Some(opposite) = opposite {
            if maybe_file(&opposite.file, leaf)?.is_some() {
                return Err(failure(
                    "Installed recovery ciphertext has duplicate location",
                ));
            }
        }
        let named = artifact_file(&parent.file, leaf)?;
        let m = file.metadata()?;
        let owner = if m.uid() == 0 {
            (0, 0)
        } else {
            (
                self.attempt.binding.source_uid,
                self.attempt.binding.source_gid,
            )
        };
        measured_file(file, fact, owner)?;
        measured_file(&named, fact, owner)?;
        self.attempt.physical.revalidate(cancel)?;
        if fs_native::snapshot_lease::mount_id(file)? != self.attempt.target_storage.mount {
            return Err(failure("Installed recovery file mount changed"));
        }
        self.check(cancel)
    }
    /// Restores only exact journal-bound ciphertext; no duplicate or rollback.
    pub fn restore_for_verifier(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        for index in 0..self.files.len() {
            let location = self.files[index]
                .1
                .split_once('/')
                .ok_or_else(|| failure("Installed recovery location malformed"))?
                .0
                .to_owned();
            self.file_check(index, &location, cancel)?;
            let leaf = self.facts.files[index]
                .name
                .strip_prefix("output/")
                .ok_or_else(|| failure("Installed recovery leaf malformed"))?
                .to_owned();
            // Current source authority, physical geometry, protected secret and
            // exact held/named ciphertext precede EACH native mutation.
            let file = &self.files[index].0;
            if unsafe {
                libc::fchown(
                    file.as_raw_fd(),
                    self.attempt.binding.source_uid,
                    self.attempt.binding.source_gid,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            file.sync_all()?;
            self.file_check(index, &location, cancel)?;
            if location == "artifacts" {
                let source = self
                    .artifacts
                    .as_ref()
                    .ok_or_else(|| failure("Installed sealed parent missing"))?;
                if maybe_file(&self.output.file, &leaf)?.is_some() {
                    return Err(failure("Installed recovery destination already exists"));
                }
                self.file_check(index, "artifacts", cancel)?;
                let name = std::ffi::CString::new(leaf.as_str())
                    .map_err(|_| failure("Installed recovery name invalid"))?;
                if unsafe {
                    libc::syscall(
                        libc::SYS_renameat2,
                        source.file.as_raw_fd(),
                        name.as_ptr(),
                        self.output.file.as_raw_fd(),
                        name.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                self.files[index].1 = format!("output/{leaf}");
                migration_durability::sync_directory(&source.file)?;
                migration_durability::sync_directory(&self.output.file)?;
            }
            self.file_check(index, "output", cancel)?;
            let metadata = self.files[index].0.metadata()?;
            if (metadata.uid(), metadata.gid())
                != (
                    self.attempt.binding.source_uid,
                    self.attempt.binding.source_gid,
                )
            {
                return Err(failure("Installed recovered ciphertext credentials differ"));
            }
        }
        self.check(cancel)
    }
    pub(in crate::setup::home_capture) fn scratch_parts(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<(
        Arc<PinnedDirectory>,
        Arc<PinnedDirectory>,
        File,
        (u64, u64),
        u64,
    )> {
        self.check(cancel)?;
        for index in 0..self.files.len() {
            if !self.files[index].1.starts_with("output/") {
                return Err(failure(
                    "Installed recovery ciphertext has not been restored",
                ));
            }
            self.file_check(index, "output", cancel)?;
            let metadata = self.files[index].0.metadata()?;
            if (metadata.uid(), metadata.gid())
                != (
                    self.attempt.binding.source_uid,
                    self.attempt.binding.source_gid,
                )
            {
                return Err(failure(
                    "Installed verifier ciphertext is not original-readable",
                ));
            }
        }
        let store = self
            .attempt
            .store
            .lock()
            .map_err(|_| failure("Installed Store lock poisoned"))?;
        let record = load(&store)?.ok_or_else(|| failure("Installed recovery journal missing"))?;
        let scratch = record
            .scratch
            .as_ref()
            .ok_or_else(|| failure("Installed recovery scratch missing"))?;
        let parent = store.installed_copy_snapshots(&self.attempt.binding.operation)?;
        Ok((
            parent,
            self.stage.clone(),
            migration_durability::readable_directory(&self.output.file)?,
            scratch.inventory,
            scratch.allowance,
        ))
    }
}
