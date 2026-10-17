//! Stable pending intent and durable no-launch recovery on the admitted medium.
//! Mount IDs/dev numbers/read claims are reattested separately on every attempt.
use super::*;
use std::os::{fd::AsRawFd, unix::fs::FileExt};
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::setup::home_capture) struct StageBinding {
    pub(in crate::setup::home_capture) declaration: [u8; 32],
    pub(in crate::setup::home_capture) filesystem: [u8; 16],
    pub(in crate::setup::home_capture) uid: u32,
    pub(in crate::setup::home_capture) gid: u32,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Prepared,
    Launching,
    Exported,
    Verifying,
    Verified,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceCheckpoint {
    init_pid: u32,
    start_ticks: u64,
    namespace_dev: u64,
    namespace_ino: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    schema: u32,
    binding: StageBinding,
    attempt: String,
    inode: u64,
    phase: Phase,
    #[serde(default)]
    namespace: Option<NamespaceCheckpoint>,
    #[serde(default)]
    expected: Option<crate::setup::portable_home_media::Expectation>,
    #[serde(default)]
    completed_inode: Option<u64>,
    #[serde(default)]
    expected_applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
    #[serde(default)]
    export_reservation: Option<u64>,
    #[serde(default)]
    completed_setup: Option<CompletedSetup>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CompletedSetup {
    expected: crate::imager_model::PreparedSetupExpectation,
    marker_sha256: [u8; 32],
}
fn name(operation: &str) -> String {
    format!("{operation}.original-output-attempt")
}
fn load(store: &Store) -> io::Result<Option<Attempt>> {
    store.check_original_output_operation(&store.operation)?;
    let directory = storage(&store.records)?;
    let mut file =
        match directory.open_read(OsStr::new(&name(&store.operation)), Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
    if file.metadata()?.len() > 4096 {
        return Err(failure("Original output attempt exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(4097).read_to_end(&mut bytes)?;
    let record: Attempt =
        serde_json::from_slice(&bytes).map_err(|_| failure("Original output attempt invalid"))?;
    if !matches!(record.schema, 1 | 2)
        || record.inode == 0
        || record.binding.declaration == [0; 32]
        || record.binding.filesystem == [0; 16]
        || record.binding.uid == 0
        || uuid::Uuid::parse_str(&record.attempt).is_err()
    {
        return Err(failure("Original output attempt invalid"));
    }
    if record.schema == 1 {
        if record.namespace.as_ref().is_some_and(|facts| {
            facts.init_pid <= 1 || facts.start_ticks == 0 || facts.namespace_ino == 0
        }) || matches!(record.phase, Phase::Prepared) && record.namespace.is_some()
            || matches!(record.phase, Phase::Prepared | Phase::Launching)
                && record.expected.is_some()
            || matches!(record.phase, Phase::Exported | Phase::Verified)
                && record.namespace.is_none()
            || matches!(
                record.phase,
                Phase::Exported | Phase::Verifying | Phase::Verified
            ) && record.expected.is_none()
        {
            return Err(failure("Original output checkpoint shape invalid"));
        }
        if let Some(expected) = &record.expected {
            expected.validate()?;
        }
        if record.expected_applications.is_some()
            && !matches!(
                record.phase,
                Phase::Exported | Phase::Verifying | Phase::Verified
            )
        {
            return Err(failure("Original paired checkpoint shape invalid"));
        }
        if let Some(applications) = &record.expected_applications {
            let home = record
                .expected
                .as_ref()
                .ok_or_else(|| failure("Original paired Home expectation missing"))?;
            OriginalOutputExpectations {
                home,
                applications: Some(applications),
                reserve_bytes: record
                    .export_reservation
                    .ok_or_else(|| failure("Original paired reservation missing"))?,
            }
            .validate()?;
        }
        if let Some(reserve_bytes) = record.export_reservation {
            let home = record
                .expected
                .as_ref()
                .ok_or_else(|| failure("Original export reservation checkpoint invalid"))?;
            OriginalOutputExpectations {
                home,
                applications: record.expected_applications.as_ref(),
                reserve_bytes,
            }
            .validate()?;
        }
    } else {
        validate_applications_attempt(&record)?;
    }
    if record.completed_inode.is_some_and(|inode| inode == 0)
        || record.completed_inode.is_some() && record.phase != Phase::Verified
    {
        return Err(failure("Original completed setup inode checkpoint invalid"));
    }
    if record.completed_setup.as_ref().is_some_and(|completed| {
        record.completed_inode.is_none()
            || !completed.expected.valid()
            || completed.marker_sha256 == [0; 32]
    }) {
        return Err(failure("Completed setup expectation checkpoint invalid"));
    }
    Ok(Some(record))
}
fn validate_applications_attempt(record: &Attempt) -> io::Result<()> {
    if record.schema != 2
        || record.expected.is_some()
        || record.namespace.as_ref().is_some_and(|facts| {
            facts.init_pid <= 1 || facts.start_ticks == 0 || facts.namespace_ino == 0
        })
        || record.phase == Phase::Prepared && record.namespace.is_some()
        || matches!(record.phase, Phase::Prepared | Phase::Launching)
            && (record.expected_applications.is_some() || record.export_reservation.is_some())
        || matches!(record.phase, Phase::Exported | Phase::Verified) && record.namespace.is_none()
    {
        return Err(failure("Applications-only checkpoint shape invalid"));
    }
    if matches!(
        record.phase,
        Phase::Exported | Phase::Verifying | Phase::Verified
    ) {
        let applications = record
            .expected_applications
            .as_ref()
            .ok_or_else(|| failure("Applications-only expectation missing"))?;
        applications.validate()?;
        if applications.sealed_bytes
            > record
                .export_reservation
                .filter(|reserve| *reserve != 0)
                .ok_or_else(|| failure("Applications-only reservation missing"))?
        {
            return Err(failure("Applications-only checkpoint exceeds budget"));
        }
    }
    Ok(())
}
fn check_applications(
    record: &Attempt,
    expected: &crate::setup::flatpak_transfer::portable::Expectation,
    reserve_bytes: u64,
) -> io::Result<()> {
    validate_applications_attempt(record)?;
    expected.validate()?;
    if record.expected_applications.as_ref() != Some(expected)
        || record.export_reservation != Some(reserve_bytes)
        || reserve_bytes == 0
        || expected.sealed_bytes > reserve_bytes
    {
        return Err(failure("Applications-only checkpoint expectation differs"));
    }
    Ok(())
}
fn save(store: &Store, record: &Attempt, mode: PublishMode) -> io::Result<()> {
    store.check_original_output_operation(&store.operation)?;
    if record.schema == 2 {
        validate_applications_attempt(record)?;
    }
    let bytes = serde_json::to_vec(record)?;
    if bytes.len() > 4096 {
        return Err(failure("Original output attempt exceeds bounds"));
    }
    storage(&store.records)?
        .publish(
            OsStr::new(&name(&store.operation)),
            &bytes,
            mode,
            Privacy::OwnerOnly,
        )
        .map_err(|e| e.into_io_error())?;
    Ok(())
}
/// Actual descriptor-relative enumeration, fresh directory cursor; no pathname
/// or reused read offset. A single non-dot entry makes retry refuse.
fn empty(file: &File) -> io::Result<()> {
    only_entries(file, &[])
}
fn only_entries(file: &File, allowed: &[&[u8]]) -> io::Result<()> {
    let fd = unsafe {
        libc::openat(
            file.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = unsafe { libc::fdopendir(fd) };
    if directory.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(io::Error::last_os_error());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Directory(directory);
    loop {
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." && !allowed.contains(&name) {
            return Err(failure(
                "Original output incomplete data requires verified recovery",
            ));
        }
    }
}
fn check_stage(store: &Store, record: &Attempt, output: &File) -> io::Result<()> {
    let metadata = output.metadata()?;
    if !metadata.is_dir()
        || metadata.ino() != record.inode
        || metadata.uid() != record.binding.uid
        || metadata.gid() != record.binding.gid
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(failure("Original output attempt stage differs"));
    }
    store.check_original_output_filesystem(metadata.dev())
}
/// Exact paired ciphertext expectations under one authenticated output budget.
pub(in crate::setup::home_capture) struct OriginalOutputExpectations<'a> {
    pub(in crate::setup::home_capture) home: &'a crate::setup::portable_home_media::Expectation,
    pub(in crate::setup::home_capture) applications:
        Option<&'a crate::setup::flatpak_transfer::portable::Expectation>,
    pub(in crate::setup::home_capture) reserve_bytes: u64,
}
impl OriginalOutputExpectations<'_> {
    pub(in crate::setup::home_capture) fn validate(&self) -> io::Result<()> {
        self.home.validate()?;
        let mut sealed = self.home.sealed_bytes;
        if let Some(applications) = self.applications {
            applications.validate()?;
            if applications.setup_binding != self.home.setup_binding
                || applications.operation == self.home.operation
                || applications.filename == self.home.filename
            {
                return Err(failure(
                    "Original paired package bindings or receipt names differ",
                ));
            }
            sealed = sealed
                .checked_add(applications.sealed_bytes)
                .ok_or_else(|| failure("Original paired output budget overflow"))?;
        }
        if self.reserve_bytes == 0 || sealed > self.reserve_bytes {
            return Err(failure("Original paired output exceeds admitted budget"));
        }
        Ok(())
    }
}
fn check_pair(record: &Attempt, expected: &OriginalOutputExpectations<'_>) -> io::Result<()> {
    if record.schema != 1 {
        return Err(failure(
            "Home checkpoint cannot adopt applications-only intent",
        ));
    }
    expected.validate()?;
    if record
        .expected
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?
        != Some(serde_json::to_vec(expected.home)?)
        || record.expected_applications.as_ref() != expected.applications
        || record.export_reservation != Some(expected.reserve_bytes)
    {
        return Err(failure(
            "Original paired expectations or reservation changed",
        ));
    }
    Ok(())
}

impl Store {
    pub(in crate::setup::home_capture) fn commit_completed_original_setup(
        &self,
        attempt: &str,
        directory: &File,
        expected: &crate::imager_model::PreparedSetupExpectation,
        marker_sha256: [u8; 32],
    ) -> io::Result<()> {
        if !expected.valid() || marker_sha256 == [0; 32] {
            return Err(failure("Completed original setup expectation invalid"));
        }
        let mut record =
            load(self)?.ok_or_else(|| failure("Verified original checkpoint missing"))?;
        let metadata = directory.metadata()?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.completed_inode != Some(metadata.ino())
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o700
            || !metadata.is_dir()
        {
            return Err(failure("Completed setup generation checkpoint differs"));
        }
        self.check_original_output_filesystem(metadata.dev())?;
        let completed = CompletedSetup {
            expected: expected.clone(),
            marker_sha256,
        };
        if let Some(prior) = &record.completed_setup {
            return if prior == &completed {
                Ok(())
            } else {
                Err(failure(
                    "Completed setup expectation conflicts with committed generation",
                ))
            };
        }
        migration_durability::readable_directory(directory)?.sync_all()?;
        record.completed_setup = Some(completed);
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn committed_original_setup(
        &self,
        attempt: &str,
        directory: &File,
    ) -> io::Result<(crate::imager_model::PreparedSetupExpectation, [u8; 32])> {
        let record = load(self)?.ok_or_else(|| failure("Completed setup checkpoint missing"))?;
        let metadata = directory.metadata()?;
        self.check_original_output_filesystem(metadata.dev())?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.completed_inode != Some(metadata.ino())
            || record.namespace.is_none()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o700
            || !metadata.is_dir()
        {
            return Err(failure("Committed setup directory differs"));
        }
        let committed = record
            .completed_setup
            .ok_or_else(|| failure("Completed setup not committed"))?;
        if !committed.expected.valid() || committed.marker_sha256 == [0; 32] {
            return Err(failure("Committed setup binding invalid"));
        }
        Ok((committed.expected, committed.marker_sha256))
    }
    /// A fresh setup generation is separate from the retired Home stage. Its
    /// first inode is durably recorded under the already held operation lease;
    /// an unrecorded existing name is quarantined, never adopted on retry.
    pub(in crate::setup::home_capture) fn completed_original_setup_directory(
        &self,
        parent: &Arc<PinnedDirectory>,
        attempt: &str,
    ) -> io::Result<Arc<PinnedDirectory>> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(failure("Completed original setup requires root custody"));
        }
        parent.validate_destination()?;
        self.check_original_output_filesystem(parent.file.metadata()?.dev())?;
        let mut record =
            load(self)?.ok_or_else(|| failure("Verified original output checkpoint missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.namespace.is_none()
        {
            return Err(failure("Completed original setup checkpoint differs"));
        }
        let name = format!("completed-setup-{attempt}");
        let pin = if let Some(inode) = record.completed_inode {
            let metadata =
                crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
            let pin = parent.open_child(OsStr::new(&name), &metadata)?;
            if pin.file.metadata()?.ino() != inode {
                return Err(failure("Completed original setup inode changed"));
            }
            pin
        } else {
            // Use the same protocol ciphertext bound and bounded completion
            // metadata reserve admitted by the original media plan. No larger
            // arbitrary estimate may fail an already promised allocation.
            let required = crate::setup::prepared_setup::completed_setup_write_reservation()?;
            let available = crate::setup::prepared_setup::completed_setup_free_bytes(&parent.file)?;
            if available < required {
                return Err(failure(
                    "Completed setup generation has insufficient capacity",
                ));
            }
            // create_child is exclusive and never adopts an existing directory.
            let pin = parent.create_child(OsStr::new(&name))?;
            let metadata = pin.file.metadata()?;
            if metadata.uid() != 0 || metadata.gid() != 0 || metadata.mode() & 0o7777 != 0o700 {
                return Err(failure("Fresh completed setup is not root private"));
            }
            migration_durability::readable_directory(&pin.file)?.sync_all()?;
            migration_durability::readable_directory(&parent.file)?.sync_all()?;
            record.completed_inode = Some(metadata.ino());
            save(self, &record, PublishMode::Replace)?;
            pin
        };
        let metadata = pin.file.metadata()?;
        if metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o700
            || metadata.dev() != parent.file.metadata()?.dev()
        {
            return Err(failure("Completed setup generation is not root private"));
        }
        pin.validate_destination()?;
        Ok(pin)
    }
    /// A verified checkpoint alone is not ciphertext authority. The caller
    /// separately supplies actual verifier quiescence and the canonical receipt.
    pub(in crate::setup::home_capture) fn check_verified_original_output(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::portable_home_media::Expectation,
        binding: &StageBinding,
    ) -> io::Result<()> {
        expected.validate()?;
        let record =
            load(self)?.ok_or_else(|| failure("Verified original output checkpoint missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.namespace.is_none()
            || record.expected_applications.is_some()
            || &record.binding != binding
            || record
                .expected
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()?
                != Some(serde_json::to_vec(expected)?)
        {
            return Err(failure("Verified original output checkpoint differs"));
        }
        // Canonical publication retires the worker stage to root only after
        // the actual verifier namespace has died; never adopt another inode.
        let metadata = output.metadata()?;
        if !metadata.is_dir()
            || metadata.ino() != record.inode
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failure("Verified original output retired stage differs"));
        }
        self.check_original_output_filesystem(metadata.dev())
    }

    pub(in crate::setup::home_capture) fn check_verified_original_output_paired(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
        binding: &StageBinding,
    ) -> io::Result<()> {
        expected.validate()?;
        let record =
            load(self)?.ok_or_else(|| failure("Verified original output checkpoint missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.namespace.is_none()
            || &record.binding != binding
            || record
                .expected
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()?
                != Some(serde_json::to_vec(expected.home)?)
        {
            return Err(failure("Verified original output checkpoint differs"));
        }
        check_pair(&record, expected)?;
        // Canonical publication retires the worker stage to root only after
        // the actual verifier namespace has died; never adopt another inode.
        let metadata = output.metadata()?;
        if !metadata.is_dir()
            || metadata.ino() != record.inode
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failure("Verified original output retired stage differs"));
        }
        self.check_original_output_filesystem(metadata.dev())
    }

    pub(in crate::setup::home_capture) fn mark_original_output_exported(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::portable_home_media::Expectation,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        let pair = OriginalOutputExpectations {
            home: expected,
            applications: None,
            reserve_bytes: expected.sealed_bytes,
        };
        self.checkpoint_original_output_exported(attempt, output, &pair, exit, key, work)
    }
    pub(in crate::setup::home_capture) fn mark_original_output_exported_paired(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if expected.applications.is_none() && expected.reserve_bytes == expected.home.sealed_bytes {
            return self.mark_original_output_exported(
                attempt,
                output,
                expected.home,
                exit,
                key,
                work,
            );
        }
        self.checkpoint_original_output_exported(attempt, output, expected, exit, key, work)
    }
    fn checkpoint_original_output_exported(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, crate::setup::home_capture::runtime::PACKAGE) {
            return Err(failure("Original output quiescence checkpoint differs"));
        }

        expected.validate()?;
        let mut record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.schema != 1
            || record.attempt != attempt
            || record.phase != Phase::Launching
            || record.namespace.is_none()
        {
            return Err(failure("Original output export checkpoint differs"));
        }
        check_stage(self, &record, output)?;
        if expected.home.operation != self.operation {
            return Err(failure("Original export operation differs"));
        }
        record.expected = Some(expected.home.clone());
        record.expected_applications = expected.applications.cloned();
        record.export_reservation = Some(expected.reserve_bytes);
        record.phase = Phase::Exported;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn mark_original_output_verifying(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::portable_home_media::Expectation,
    ) -> io::Result<()> {
        let pair = OriginalOutputExpectations {
            home: expected,
            applications: None,
            reserve_bytes: expected.sealed_bytes,
        };
        self.checkpoint_original_output_verifying(attempt, output, &pair)
    }
    pub(in crate::setup::home_capture) fn mark_original_output_verifying_paired(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
    ) -> io::Result<()> {
        if expected.applications.is_none() && expected.reserve_bytes == expected.home.sealed_bytes {
            return self.mark_original_output_verifying(attempt, output, expected.home);
        }
        self.checkpoint_original_output_verifying(attempt, output, expected)
    }
    fn checkpoint_original_output_verifying(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
    ) -> io::Result<()> {
        let mut record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.attempt != attempt || record.phase != Phase::Exported {
            return Err(failure("Original output verification checkpoint differs"));
        }
        check_pair(&record, expected)?;
        check_stage(self, &record, output)?;
        record.namespace = None;
        record.phase = Phase::Verifying;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn mark_original_output_verified(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::portable_home_media::Expectation,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        let pair = OriginalOutputExpectations {
            home: expected,
            applications: None,
            reserve_bytes: expected.sealed_bytes,
        };
        self.checkpoint_original_output_verified(attempt, output, &pair, exit, key, work)
    }
    pub(in crate::setup::home_capture) fn mark_original_output_verified_paired(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if expected.applications.is_none() && expected.reserve_bytes == expected.home.sealed_bytes {
            return self.mark_original_output_verified(
                attempt,
                output,
                expected.home,
                exit,
                key,
                work,
            );
        }
        self.checkpoint_original_output_verified(attempt, output, expected, exit, key, work)
    }
    fn checkpoint_original_output_verified(
        &self,
        attempt: &str,
        output: &File,
        expected: &OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, crate::setup::home_capture::runtime::PACKAGE) {
            return Err(failure("Original output quiescence checkpoint differs"));
        }

        let mut record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verifying
            || record.namespace.is_none()
        {
            return Err(failure("Original output verified checkpoint differs"));
        }
        check_pair(&record, expected)?;
        check_stage(self, &record, output)?;
        record.phase = Phase::Verified;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn mark_original_applications_exported(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        reserve_bytes: u64,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, crate::setup::home_capture::runtime::PACKAGE) {
            return Err(failure("Applications export quiescence differs"));
        }
        expected.validate()?;
        if reserve_bytes == 0 || expected.sealed_bytes > reserve_bytes {
            return Err(failure("Applications export exceeds admitted budget"));
        }
        let mut record = load(self)?.ok_or_else(|| failure("Applications attempt missing"))?;
        if record.schema != 2
            || record.attempt != attempt
            || record.phase != Phase::Launching
            || record.namespace.is_none()
        {
            return Err(failure("Applications export checkpoint differs"));
        }
        check_stage(self, &record, output)?;
        record.expected_applications = Some(expected.clone());
        record.export_reservation = Some(reserve_bytes);
        record.phase = Phase::Exported;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn mark_original_applications_verifying(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        reserve_bytes: u64,
    ) -> io::Result<()> {
        let mut record = load(self)?.ok_or_else(|| failure("Applications attempt missing"))?;
        if record.attempt != attempt || record.phase != Phase::Exported {
            return Err(failure("Applications verification checkpoint differs"));
        }
        check_applications(&record, expected, reserve_bytes)?;
        check_stage(self, &record, output)?;
        record.namespace = None;
        record.phase = Phase::Verifying;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn mark_original_applications_verified(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        reserve_bytes: u64,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, crate::setup::home_capture::runtime::PACKAGE) {
            return Err(failure("Applications verifier quiescence differs"));
        }
        let mut record = load(self)?.ok_or_else(|| failure("Applications attempt missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verifying
            || record.namespace.is_none()
        {
            return Err(failure("Applications verified checkpoint differs"));
        }
        check_applications(&record, expected, reserve_bytes)?;
        check_stage(self, &record, output)?;
        record.phase = Phase::Verified;
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn check_verified_original_applications(
        &self,
        attempt: &str,
        output: &File,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        reserve_bytes: u64,
        binding: &StageBinding,
    ) -> io::Result<()> {
        let record = load(self)?.ok_or_else(|| failure("Applications attempt missing"))?;
        if record.attempt != attempt
            || record.phase != Phase::Verified
            || record.namespace.is_none()
            || &record.binding != binding
        {
            return Err(failure("Applications verified intent differs"));
        }
        check_applications(&record, expected, reserve_bytes)?;
        // Publication can have retired this exact stage to root ownership.
        let metadata = output.metadata()?;
        if !metadata.is_dir()
            || metadata.ino() != record.inode
            || ![0, record.binding.uid].contains(&metadata.uid())
            || metadata.uid() == 0 && metadata.gid() != 0
            || metadata.uid() != 0 && metadata.gid() != record.binding.gid
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failure("Applications verified stage differs"));
        }
        self.check_original_output_filesystem(metadata.dev())
    }
    pub(in crate::setup::home_capture) fn record_original_output_namespace(
        &self,
        attempt: &str,
        output: &File,
        facts: &launcher::owned_service::ServiceCustody,
    ) -> io::Result<()> {
        let mut record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.attempt != attempt
            || !matches!(record.phase, Phase::Launching | Phase::Verifying)
            || record.namespace.is_some()
            || facts.init_pid <= 1
            || facts.start_ticks == 0
            || facts.namespace_ino == 0
        {
            return Err(failure("Original output namespace checkpoint differs"));
        }
        check_stage(self, &record, output)?;
        record.namespace = Some(NamespaceCheckpoint {
            init_pid: facts.init_pid,
            start_ticks: facts.start_ticks,
            namespace_dev: facts.namespace_dev,
            namespace_ino: facts.namespace_ino,
        });
        save(self, &record, PublishMode::Replace)
    }
    pub(in crate::setup::home_capture) fn prepare_original_output_stage(
        &self,
        parent: &Arc<PinnedDirectory>,
        binding: StageBinding,
    ) -> io::Result<(String, File)> {
        self.prepare_original_output_stage_for(parent, binding, 1)
    }
    pub(in crate::setup::home_capture) fn prepare_original_applications_output_stage(
        &self,
        parent: &Arc<PinnedDirectory>,
        binding: StageBinding,
    ) -> io::Result<(String, File)> {
        self.prepare_original_output_stage_for(parent, binding, 2)
    }
    fn prepare_original_output_stage_for(
        &self,
        parent: &Arc<PinnedDirectory>,
        binding: StageBinding,
        schema: u32,
    ) -> io::Result<(String, File)> {
        parent.validate_destination()?;
        self.check_original_output_filesystem(parent.file.metadata()?.dev())?;
        let record = load(self)?;
        let (record, file) = if let Some(record) = record {
            if record.binding != binding || record.schema != schema {
                return Err(failure("Original output durable intent differs"));
            }
            if record.phase != Phase::Prepared {
                return Err(failure(
                    "Original output launched attempt requires namespace recovery",
                ));
            }
            let mut options = cap_primitives::fs::OpenOptions::new();
            use cap_fs_ext::OpenOptionsFollowExt;
            use cap_primitives::fs::OpenOptionsExt;
            options
                .read(true)
                .follow(cap_primitives::fs::FollowSymlinks::No)
                .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK);
            let file = cap_primitives::fs::open(
                &parent.file,
                Path::new(&format!("stage-{}", record.attempt)),
                &options,
            )?;
            let metadata = file.metadata()?;
            if metadata.ino() != record.inode
                || metadata.dev() != parent.file.metadata()?.dev()
                || ![0, binding.uid].contains(&metadata.uid())
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(failure("Original output prepared stage changed"));
            }
            empty(&file)?;
            (record, file)
        } else {
            if binding.declaration == [0; 32] || binding.filesystem == [0; 16] || binding.uid == 0 {
                return Err(failure("Original output stable intent invalid"));
            }
            let attempt = uuid::Uuid::new_v4().to_string();
            let stage = parent.create_child(OsStr::new(&format!("stage-{attempt}")))?;
            let file = migration_durability::readable_directory(&stage.file)?;
            let record = Attempt {
                schema,
                binding,
                attempt,
                inode: file.metadata()?.ino(),
                phase: Phase::Prepared,
                namespace: None,
                expected: None,
                completed_inode: None,
                expected_applications: None,
                export_reservation: None,
                completed_setup: None,
            };
            save(self, &record, PublishMode::IfAbsent)?;
            (record, file)
        };
        // Safe to repeat only before a durable Launching state and only on the
        // held exact empty stage under the exclusive root operation lease.
        if unsafe { libc::fchown(file.as_raw_fd(), record.binding.uid, record.binding.gid) } < 0 {
            return Err(io::Error::last_os_error());
        }
        file.sync_all()?;
        check_stage(self, &record, &file)?;
        Ok((record.attempt, file))
    }
    pub(in crate::setup::home_capture) fn claim_prepared_original_output_intent(
        &self,
        digest: [u8; 32],
        attempt: &str,
        output: &File,
    ) -> io::Result<(File, Vec<u8>)> {
        let record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.attempt != attempt || record.phase != Phase::Prepared {
            return Err(failure("Original output retry is not a prepared attempt"));
        }
        check_stage(self, &record, output)?;
        empty(output)?;
        match self.claim_original_output_intent(digest, attempt) {
            Ok(value) => Ok(value),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    || e.kind() == io::ErrorKind::PermissionDenied =>
            {
                // Prepared record is authority to refresh ephemeral claims;
                // malformed, foreign-operation/attempt intents never qualify.
                let directory = storage(&self.records)?;
                let name = format!("{}.original-output", self.operation);
                let file = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
                if file.metadata()?.len() != 116 {
                    return Err(failure("Original output prior intent invalid"));
                }
                let mut old = [0; 116];
                file.read_exact_at(&mut old, 0)?;
                if &old[..12] != b"TILOOUTPUT1\0"
                    || &old[44..80] != self.operation.as_bytes()
                    || &old[80..116] != attempt.as_bytes()
                {
                    return Err(failure("Original output prior intent differs"));
                }
                let mut bytes = b"TILOOUTPUT1\0".to_vec();
                bytes.extend_from_slice(&digest);
                bytes.extend_from_slice(self.operation.as_bytes());
                bytes.extend_from_slice(attempt.as_bytes());
                directory
                    .publish(
                        OsStr::new(&name),
                        &bytes,
                        PublishMode::Replace,
                        Privacy::OwnerOnly,
                    )
                    .map_err(|e| e.into_io_error())?;
                let file = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
                self.check_original_output_intent(&file, &bytes)?;
                Ok((file, bytes))
            }
            Err(e) => Err(e),
        }
    }
    pub(in crate::setup::home_capture) fn mark_original_output_launching(
        &self,
        attempt: &str,
        output: &File,
    ) -> io::Result<()> {
        let mut record = load(self)?.ok_or_else(|| failure("Original output attempt missing"))?;
        if record.attempt != attempt || record.phase != Phase::Prepared {
            return Err(failure("Original output launch state differs"));
        }
        check_stage(self, &record, output)?;
        record.phase = Phase::Launching;
        save(self, &record, PublishMode::Replace)
    }
}

/// Read-only protected facts, retaining the existing shared operation lease.
/// This is not an output/export authority and must retire inside BootMedium.
pub struct CommittedOriginalSelection {
    _store: Store,
    declaration: crate::setup::prepared_setup::OfflineHomePlan,
    attempt: String,
    pending: crate::imager_model::PreparedSetupExpectation,
    completed: crate::imager_model::PreparedSetupExpectation,
    home: crate::setup::portable_home_media::Expectation,
    applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
    home_inode: u64,
    completed_inode: u64,
    marker_sha256: [u8; 32],
}
impl CommittedOriginalSelection {
    pub fn declaration(&self) -> &crate::setup::prepared_setup::OfflineHomePlan {
        &self.declaration
    }
    pub fn attempt(&self) -> &str {
        &self.attempt
    }
    pub fn pending_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.pending
    }
    pub fn completed_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.completed
    }
    pub fn home(&self) -> &crate::setup::portable_home_media::Expectation {
        &self.home
    }
    pub fn applications(&self) -> Option<&crate::setup::flatpak_transfer::portable::Expectation> {
        self.applications.as_ref()
    }
    pub fn home_inode(&self) -> u64 {
        self.home_inode
    }
    pub fn completed_inode(&self) -> u64 {
        self.completed_inode
    }
    pub fn marker_sha256(&self) -> [u8; 32] {
        self.marker_sha256
    }
}
fn existing_private(parent: &Arc<PinnedDirectory>, name: &str) -> io::Result<Arc<PinnedDirectory>> {
    parent.validate_destination()?;
    let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(name))?;
    if cap_primitives::fs::MetadataExt::uid(&metadata) != 0
        || cap_primitives::fs::MetadataExt::gid(&metadata) != 0
        || cap_primitives::fs::MetadataExt::mode(&metadata) & 0o7777 != 0o700
        || !metadata.is_dir()
    {
        return Err(failure("Readonly committed directory is not root private"));
    }
    let pin = parent.open_child(OsStr::new(name), &metadata)?;
    storage(&pin)?.validate_privacy(Privacy::OwnerOnly)?;
    Ok(pin)
}
fn bounded_private_bytes(
    pin: &Arc<PinnedDirectory>,
    name: &str,
    bound: u64,
) -> io::Result<Vec<u8>> {
    let mut file = storage(pin)?.open_read(OsStr::new(name), Privacy::OwnerOnly)?;
    if file.metadata()?.len() > bound {
        return Err(failure("Readonly committed record exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(bound + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        return Err(failure("Readonly committed record exceeds bounds"));
    }
    Ok(bytes)
}
pub fn review_committed_original_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineHomePlan,
    attempt: &str,
    home: &Arc<PinnedDirectory>,
    completed: &Arc<PinnedDirectory>,
) -> io::Result<CommittedOriginalSelection> {
    use sha2::Digest;
    if rustix::process::geteuid().as_raw() != 0 || uuid::Uuid::parse_str(attempt).is_err() {
        return Err(failure(
            "Readonly committed review requires root and exact attempt",
        ));
    }
    private.validate_destination()?;
    storage(private)?.validate_privacy(Privacy::OwnerOnly)?;
    let parent = existing_private(private, "original-output")?;
    let records = existing_private(&parent, "records")?;
    let snapshots = existing_private(&parent, "snapshots")?;
    let operation = declaration.source().operation_id.clone();
    super::operation(&operation)?;
    let sentinel = storage(&records)?
        .open_read(OsStr::new(&format!("{operation}.lock")), Privacy::OwnerOnly)?;
    let ownership =
        FileLease::try_acquire(sentinel, LeaseMode::Shared).map_err(io::Error::other)?;
    let store = Store {
        records,
        snapshots,
        operation,
        _ownership: ownership,
        records_path: None,
    };
    let record = load(&store)?.ok_or_else(|| failure("Readonly committed checkpoint missing"))?;
    let digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(declaration)?).into();
    let current_home = existing_private(&parent, &format!("stage-{attempt}"))?;
    let current_completed = existing_private(&parent, &format!("completed-setup-{attempt}"))?;
    for (supplied, named) in [(home, &current_home), (completed, &current_completed)] {
        supplied.validate_destination()?;
        let a = supplied.file.metadata()?;
        let b = named.file.metadata()?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
            return Err(failure("Readonly committed stage differs"));
        }
    }
    let pending = crate::setup::media_setup::protected_output::PendingArea::parse(
        &bounded_private_bytes(private, "original-pending-area.json", 4096)?,
    )?;
    let filesystem = crate::setup::home_capture::offline_output::review_filesystem_witness(
        private,
        declaration.media(),
    )?;
    let plan_digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(
        &declaration.authenticated_source_plans()?,
    )?)
    .into();
    if record.schema != 1
        || record.attempt != attempt
        || record.phase != Phase::Verified
        || record.namespace.is_none()
        || record.binding.declaration != digest
        || record.binding.filesystem != filesystem
        || record.binding.uid != declaration.requested_caller().uid()
        || record.binding.gid != declaration.requested_caller().primary_gid()
        || record.inode != home.file.metadata()?.ino()
        || record.completed_inode != Some(completed.file.metadata()?.ino())
        || pending.medium != *declaration.media()
        || pending.operation != declaration.source().operation_id
        || pending.original_uid != record.binding.uid
        || pending.plans_sha256 != plan_digest
    {
        return Err(failure("Readonly committed selection binding differs"));
    }
    store.check_original_output_filesystem(home.file.metadata()?.dev())?;
    let committed = record
        .completed_setup
        .ok_or_else(|| failure("Readonly completed setup missing"))?;
    let marker = bounded_private_bytes(completed, "completion.json", 16 * 1024)?;
    if <[u8; 32]>::from(sha2::Sha256::digest(&marker)) != committed.marker_sha256 {
        return Err(failure("Readonly completed marker differs"));
    }
    let expected = record
        .expected
        .ok_or_else(|| failure("Readonly verified Home missing"))?;
    OriginalOutputExpectations {
        home: &expected,
        applications: record.expected_applications.as_ref(),
        reserve_bytes: declaration.sealed_reservation(),
    }
    .validate()?;
    private.validate_destination()?;
    home.validate_destination()?;
    completed.validate_destination()?;
    Ok(CommittedOriginalSelection {
        _store: store,
        declaration: declaration.clone(),
        attempt: attempt.into(),
        pending: pending.setup,
        completed: committed.expected,
        home: expected,
        applications: record.expected_applications,
        home_inode: record.inode,
        completed_inode: record
            .completed_inode
            .ok_or_else(|| failure("Readonly completed inode missing"))?,
        marker_sha256: committed.marker_sha256,
    })
}

