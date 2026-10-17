//! Exact selected-medium read-only custody before original-source registration.
//! The app never opens a raw device path; only the authenticated root helper does.
use super::{
    home_capture::{MediaReadSession, Rendezvous, RootMediaReadSession},
    migration_handoff,
    migration_intent::MigrationIntent,
};
use crate::imager_model::TargetDrive;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
const ARG: &str = "--tilo-read-selected-media";
const MAX: usize = 1024 * 1024;

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReadMediaPurpose {
    #[default]
    OriginalRegistration,
    BootPending,
}

// Private command intent, never a public receipt or credential container.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootMediaOperation {
    CompleteOriginal,
    ExtractCompleted,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: u32,
    operation: String,
    target: TargetDrive,
    source_plans: Vec<Vec<u8>>,
    #[serde(default)]
    purpose: ReadMediaPurpose,
    rendezvous: Rendezvous,
}
impl Request {
    fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || !uuid::Uuid::parse_str(&self.operation)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.operation)
            || !target_eligible(&self.target, self.purpose)
            || self.target.name.len() > 256
            || self.target.model.len() > 256
            || self
                .target
                .stable_id
                .as_ref()
                .is_none_or(|id| id.is_empty() || id.len() > 256)
            || self.target.partitions.len() > 128
            || (self.purpose == ReadMediaPurpose::OriginalRegistration
                && self.source_plans.is_empty())
            || (self.purpose == ReadMediaPurpose::BootPending && !self.source_plans.is_empty())
            || self.source_plans.len() > 128
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        for bytes in &self.source_plans {
            let plan = migration_handoff::Plan::parse(bytes)?;
            if plan.operation_id != self.operation {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
        Ok(())
    }
}

/// Nonserializable authority. The retained request and live reverse-authenticated
/// helper bind a single read-only descriptor to this exact selected medium.
/// Dropping it closes the socket, lets the helper exit, and reaps that child.
pub struct ReadOnlyMediaGrant {
    purpose: ReadMediaPurpose,
    owned: Option<OwnedMedia>,
    target: TargetDrive,
    source_plans: Vec<Vec<u8>>,
    session: Mutex<Option<MediaReadSession>>,
    inbox_active: AtomicBool,
}
struct OwnedMedia {
    key: String,
    child: Child,
    file: File,
    _request: tempfile::TempDir,
}
impl ReadOnlyMediaGrant {
    fn file(&self) -> io::Result<&File> {
        Ok(&self.owned.as_ref().ok_or(io::ErrorKind::BrokenPipe)?.file)
    }