/// Proves only never-started or exact clean Prepared state. Absence of a selected
/// completion is not permission to retry a launched or exported attempt.
pub fn review_uncommitted_original_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineHomePlan,
) -> io::Result<()> {
    review_uncommitted_original_selection_plan(
        private,
        &crate::setup::prepared_setup::OriginalSelectionPlan::Home(declaration.clone()),
    )
}
pub fn review_uncommitted_original_applications_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineApplicationsPlan,
) -> io::Result<()> {
    review_uncommitted_original_selection_plan(
        private,
        &crate::setup::prepared_setup::OriginalSelectionPlan::Applications(declaration.clone()),
    )
}
fn review_uncommitted_original_selection_plan(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OriginalSelectionPlan,
) -> io::Result<()> {
    use sha2::Digest;
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(failure("Uncommitted review requires root"));
    }
    private.validate_destination()?;
    storage(private)?.validate_privacy(Privacy::OwnerOnly)?;
    let pending = crate::setup::media_setup::protected_output::PendingArea::parse(
        &bounded_private_bytes(private, "original-pending-area.json", 4096)?,
    )?;
    let plans: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(
        &declaration.authenticated_source_plans()?,
    )?)
    .into();
    let filesystem = crate::setup::home_capture::offline_output::review_filesystem_witness(
        private,
        declaration.media(),
    )?;
    if pending.medium != *declaration.media()
        || pending.operation != declaration.source().operation_id
        || pending.original_uid != declaration.requested_caller().uid()
        || pending.plans_sha256 != plans
    {
        return Err(failure("Uncommitted pending source binding differs"));
    }
    match crate::setup::nofollow::metadata_nofollow(&private.file, Path::new("original-output")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            private.validate_destination()?;
            return match crate::setup::nofollow::metadata_nofollow(
                &private.file,
                Path::new("original-output"),
            ) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                _ => Err(failure("Uncommitted output absence changed")),
            };
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let parent = existing_private(private, "original-output")?;
    let records = existing_private(&parent, "records")?;
    let snapshots = existing_private(&parent, "snapshots")?;
    let operation = declaration.source().operation_id.clone();
    super::operation(&operation)?;
    let lock_name = format!("{operation}.lock");
    let sentinel = storage(&records)?.open_read(OsStr::new(&lock_name), Privacy::OwnerOnly)?;
    let ownership =
        FileLease::try_acquire(sentinel, LeaseMode::Shared).map_err(io::Error::other)?;
    let store = Store {
        records,
        snapshots,
        operation,
        _ownership: ownership,
        records_path: None,
    };
    store.check_original_output_filesystem(parent.file.metadata()?.dev())?;
    empty(&store.snapshots.file)?;
    let record = load(&store)?;
    if let Some(record) = record {
        let digest = declaration.canonical_digest()?;
        if record.schema
            != if declaration.is_applications_only() {
                2
            } else {
                1
            }
            || record.phase != Phase::Prepared
            || record.namespace.is_some()
            || record.expected.is_some()
            || record.expected_applications.is_some()
            || record.export_reservation.is_some()
            || record.completed_inode.is_some()
            || record.completed_setup.is_some()
            || record.binding.declaration != digest
            || record.binding.filesystem != filesystem
            || record.binding.uid != declaration.requested_caller().uid()
            || record.binding.gid != declaration.requested_caller().primary_gid()
        {
            return Err(failure("Original attempt requires verified recovery"));
        }
        let stage_name = format!("stage-{}", record.attempt);
        let metadata =
            crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(&stage_name))?;
        let stage = parent.open_child(OsStr::new(&stage_name), &metadata)?;
        check_stage(&store, &record, &stage.file)?;
        empty(&stage.file)?;
        let record_name = name(&store.operation);
        let intent_name = format!("{}.original-output", store.operation);
        match storage(&store.records)?.open_read(OsStr::new(&intent_name), Privacy::OwnerOnly) {
            Ok(mut file) => {
                if file.metadata()?.len() != 116 {
                    return Err(failure("Prepared original intent invalid"));
                }
                let mut bytes = [0; 116];
                file.read_exact(&mut bytes)?;
                if &bytes[..12] != b"TILOOUTPUT1\0"
                    || bytes[12..44] == [0; 32]
                    || &bytes[44..80] != store.operation.as_bytes()
                    || &bytes[80..116] != record.attempt.as_bytes()
                {
                    return Err(failure("Prepared original intent binding differs"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        only_entries(
            &store.records.file,
            &[
                lock_name.as_bytes(),
                record_name.as_bytes(),
                intent_name.as_bytes(),
            ],
        )?;
        only_entries(
            &parent.file,
            &[b"records", b"snapshots", stage_name.as_bytes()],
        )?;
        // A Prepared stage intentionally belongs to the original source UID.
        // Its root-private parent and Store lease have already been checked;
        // recheck held/named identity and the exact declared owner separately.
        stage.validate()?;
        check_stage(&store, &record, &stage.file)?;
    } else {
        only_entries(&store.records.file, &[lock_name.as_bytes()])?;
        only_entries(&parent.file, &[b"records", b"snapshots"])?;
    }
    private.validate_destination()?;
    parent.validate_destination()?;
    store.check_original_output_operation(&store.operation)
}

/// Locate the single committed attempt from the authenticated source operation's
/// fixed protected journal. This never scans generations, creates a Store, or
/// grants writer authority. All returned facts come from the existing reviewer.
pub fn lookup_committed_original_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineHomePlan,
) -> io::Result<Option<CommittedOriginalSelection>> {
    // SAFETY: geteuid only observes the native controller's credentials.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(failure("Committed lookup requires root custody"));
    }
    private.validate_destination()?;
    storage(private)?.validate_privacy(Privacy::OwnerOnly)?;
    match crate::setup::nofollow::metadata_nofollow(&private.file, Path::new("original-output")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let parent = existing_private(private, "original-output")?;
    let records = existing_private(&parent, "records")?;
    let snapshots = existing_private(&parent, "snapshots")?;
    let operation = declaration.source().operation_id.clone();
    super::operation(&operation)?;
    let sentinel = storage(&records)?
        .open_read(OsStr::new(&format!("{operation}.lock")), Privacy::OwnerOnly)?;
    let ownership =
        FileLease::try_acquire(sentinel, LeaseMode::Shared).map_err(io::Error::other)?;
    let store = Store {
        records,
        snapshots,
        operation,
        _ownership: ownership,
        records_path: None,
    };
    store.check_original_output_filesystem(parent.file.metadata()?.dev())?;
    let Some(record) = load(&store)? else {
        return Ok(None);
    };
    if record.schema != 1 {
        return Err(failure("Home lookup purpose differs"));
    }
    if record.phase == Phase::Prepared && record.completed_setup.is_none() {
        return Ok(None);
    }
    if record.completed_setup.is_none() {
        return Err(failure(
            "Original attempt has no committed setup recovery proof",
        ));
    }
    // load() rejects malformed phase/checkpoint combinations. The canonical
    // reviewer below separately verifies declaration, namespace, root witnesses,
    // original caller, paired expectations, retired stages and committed marker.
    let home = existing_private(&parent, &format!("stage-{}", record.attempt))?;
    let completed = existing_private(&parent, &format!("completed-setup-{}", record.attempt))?;
    let facts = review_committed_original_selection(
        private,
        declaration,
        &record.attempt,
        &home,
        &completed,
    )?;
    // Keep this first lease until the returned review has acquired its own
    // shared lease, preventing a journal mutation gap between lookup and proof.
    private.validate_destination()?;
    parent.validate_destination()?;
    Ok(Some(facts))
}

pub struct CommittedOriginalApplicationsSelection {
    _store: Store,
    declaration: crate::setup::prepared_setup::OfflineApplicationsPlan,
    attempt: String,
    pending: crate::imager_model::PreparedSetupExpectation,
    completed: crate::imager_model::PreparedSetupExpectation,
    applications: crate::setup::flatpak_transfer::portable::Expectation,
    applications_inode: u64,
    completed_inode: u64,
    marker_sha256: [u8; 32],
}
impl CommittedOriginalApplicationsSelection {
    pub fn declaration(&self) -> &crate::setup::prepared_setup::OfflineApplicationsPlan {
        &self.declaration
    }
    pub fn attempt(&self) -> &str {
        &self.attempt
    }
    pub fn pending_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.pending
    }
    pub fn completed_setup(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.completed
    }
    pub fn applications(&self) -> &crate::setup::flatpak_transfer::portable::Expectation {
        &self.applications
    }
    pub fn applications_inode(&self) -> u64 {
        self.applications_inode
    }
    pub fn completed_inode(&self) -> u64 {
        self.completed_inode
    }
    pub fn marker_sha256(&self) -> [u8; 32] {
        self.marker_sha256
    }
}
pub fn review_committed_original_applications_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineApplicationsPlan,
    attempt: &str,
    applications: &Arc<PinnedDirectory>,
    completed: &Arc<PinnedDirectory>,
) -> io::Result<CommittedOriginalApplicationsSelection> {
    use sha2::Digest;
    if rustix::process::geteuid().as_raw() != 0 || uuid::Uuid::parse_str(attempt).is_err() {
        return Err(failure(
            "Readonly committed review requires root and exact attempt",
        ));
    }
    private.validate_destination()?;
    storage(private)?.validate_privacy(Privacy::OwnerOnly)?;
    let parent = existing_private(private, "original-output")?;
    let records = existing_private(&parent, "records")?;
    let snapshots = existing_private(&parent, "snapshots")?;
    let operation = declaration.source().operation_id.clone();
    super::operation(&operation)?;
    let sentinel = storage(&records)?
        .open_read(OsStr::new(&format!("{operation}.lock")), Privacy::OwnerOnly)?;
    let ownership =
        FileLease::try_acquire(sentinel, LeaseMode::Shared).map_err(io::Error::other)?;
    let store = Store {
        records,
        snapshots,
        operation,
        _ownership: ownership,
        records_path: None,
    };
    let record = load(&store)?.ok_or_else(|| failure("Readonly committed checkpoint missing"))?;
    let digest = declaration.canonical_digest()?;
    let current_applications = existing_private(&parent, &format!("stage-{attempt}"))?;
    let current_completed = existing_private(&parent, &format!("completed-setup-{attempt}"))?;
    for (supplied, named) in [
        (applications, &current_applications),
        (completed, &current_completed),
    ] {
        supplied.validate_destination()?;
        let a = supplied.file.metadata()?;
        let b = named.file.metadata()?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
            return Err(failure("Readonly committed stage differs"));
        }
    }
    let pending = crate::setup::media_setup::protected_output::PendingArea::parse(
        &bounded_private_bytes(private, "original-pending-area.json", 4096)?,
    )?;
    let filesystem = crate::setup::home_capture::offline_output::review_filesystem_witness(
        private,
        declaration.media(),
    )?;
    let plan_digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(
        &declaration.authenticated_source_plans()?,
    )?)
    .into();
    if record.schema != 2
        || record.attempt != attempt
        || record.phase != Phase::Verified
        || record.namespace.is_none()
        || record.binding.declaration != digest
        || record.binding.filesystem != filesystem
        || record.binding.uid != declaration.requested_caller().uid()
        || record.binding.gid != declaration.requested_caller().primary_gid()
        || record.inode != applications.file.metadata()?.ino()
        || record.completed_inode != Some(completed.file.metadata()?.ino())
        || pending.medium != *declaration.media()
        || pending.operation != declaration.source().operation_id
        || pending.original_uid != record.binding.uid
        || pending.plans_sha256 != plan_digest
    {
        return Err(failure("Readonly committed selection binding differs"));
    }
    store.check_original_output_filesystem(applications.file.metadata()?.dev())?;
    let committed = record
        .completed_setup
        .as_ref()
        .ok_or_else(|| failure("Readonly completed setup missing"))?;
    let marker = bounded_private_bytes(completed, "completion.json", 16 * 1024)?;
    if <[u8; 32]>::from(sha2::Sha256::digest(&marker)) != committed.marker_sha256 {
        return Err(failure("Readonly completed marker differs"));
    }
    let expected = record
        .expected_applications
        .clone()
        .ok_or_else(|| failure("Readonly verified applications missing"))?;
    check_applications(&record, &expected, declaration.sealed_reservation())?;
    if expected.operation != declaration.applications_operation()?
        || expected.setup_binding != declaration.setup_binding()
    {
        return Err(failure("Readonly applications expectation binding differs"));
    }
    private.validate_destination()?;
    applications.validate_destination()?;
    completed.validate_destination()?;
    Ok(CommittedOriginalApplicationsSelection {
        _store: store,
        declaration: declaration.clone(),
        attempt: attempt.into(),
        pending: pending.setup,
        completed: committed.expected.clone(),
        applications: expected,
        applications_inode: record.inode,
        completed_inode: record
            .completed_inode
            .ok_or_else(|| failure("Readonly completed inode missing"))?,
        marker_sha256: committed.marker_sha256,
    })
}

pub fn lookup_committed_original_applications_selection(
    private: &Arc<PinnedDirectory>,
    declaration: &crate::setup::prepared_setup::OfflineApplicationsPlan,
) -> io::Result<Option<CommittedOriginalApplicationsSelection>> {
    // SAFETY: geteuid only observes the native controller's credentials.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(failure("Committed lookup requires root custody"));
    }
    private.validate_destination()?;
    storage(private)?.validate_privacy(Privacy::OwnerOnly)?;
    match crate::setup::nofollow::metadata_nofollow(&private.file, Path::new("original-output")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let parent = existing_private(private, "original-output")?;
    let records = existing_private(&parent, "records")?;
    let snapshots = existing_private(&parent, "snapshots")?;
    let operation = declaration.source().operation_id.clone();
    super::operation(&operation)?;
    let sentinel = storage(&records)?
        .open_read(OsStr::new(&format!("{operation}.lock")), Privacy::OwnerOnly)?;
    let ownership =
        FileLease::try_acquire(sentinel, LeaseMode::Shared).map_err(io::Error::other)?;
    let store = Store {
        records,
        snapshots,
        operation,
        _ownership: ownership,
        records_path: None,
    };
    store.check_original_output_filesystem(parent.file.metadata()?.dev())?;
    let Some(record) = load(&store)? else {
        return Ok(None);
    };
    if record.schema != 2 {
        return Err(failure("Applications lookup purpose differs"));
    }
    if record.phase == Phase::Prepared && record.completed_setup.is_none() {
        return Ok(None);
    }
    if record.completed_setup.is_none() {
        return Err(failure(
            "Original attempt has no committed setup recovery proof",
        ));
    }
    // load() rejects malformed phase/checkpoint combinations. The canonical
    // reviewer below separately verifies declaration, namespace, root witnesses,
    // original caller, paired expectations, retired stages and committed marker.
    let applications = existing_private(&parent, &format!("stage-{}", record.attempt))?;
    let completed = existing_private(&parent, &format!("completed-setup-{}", record.attempt))?;
    let facts = review_committed_original_applications_selection(
        private,
        declaration,
        &record.attempt,
        &applications,
        &completed,
    )?;
    // Keep this first lease until the returned review has acquired its own
    // shared lease, preventing a journal mutation gap between lookup and proof.
    private.validate_destination()?;
    parent.validate_destination()?;
    Ok(Some(facts))
}