    pub fn validate(&self) -> io::Result<()> {
        self.session
            .lock()
            .map_err(|_| io::ErrorKind::BrokenPipe)?
            .as_ref()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .validate()?;
        attest_selected_descriptor(&self.target, self.file()?, self.purpose)?;
        if self.purpose == ReadMediaPurpose::OriginalRegistration {
            migration_handoff::validate_offline_source_disks(
                &self.source_plans,
                self.file()?.metadata()?.rdev(),
            )?;
        } else if !self.source_plans.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        super::home_capture::validate_read_only_block_device(self.file()?)
    }
    pub fn clone_descriptor(&self) -> io::Result<File> {
        self.validate()?;
        let file = self.file()?.try_clone()?;
        self.validate()?;
        Ok(file)
    }
}
impl ReadOnlyMediaGrant {
    pub fn complete_original(
        &self,
        secret: zeroize::Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.run_boot_operation(BootMediaOperation::CompleteOriginal, secret, cancel)
    }
    // Kept private to setup until the packaged root helper admits the complete
    // fixed-destination generation adoption; no public action is added here.
    pub fn extract_completed(
        &self,
        secret: zeroize::Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.run_boot_operation(BootMediaOperation::ExtractCompleted, secret, cancel)
    }
    fn run_boot_operation(
        &self,
        operation: BootMediaOperation,
        secret: zeroize::Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.purpose != ReadMediaPurpose::BootPending {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.validate()?;
        let mut held = self.session.lock().map_err(|_| io::ErrorKind::BrokenPipe)?;
        let session = held.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        let result = match operation {
            BootMediaOperation::CompleteOriginal => session.complete_original(secret, cancel),
            BootMediaOperation::ExtractCompleted => session.extract_completed(secret, cancel),
        };
        // EOF retires this exact helper after one job, including errors. Existing
        // child/raw ownership keeps cleanup custody until confirmed retirement.
        *held = None;
        result
    }
}

pub struct VerifiedInboxLease<'a> {
    grant: &'a ReadOnlyMediaGrant,
    area: super::media_setup::protected_output::PendingArea,
    layout: &'a crate::setup::OfflineLayout,
    device: u64,
}
impl VerifiedInboxLease<'_> {
    pub fn validate_declaration(
        &self,
        medium: &crate::media_layout::MediaPartitionBinding,
        setup: &crate::imager_model::PreparedSetupExpectation,
    ) -> io::Result<()> {
        if medium != &self.area.medium || setup != &self.area.setup {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.grant.validate()
    }
    pub fn validate_exact(&self, file: &File) -> io::Result<()> {
        self.grant.validate()?;
        let actual = file.metadata()?;
        // SAFETY: getuid observes only current process credentials.
        let uid = rustix::process::getuid().as_raw();
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&self.layout.source_plans)?).into();
        if !self.grant.inbox_active.load(Ordering::Acquire)
            || !actual.is_dir()
            || actual.uid() != uid
            || actual.gid() != 0
            || actual.mode() & 0o7777 != 0o700
            || actual.ino() != self.area.inode
            || actual.dev() != self.device
            || self.area.original_uid != uid
            || self.area.medium != self.layout.binding
            || self.area.setup != self.layout.setup
            || self.area.plans_sha256 != digest
            || self.layout.source_plans != self.grant.source_plans
            || self.layout.target_number != self.grant.file()?.metadata()?.rdev()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let plan = migration_handoff::Plan::parse(&self.grant.source_plans[0])?;
        if self.area.operation != plan.operation_id || plan.home_uid() != uid {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}
struct InboxActive<'a> {
    grant: &'a ReadOnlyMediaGrant,
    released: bool,
}
impl Drop for InboxActive<'_> {
    fn drop(&mut self) {
        if !self.released {
            // EOF retires the exact root-held mount even on panic/error. It
            // never constitutes a successful pending receipt.
            *self
                .grant
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        self.grant.inbox_active.store(false, Ordering::Release);
    }
}
impl ReadOnlyMediaGrant {
    pub fn with_protected_inbox<T>(
        &self,
        layout: &crate::setup::OfflineLayout,
        cancel: &AtomicBool,
        deliver: impl FnOnce(&super::media_setup::inbox::ProtectedInboxGrant<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.purpose != ReadMediaPurpose::OriginalRegistration {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        super::migration::check_cancel(cancel)?;
        self.validate()?;
        if layout.source_plans != self.source_plans
            || layout.target_number != self.file()?.metadata()?.rdev()
            || layout.binding.target_stable_id != self.target.stable_id.as_deref().unwrap_or("")
            || !layout.setup.valid()
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.inbox_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        let mut active = InboxActive {
            grant: self,
            released: false,
        };
        let (file, bytes) = self
            .session
            .lock()
            .map_err(|_| io::ErrorKind::BrokenPipe)?
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .acquire_inbox(layout)?;
        let area = super::media_setup::protected_output::PendingArea::parse(&bytes)?;
        let lease = VerifiedInboxLease {
            grant: self,
            area,
            layout,
            device: file.metadata()?.dev(),
        };
        let inbox = super::media_setup::inbox::ProtectedInboxGrant::from_admitted(file, &lease)?;
        super::migration::check_cancel(cancel)?;
        let result = deliver(&inbox).and_then(|value| {
            inbox.validate()?;
            super::migration::check_cancel(cancel)?;
            Ok(value)
        });
        self.session
            .lock()
            .map_err(|_| io::ErrorKind::BrokenPipe)?
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .finish_inbox(result.is_ok())?;
        active.released = true;
        self.validate()?;
        super::migration::check_cancel(cancel)?;
        result
    }
}
impl Drop for ReadOnlyMediaGrant {
    fn drop(&mut self) {
        *self
            .session
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Some(owned) = self.owned.take() {
            retain_retired(owned);
        }
    }
}

// The registry, not an event loop or observer, owns abandoned native custody.
// Frontends may observe readiness on their runtime and call finish off-thread.
struct RetiredMedia {
    key: String,
    owned: Mutex<Option<OwnedMedia>>,
}
fn retired() -> &'static Mutex<Vec<std::sync::Arc<RetiredMedia>>> {
    static RETIRED: std::sync::OnceLock<Mutex<Vec<std::sync::Arc<RetiredMedia>>>> =
        std::sync::OnceLock::new();
    RETIRED.get_or_init(Default::default)
}
fn retain_retired(owned: OwnedMedia) {
    retired()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(std::sync::Arc::new(RetiredMedia {
            key: owned.key.clone(),
            owned: Mutex::new(Some(owned)),
        }));
}

/// Conservative nonblocking busy check; cleanup is never inferred from EOF.
pub fn retirement_pending(key: &str) -> bool {
    let entries = match retired().try_lock() {
        Ok(entries) => entries,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return true,
    };
    entries
        .iter()
        .filter(|entry| entry.key == key)
        .any(|entry| match entry.owned.try_lock() {
            Ok(held) => held.is_some(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().is_some(),
            Err(std::sync::TryLockError::WouldBlock) => true,
        })
}

/// Observe the owned child without waiting or releasing its retained resources.
pub fn retirement_ready(key: &str) -> bool {
    let entries = match retired().try_lock() {
        Ok(entries) => entries,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return false,
    };
    entries
        .iter()
        .filter(|entry| entry.key == key)
        .all(|entry| {
            let mut held = match entry.owned.try_lock() {
                Ok(held) => held,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            };
            held.as_mut()
                .is_none_or(|owned| owned.child.try_wait().is_ok_and(|exit| exit.is_some()))
        })
}

/// Blocking final retirement, for an off-thread observer or final host teardown.
/// Wait failures retain the descriptor and private request and keep media busy.
pub fn finish_retirement(key: &str) -> io::Result<()> {
    let entries = retired()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|entry| entry.key == key)
        .cloned()
        .collect::<Vec<_>>();
    let mut failure = None;
    for entry in entries {
        let mut held = entry
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owned) = held.as_mut() {
            match owned.child.wait() {
                Ok(_) => drop(held.take()),
                Err(error) => failure = Some(error),
            }
        }
    }
    retired()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|entry| match entry.owned.try_lock() {
            Ok(held) => held.is_some(),
            Err(_) => true,
        });
    failure.map_or(Ok(()), Err)
}

struct ChildOwner(Option<Child>);
impl ChildOwner {
    fn child(&mut self) -> io::Result<&mut Child> {
        self.0.as_mut().ok_or(io::ErrorKind::BrokenPipe.into())
    }
    fn take(&mut self) -> io::Result<Child> {
        self.0.take().ok_or(io::ErrorKind::BrokenPipe.into())
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        // Kill only this owned child if authorization has not execed root yet.
        // After root exec, socket EOF is its bounded teardown signal.
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Off-thread canonical elevation. No write preparation, unmount, partition
/// change, or migration completion happens during this permission operation.
pub fn admit(
    target: &TargetDrive,
    intent: &MigrationIntent,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    super::migration::check_cancel(cancel)?;
    if !cfg!(feature = "host-media-writer")
        || std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1")
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native read-only media admission unavailable",
        ));
    }
    let plan = intent.for_pending_offline()?;
    let source_plans = crate::setup::source_geometry::original_source_plans(&plan)?;
    let grant = admit_request(
        target,
        ReadMediaPurpose::OriginalRegistration,
        plan.operation_id,
        source_plans,
        cancel,
    )?;
    intent.for_pending_offline()?;
    Ok(grant)
}