#[cfg(test)]
mod pair_bounds_tests {
    use super::*;
    use crate::setup::portable_transfer::{Limits, Purpose};
    #[test]
    fn original_output_pair_refuses_receipt_collision_foreign_binding_and_aggregate_shortfall() {
        let operation = uuid::Uuid::new_v4().to_string();
        let applications_operation = uuid::Uuid::new_v4().to_string();
        let limits = Limits {
            plaintext_bytes: 64,
            entries: 1,
            metadata_bytes: 1024,
        };
        let home = crate::setup::portable_home_media::Expectation {
            schema: 1,
            operation: operation.clone(),
            purpose: Purpose::Home,
            filename: format!("home-{operation}.sealed"),
            setup_binding: "paired-fixture".into(),
            sealed_bytes: 32,
            plaintext_bytes: 8,
            entries: 1,
            runtime_sockets: 0,
            ciphertext_sha256: [1; 32],
            limits,
        };
        let mut applications = crate::setup::flatpak_transfer::portable::Expectation {
            schema: 1,
            consumer_protocol: 1,
            operation: applications_operation.clone(),
            purpose: Purpose::FlatpakApplications,
            filename: format!("applications-{applications_operation}.sealed"),
            setup_binding: "paired-fixture".into(),
            sealed_bytes: 48,
            plaintext_bytes: 8,
            entries: 1,
            ciphertext_sha256: [2; 32],
            manifest_sha256: [3; 32],
            limits,
        };
        OriginalOutputExpectations {
            home: &home,
            applications: Some(&applications),
            reserve_bytes: 80,
        }
        .validate()
        .unwrap();
        assert!(
            OriginalOutputExpectations {
                home: &home,
                applications: Some(&applications),
                reserve_bytes: 79
            }
            .validate()
            .is_err()
        );
        applications.setup_binding = "another-setup".into();
        assert!(
            OriginalOutputExpectations {
                home: &home,
                applications: Some(&applications),
                reserve_bytes: 80
            }
            .validate()
            .is_err()
        );
        applications.setup_binding = home.setup_binding.clone();
        applications.operation = home.operation.clone();
        applications.filename = format!("applications-{}.sealed", home.operation);
        applications.validate().unwrap();
        assert!(
            OriginalOutputExpectations {
                home: &home,
                applications: Some(&applications),
                reserve_bytes: 80
            }
            .validate()
            .is_err(),
            "distinct ciphertext names still collide at the canonical receipt name"
        );
    }
}