/// Apps-only admission derives source scope from its retained selected intent;
/// medium collision and reverse-authenticated descriptor guards remain identical.
pub fn admit_applications(
    target: &TargetDrive,
    intent: &MigrationIntent,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    super::migration::check_cancel(cancel)?;
    if !cfg!(feature = "host-media-writer")
        || std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1")
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native read-only media admission unavailable",
        ));
    }
    let plan = intent.for_pending_applications()?;
    let source_plans = crate::setup::source_geometry::original_source_plans(&plan)?;
    let grant = admit_request(
        target,
        ReadMediaPurpose::OriginalRegistration,
        plan.operation_id.clone(),
        source_plans,
        cancel,
    )?;
    if intent.for_pending_applications()? != plan {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    grant.validate()?;
    Ok(grant)
}

pub fn admit_boot(target: &TargetDrive, cancel: &AtomicBool) -> io::Result<ReadOnlyMediaGrant> {
    if !cfg!(feature = "host-media-writer")
        || std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1")
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    admit_request(
        target,
        ReadMediaPurpose::BootPending,
        uuid::Uuid::new_v4().to_string(),
        Vec::new(),
        cancel,
    )
}
fn admit_request(
    target: &TargetDrive,
    purpose: ReadMediaPurpose,
    operation: String,
    source_plans: Vec<Vec<u8>>,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    super::migration::check_cancel(cancel)?;
    let directory = tempfile::Builder::new()
        .prefix("tilo-media-read-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let (rendezvous, listener) = Rendezvous::create(directory.path())?;
    let request = Request {
        schema: 1,
        operation,
        purpose,
        target: target.clone(),
        source_plans,
        rendezvous,
    };
    request.validate()?;
    let retirement_key = format!(
        "target:{}",
        request
            .target
            .stable_id
            .as_deref()
            .ok_or(io::ErrorKind::InvalidData)?
    );
    let bytes = serde_json::to_vec(&request)?;
    super::home_capture_helper::publish_request_bytes(directory.path(), &bytes, MAX)?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let mut child = ChildOwner(Some(
        Command::new("pkexec")
            .arg(std::env::current_exe()?)
            .arg(ARG)
            .arg(directory.path().join("request.json"))
            .arg(digest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ));
    let result = (|| {
        let mut session = listener.accept_read_media(child.child()?.id(), cancel, || {
            child.child()?.try_wait().map(|exit| exit.is_none())
        })?;
        super::migration::check_cancel(cancel)?;
        let file = session.receive()?;
        let grant = ReadOnlyMediaGrant {
            purpose,
            owned: Some(OwnedMedia {
                key: retirement_key,
                child: child.take()?,
                file,
                _request: directory,
            }),
            target: target.clone(),
            source_plans: request.source_plans,
            session: Mutex::new(Some(session)),
            inbox_active: AtomicBool::new(false),
        };
        grant.validate()?;
        super::migration::check_cancel(cancel)?;
        Ok(grant)
    })();
    // Until the owned grant is assembled, dropping its socket causes the exact
    // helper to finish. The separate child owner below must be reaped on error.
    result
}

/// Registered before the application starts; malformed invocations never fall
/// through to a graphical app. The privileged opener receives no output path.
pub fn maybe_run(args: &[String]) -> Result<Option<i32>, String> {
    let Some(at) = args.iter().position(|arg| arg == ARG) else {
        return Ok(None);
    };
    run_helper(args, at)
        .map(|()| Some(0))
        .map_err(|_| "Selected-medium read permission failed".into())
}
fn run_helper(args: &[String], at: usize) -> io::Result<()> {
    run_helper_with_boot(
        args,
        at,
        |operation, target, raw, caller, secret, cancel, retirement| match operation {
            BootMediaOperation::CompleteOriginal => {
                // Normal installed completion needs the protected receipt from
                // the actual signed export/verifier cohort. The disposable
                // qualifier invokes the private transaction directly and never
                // needs this preexisting receipt to qualify a candidate.
                super::prepared_setup_helper::original_reattest::run(
                    std::iter::empty::<std::ffi::OsString>(),
                    &mut io::sink(),
                )?;
                super::home_capture::offline_output::bootstrap::complete_pending_boot(
                    target, raw, caller, secret, cancel, retirement,
                )
            }
            BootMediaOperation::ExtractCompleted => {
                super::home_capture::offline_output::bootstrap::extract_completed_boot(
                    target, raw, caller, secret, cancel, retirement,
                )
            }
        },
    )
}
// Carrier callback owns the full transaction and must return only after
// authentication, read mount retirement, reviewed export/verification,
// reseal/commit and confirmed owned unmount. Unsupported is never success.
pub(crate) fn run_helper_with_boot(
    args: &[String],
    at: usize,
    complete: impl FnOnce(
        BootMediaOperation,
        &TargetDrive,
        &File,
        u32,
        zeroize::Zeroizing<Vec<u8>>,
        &AtomicBool,
        &mut RootMediaReadSession,
    ) -> io::Result<()>,
) -> io::Result<()> {
    if args.get(at + 3).is_some() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let path = Path::new(args.get(at + 1).ok_or(io::ErrorKind::InvalidInput)?);
    let digest = args.get(at + 2).ok_or(io::ErrorKind::InvalidInput)?;
    // SAFETY: geteuid has no arguments and observes only current credentials.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let (bytes, caller) = crate::write::helper::read_linux_helper_request(path)?;
    if caller == 0
        || bytes.len() > MAX
        || digest.len() != 64
        || hex::encode(Sha256::digest(&bytes)) != *digest
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let mut session = RootMediaReadSession::connect(
        &request.rendezvous,
        path.parent().ok_or(io::ErrorKind::InvalidInput)?,
        caller,
    )?;
    // Authenticate the exact caller before opening the selected raw device.
    validate_selected_target(&request.target, request.purpose)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(request.target.dev_path())?;
    attest_selected_descriptor(&request.target, &file, request.purpose)?;
    if request.purpose == ReadMediaPurpose::OriginalRegistration {
        migration_handoff::validate_offline_source_disks(
            &request.source_plans,
            file.metadata()?.rdev(),
        )?;
    }
    super::home_capture::validate_read_only_block_device(&file)?;
    session.send(&file)?;
    if request.purpose == ReadMediaPurpose::BootPending {
        let Some((operation, secret)) = session.next_boot_operation()? else {
            return Ok(());
        };
        let cancel = AtomicBool::new(false);
        let result = complete(
            operation,
            &request.target,
            &file,
            caller,
            secret,
            &cancel,
            &mut session,
        );
        session.finish_original_completion(&result)?;
        return result;
    }
    while let Some(layout) = session.next_inbox_request()? {
        if layout.source_plans != request.source_plans
            || layout.target_number != file.metadata()?.rdev()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        validate_selected_target(&request.target, request.purpose)?;
        migration_handoff::validate_offline_source_disks(
            &request.source_plans,
            layout.target_number,
        )?;
        let cancel = AtomicBool::new(false);
        let result = crate::write::with_locked_media_root(&request.target, || {
            super::media_setup::protected_output::with_reviewed_inbox(
                &request.target,
                &file,
                &layout,
                caller,
                &cancel,
                |inbox, receipt| {
                    session.send_inbox(&inbox, &receipt)?;
                    session.await_inbox_completion()
                },
            )
        });
        session.release_inbox(result.is_ok())?;
        result?;
    }
    Ok(())
}

fn target_eligible(selected: &TargetDrive, purpose: ReadMediaPurpose) -> bool {
    match purpose {
        ReadMediaPurpose::OriginalRegistration => selected.is_media_target(),
        // The USB/SD medium may now host the booted system. That permits only
        // read custody, never destructive eligibility or source admission.
        ReadMediaPurpose::BootPending => {
            selected.kind.is_removable_class()
                && (selected.removable
                    || selected.tran.as_deref().is_some_and(|value| {
                        matches!(value.to_ascii_lowercase().as_str(), "usb" | "sd" | "mmc")
                    }))
        }
    }
}
fn validate_selected_target(selected: &TargetDrive, purpose: ReadMediaPurpose) -> io::Result<()> {
    let actual = crate::probe::probe()
        .and_then(|drives| drives.into_iter().find(|drive| drive.name == selected.name))
        .ok_or(io::ErrorKind::NotFound)?;
    if !target_eligible(&actual, purpose)
        || actual.stable_id != selected.stable_id
        || actual.model != selected.model
        || actual.size != selected.size
        || actual.removable != selected.removable
        || actual.tran != selected.tran
        || actual.kind != selected.kind
        || (purpose == ReadMediaPurpose::OriginalRegistration && actual.is_system)
        || (purpose == ReadMediaPurpose::BootPending
            && (actual.is_system != selected.is_system
                || actual.partitions.len() != selected.partitions.len()
                || actual
                    .partitions
                    .iter()
                    .zip(&selected.partitions)
                    .any(|(a, b)| {
                        a.name != b.name
                            || a.size != b.size
                            || a.fstype != b.fstype
                            || a.label != b.label
                    })))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if purpose == ReadMediaPurpose::OriginalRegistration {
        crate::write::revalidate_target(selected)?;
    }
    Ok(())
}
pub fn attest_boot_descriptor(target: &TargetDrive, file: &File) -> io::Result<()> {
    attest_selected_descriptor(target, file, ReadMediaPurpose::BootPending)
}
fn attest_selected_descriptor(
    selected: &TargetDrive,
    file: &File,
    purpose: ReadMediaPurpose,
) -> io::Result<()> {
    if purpose == ReadMediaPurpose::OriginalRegistration {
        return crate::write::attest_target_descriptor(selected, file);
    }
    use std::os::unix::fs::FileTypeExt;
    validate_selected_target(selected, purpose)?;
    // Root opened O_RDONLY before any marker parse; neither a caller-supplied
    // path nor a regular file can substitute for this exact rescanned device.
    super::home_capture::validate_read_only_block_device(file)?;
    let held = file.metadata()?;
    let current = std::fs::symlink_metadata(selected.dev_path())?;
    if !held.file_type().is_block_device()
        || !current.file_type().is_block_device()
        || (held.rdev(), held.ino(), held.dev()) != (current.rdev(), current.ino(), current.dev())
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    validate_selected_target(selected, purpose)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_grant_retains_native_custody_until_confirmed_child_exit() {
        use std::{io::Write, os::unix::net::UnixStream, time::Instant};
        let request = tempfile::tempdir().unwrap();
        let root = request.path().to_path_buf();
        let file = File::create(root.join("held-medium")).unwrap();
        let (mut release, input) = UnixStream::pair().unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "read -r release"])
            .stdin(Stdio::from(File::from(std::os::fd::OwnedFd::from(input))))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let key = format!("target:{}", uuid::Uuid::new_v4());
        let grant = ReadOnlyMediaGrant {
            purpose: ReadMediaPurpose::OriginalRegistration,
            owned: Some(OwnedMedia {
                key: key.clone(),
                child,
                file,
                _request: request,
            }),
            target: TargetDrive {
                name: "fixture-only".into(),
                model: "public test medium".into(),
                stable_id: Some("fixture-only".into()),
                size: 4096,
                kind: crate::imager_model::TargetKind::Usb,
                tran: Some("usb".into()),
                removable: true,
                is_system: false,
                partitions: Vec::new(),
            },
            source_plans: Vec::new(),
            session: Mutex::new(None),
            inbox_active: AtomicBool::new(false),
        };
        let before = Instant::now();
        drop(grant);
        assert!(before.elapsed() < std::time::Duration::from_millis(500));
        assert!(retirement_pending(&key));
        assert!(!retirement_ready(&key));
        assert!(!retirement_pending("unrelated-medium"));
        assert!(root.join("held-medium").exists());
        release.write_all(b"release\n").unwrap();
        finish_retirement(&key).unwrap();
        assert!(!retirement_pending(&key));
        assert!(!root.exists());
    }

    #[test]
    fn read_request_rejects_ambient_paths_and_changed_operation_after_private_publication() {
        let private = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let home = private.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let plan = migration_handoff::capture_fixture(
            &home,
            &operation,
            migration_handoff::Categories {
                home: true,
                ..Default::default()
            },
        )
        .unwrap();
        let (rendezvous, _listener) = Rendezvous::create(private.path()).unwrap();
        let mut request = Request {
            schema: 1,
            purpose: ReadMediaPurpose::OriginalRegistration,
            operation,
            rendezvous,
            source_plans: vec![plan.bytes().unwrap()],
            target: TargetDrive {
                name: "fixture-only".into(),
                model: "public test medium".into(),
                stable_id: Some("fixture-only".into()),
                size: 16 * 1024 * 1024 * 1024,
                kind: crate::imager_model::TargetKind::Usb,
                tran: Some("usb".into()),
                removable: true,
                is_system: false,
                partitions: Vec::new(),
            },
        };
        request.validate().unwrap();
        let bytes = serde_json::to_vec(&request).unwrap();
        let held =
            super::super::home_capture_helper::publish_request_bytes(private.path(), &bytes, MAX)
                .unwrap();
        assert_eq!(held.metadata().unwrap().mode() & 0o077, 0);
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["output_path"] = serde_json::json!("/dev/arbitrary");
        assert!(serde_json::from_value::<Request>(value).is_err());
        let mut unknown = serde_json::to_value(&request).unwrap();
        unknown["purpose"] = serde_json::json!("arbitrary-root-read");
        assert!(serde_json::from_value::<Request>(unknown).is_err());
        let mut legacy = serde_json::to_value(&request).unwrap();
        legacy.as_object_mut().unwrap().remove("purpose");
        assert!(
            serde_json::from_value::<Request>(legacy).unwrap().purpose
                == ReadMediaPurpose::OriginalRegistration
        );
        request.purpose = ReadMediaPurpose::BootPending;
        assert!(request.validate().is_err());
        let registered_plans = request.source_plans.clone();
        request.source_plans.clear();
        request.validate().unwrap();
        let boot = serde_json::to_vec(&request).unwrap();
        let decoded: Request = serde_json::from_slice(&boot).unwrap();
        assert!(
            decoded.purpose == ReadMediaPurpose::BootPending && decoded.source_plans.is_empty()
        );
        request.target.is_system = true;
        request.validate().unwrap();
        let saved_kind = request.target.kind;
        let saved_tran = request.target.tran.clone();
        let saved_removable = request.target.removable;
        request.target.kind = crate::imager_model::TargetKind::Nvme;
        request.target.tran = Some("nvme".into());
        request.target.removable = false;
        assert!(request.validate().is_err());
        request.target.kind = saved_kind;
        request.target.tran = saved_tran;
        request.target.removable = saved_removable;
        request.purpose = ReadMediaPurpose::OriginalRegistration;
        assert!(request.validate().is_err());
        request.target.is_system = false;
        request.source_plans = registered_plans;
        request.validate().unwrap();
        request.operation = uuid::Uuid::new_v4().to_string();
        assert!(request.validate().is_err());
        assert!(!private.path().join("output").exists());
    }
    #[test]
    fn registered_read_helper_refuses_malformed_arguments_before_any_permission_or_gui() {
        assert!(maybe_run(&[ARG.into()]).is_err());
        assert!(
            maybe_run(&[
                ARG.into(),
                "request.json".into(),
                "digest".into(),
                "extra".into()
            ])
            .is_err()
        );
        assert_eq!(maybe_run(&["unrelated".into()]).unwrap(), None);
    }
}
