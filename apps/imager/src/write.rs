//! The write / verify seam for destructive work, safely gated.
//!
//! Writing a disk image onto a block device is **destructive** and needs
//! privileged raw access. This module is the single choke point where that
//! work happens. It ships a streaming copy + read-back verify
//! engine ([`stream_copy`], [`verify_readback`], [`run_job_paths`]) — the
//! exact byte loop that flashes a USB stick — and three independent safety
//! gates in front of the block device:
//!
//! 1. **Typed confirmation.** A job only begins after the UI has collected an
//!    explicit, typed confirmation: the operator must type the target device
//!    name. [`begin_job`] re-checks that token against the target before it
//!    returns [`JobStart::Started`].
//!
//! 2. **Write-time privilege.** The wizard always runs as the signed-in user.
//!    Only after confirmation does it ask the host to authenticate a short-
//!    lived helper mode of this executable. The helper receives a secret-free
//!    request, repeats every target check, writes, verifies, and exits.
//!
//! 3. **Capacity + fit checks** ([`WriteRequest`] / [`TargetDrive::fits`]).
//!
//! The engine is exercised in tests against **temp files** (a copy +
//! verify, byte-for-byte) — never a block device — so the production loop is
//! proven without any risk to hardware.

use std::io;
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(test)]
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
#[cfg(test)]
use std::time::Duration;

use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceIdentity, TargetDrive,
};
#[cfg(test)]
use sha2::{Digest, Sha256};

pub use crate::profile::{PROFILE_BLOCK_SIZE, PROFILE_MAGIC};
const _: () = assert!(PROFILE_BLOCK_SIZE == imager_backend::media_model::PROFILE_BLOCK_SIZE);

/// Outcome of asking to start a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStart {
    /// Pre-conditions satisfied; the real job may run.
    Started,
    /// Refused because privileged raw-device writes are not enabled.
    WriteDisabled,
    /// Refused — the confirmation token did not match the target device.
    /// This is the safety gate: a typo means no write.
    ConfirmationMismatch,
    /// Refused — no source or no target selected.
    Incomplete,
    /// Refused — the image is larger than the target drive.
    TooSmall,
    /// Refused because desktop imaging is restricted to external media. The
    /// booted Tilo installer owns internal-disk and dual-boot installation.
    UnsafeTarget,
    /// Refused before any I/O because an installable image's embedded profile
    /// would be rejected by the live installer.
    InvalidProfile(String),
}

/// Which engine actually performs the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteBackend {
    /// Raw-device effects are unavailable in this process (tests and shots).
    Disabled,
    /// Real raw write via a direct block-device open. Selecting this is itself
    /// gated (see [`WriteBackend::for_window`]); it is never the default.
    System,
    /// The unprivileged live UI will authenticate a short-lived platform
    /// helper at write time.
    PrivilegedHelper,
    /// Linux sandbox path: `UDisks2` authorizes and returns one exclusive raw
    /// device descriptor.  The Flatpak never sees `/dev` and cannot execute
    /// commands on the host.
    Udisks2,
}

impl WriteBackend {
    /// Choose the backend for a window: `live` when someone uses it, never
    /// for a screenshot, a past version or a test. The allow gate is
    /// retained for deliberate root-side diagnostics; a live window uses the
    /// narrow helper.
    pub fn for_window(live: bool) -> Self {
        if cfg!(all(target_os = "linux", feature = "flatpak-media-writer"))
            && std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1")
        {
            WriteBackend::Udisks2
        } else if allow_write() {
            WriteBackend::System
        } else if cfg!(feature = "host-media-writer") || live {
            WriteBackend::PrivilegedHelper
        } else {
            WriteBackend::Disabled
        }
    }

    /// Whether this backend can perform a real write. Both the deliberately
    /// allow-gated direct backend and the normal live UI's short-lived helper
    /// are usable; tests and screenshot rendering remain disabled.
    pub fn is_available(self) -> bool {
        self != WriteBackend::Disabled
    }
}

/// The env allow-gate. `TILO_IMAGER_ALLOW_WRITE=1` is the single switch that
/// lets a real block-device write happen; unset (the default) keeps every
/// path unavailable. Modelled on the VPN app's `TILO_VPN_ALLOW_EXEC`.
pub fn allow_write() -> bool {
    std::env::var("TILO_IMAGER_ALLOW_WRITE").as_deref() == Ok("1")
}

/// A validated request to write `source` to `target` using `action`
/// (+ profile when pre-configuring). Construction does not start anything.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WriteRequest {
    pub source: Source,
    pub target: TargetDrive,
    pub action: Action,
    pub profile: InstallProfile,
    /// Prepared etc/var tree with Wi-Fi and enrolled identity. Built on the
    /// setup machine before elevation; the privileged writer only copies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_stage: Option<std::path::PathBuf>,
    /// Private exact Windows suggestions and independent recovery receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) windows_preparation: Option<crate::windows_media::WindowsMedia>,
    /// Provision a distinct migration data filesystem after verified imaging.
    #[serde(default)]
    pub provision_migration_data: bool,
    /// Public expectation only: the writer independently recomputes every byte
    /// of the planned geometry using its authorized descriptors before writing.
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_layout: Option<OfflineLayout>,
}
#[cfg(target_os = "linux")]
pub use imager_backend::setup::OfflineLayout;

fn validate_windows_preparation(req: &WriteRequest) -> io::Result<()> {
    if let Some(held) = &req.windows_preparation {
        if !req.source.installable
            || req.provision_migration_data
            || req.handoff_stage.is_some()
            || req.profile.prepared_setup.is_some()
            || req.profile.enroll_local_identity
            || req.target.stable_id.as_ref().is_none_or(String::is_empty)
        {
            return Err(io::Error::other(
                "Windows media requires a reviewed stable target and live-session new identity",
            ));
        }
        held.prepared.verify()?;
        crate::profile::encode_profile_block_with_windows(&req.profile, Some(&held.prepared))?;
    }
    Ok(())
}

impl WriteRequest {
    /// Drop values which are intentionally never transported to installation
    /// media before this request crosses the privilege boundary.
    fn without_secrets(mut self) -> Self {
        self.profile.user.password.clear();
        self.profile.encryption_passphrase.clear();
        self
    }
}

impl WriteRequest {
    /// Validate that the typed confirmation token matches the target's device
    /// name (e.g. the operator typed `sdb` for `/dev/sdb`). This is the
    /// explicit typed gate required before any write.
    pub fn confirmation_matches(&self, typed: &str) -> bool {
        let typed = typed.trim();
        !typed.is_empty()
            && (typed == self.target.name
                || typed == self.target.dev_path()
                || typed == format!("/dev/{}", self.target.name))
    }
}

/// Attempt to start the job. Returns [`JobStart::Started`] only after every
/// safety pre-condition is met; otherwise a specific refusal the UI surfaces.
///
/// This is the pure *planning / dry-run* entry: it validates and never opens a
/// device. The live app hands a `Started` request to [`spawn_job`], which
/// chooses the backend; with the allow-gate off the request is refused.
pub fn begin_job(req: &WriteRequest, typed_confirmation: &str) -> JobStart {
    if let Err(error) = validate_windows_preparation(req) {
        return JobStart::InvalidProfile(error.to_string());
    }
    if req.provision_migration_data
        && (!cfg!(target_os = "linux")
            || !req.source.installable
            || req.handoff_stage.is_some()
            || req.target.stable_id.as_ref().is_none_or(String::is_empty))
    {
        return JobStart::InvalidProfile("Migration media requires Linux, stable device custody and encrypted prepared setup without legacy tail handoff".into());
    }
    #[cfg(target_os = "linux")]
    if let Some(pending) = &req.offline_layout
        && (!req.provision_migration_data
            || pending.reserved_bytes == 0
            || req.profile.prepared_setup.as_ref() != Some(&pending.setup)
            || req.target.stable_id.as_deref() != Some(pending.binding.target_stable_id.as_str()))
    {
        return JobStart::InvalidProfile(
            "Pending export differs from authorized media/setup selection".into(),
        );
    }
    if req.source.size == 0 || req.target.name.is_empty() {
        return JobStart::Incomplete;
    }
    if !req.target.is_media_target() {
        return JobStart::UnsafeTarget;
    }
    let required = if req.source.installable {
        req.source.size.saturating_add(PROFILE_BLOCK_SIZE as u64)
    } else {
        req.source.size
    };
    if !req.target.fits(required) {
        return JobStart::TooSmall;
    }
    if req.source.installable
        && let Err(message) = req.profile.validate_media_profile()
    {
        return JobStart::InvalidProfile(message);
    }
    if !req.confirmation_matches(typed_confirmation) {
        return JobStart::ConfirmationMismatch;
    }
    JobStart::Started
}

#[cfg(test)]
use crate::profile::write_profile_block_bound;
pub use crate::profile::{encode_profile_block, write_profile_block};

pub use imager_backend::write::JobResult;

/// Peer-local control for one running write. The immutable write request stays
/// separately digest-bound; cancellation only narrows authority by asking the
/// worker/helper to stop at its next safe chunk boundary.
#[derive(Clone, Debug)]
pub struct JobControl {
    cancelled: Arc<AtomicBool>,
    worker: Arc<NativeWorker>,
}

#[derive(Debug, Default)]
struct NativeWorker {
    joined: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    monitor: std::sync::Mutex<Option<ui::rt::JoinHandle<()>>>,
    bound: AtomicBool,
    stopped: AtomicBool,
    failed: AtomicBool,
    wake: std::sync::Condvar,
}
impl NativeWorker {
    fn take_finished(&self) -> Option<std::thread::JoinHandle<()>> {
        let mut held = match self.joined.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        if held
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            held.take()
        } else {
            None
        }
    }
    fn mark_stopped(&self, failed: bool) {
        let _held = self
            .joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.failed.store(failed, Ordering::Relaxed);
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_all();
    }
}
impl JobControl {
    pub(crate) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: Arc::new(NativeWorker::default()),
        }
    }

    fn bind_worker(&self, joined: std::thread::JoinHandle<()>) {
        *self
            .worker
            .joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(joined);
        self.worker.bound.store(true, Ordering::Release);
        if let Ok(runtime) = ui::rt::try_app() {
            let worker = self.worker.clone();
            let monitor = runtime.spawn(async move {
                loop {
                    if worker.stopped.load(Ordering::Acquire) {
                        return;
                    }
                    let finished = worker.take_finished();
                    if let Some(finished) = finished {
                        let failed = finished.join().is_err();
                        worker.mark_stopped(failed);
                        return;
                    }
                    ui::rt::sleep(std::time::Duration::from_millis(25)).await;
                }
            });
            *self
                .worker
                .monitor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(monitor);
        }
    }
    /// Capture presentation owns an idle worker, never a device writer. Its
    /// exit is observed through the same join used by live worker teardown.
    pub(crate) fn bind_capture_worker(&self) -> io::Result<()> {
        let cancelled = Arc::downgrade(&self.cancelled);
        let joined = std::thread::Builder::new()
            .name("imager-capture-progress".into())
            .spawn(move || {
                while cancelled
                    .upgrade()
                    .is_some_and(|flag| !flag.load(Ordering::Acquire))
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })?;
        self.bind_worker(joined);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn owned_fixture(joined: std::thread::JoinHandle<()>) -> Self {
        let control = Self::new();
        control.bind_worker(joined);
        control
    }

    #[cfg(test)]
    pub(crate) fn finished_fixture() -> Self {
        let control = Self::new();
        control.bind_worker(std::thread::spawn(|| {}));
        control
            .wait_stopped()
            .expect("actual fixture writer must stop");
        control
    }

    /// Positive native completion evidence; terminal progress is not this witness.
    pub(crate) fn observed_stopped(&self) -> bool {
        self.worker.stopped.load(Ordering::Acquire)
    }
    /// Exit and outcome are distinct: a panic releases native ownership but
    /// must never turn an earlier success progress packet into a success result.
    pub(crate) fn observed_failure(&self) -> bool {
        self.observed_stopped() && self.worker.failed.load(Ordering::Relaxed)
    }
    fn stopped_result(&self) -> io::Result<()> {
        if self.observed_failure() {
            Err(io::Error::other(crate::setup::strings::t(
                "imager-write-worker-stopped",
            )))
        } else {
            Ok(())
        }
    }

    /// Host teardown only, after frames stopped. Never called by sync/close admission.
    pub(crate) fn wait_stopped(&self) -> io::Result<()> {
        if !self.worker.bound.load(Ordering::Acquire) {
            return Err(io::Error::other(crate::setup::strings::t(
                "imager-write-exit-unowned",
            )));
        }
        let mut held = self
            .worker
            .joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(joined) = held.take() {
            drop(held);
            let result = joined.join();
            self.worker.mark_stopped(result.is_err());
            return self.stopped_result();
        }
        while !self.observed_stopped() {
            held = self
                .worker
                .wake
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        self.stopped_result()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(all(test, unix))]
use imager_backend::write::try_lock_file;
#[cfg(test)]
fn target_lock_name(target: &TargetDrive) -> String {
    imager_backend::write::target_lock_name(target)
}
#[cfg(all(test, target_os = "windows"))]
fn acquire_target_write_lock(
    target: &TargetDrive,
) -> io::Result<imager_backend::write::TargetWriteLock> {
    imager_backend::write::acquire_target_write_lock(target)
}
/// Keep the privileged writer lock through reviewed mounted output.
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
fn with_locked_media_root<T>(
    target: &TargetDrive,
    callback: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    imager_backend::write::with_locked_media_root(target, callback)
}

pub use imager_backend::write::{CHUNK, stream_copy, verify_readback};

/// Run a full write + verify against explicit paths, reporting a [`Progress`]
/// snapshot after every chunk through `on_progress`. Split from device
/// selection so tests drive the real loop against temp files.
pub fn run_job_paths(
    src: &Path,
    dst: &Path,
    total: u64,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    run_job_paths_bound(src, dst, total, None, None, &|| false, &mut on_progress)
}

fn run_job_paths_bound(
    src: &Path,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    imager_backend::write::job::run_job_paths_bound(
        src,
        dst,
        total,
        expected_identity,
        expected_sha256,
        should_cancel,
        |progress| on_progress(progress.into()),
    )
}

/// Run the same exact write, digest, read-back and profile pipeline against an
/// already-authorized device descriptor. `UDisks2` uses this seam so raw-device
/// authority never has to be represented by a sandbox-visible path.
#[cfg(any(test, target_os = "linux"))]
fn run_job_file_bound(
    req: &WriteRequest,
    device: &mut std::fs::File,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    if let Err(error) = validate_windows_preparation(req).and_then(|()| {
        req.windows_preparation
            .as_ref()
            .map_or(Ok(()), |held| held.validate(&req.target))
    }) {
        return JobResult::WriteFailed(error.to_string());
    }
    imager_backend::write::transaction::run_job_file_bound(
        &imager_backend::write::transaction::DescriptorWriteRequest {
            source: reviewed_source(req),
            expected_sha256: req.source.expected_sha256.clone(),
            target_size: req.target.size,
            target_stable_id: req.target.stable_id.clone().unwrap_or_default(),
            installable: req.source.installable,
            provision_migration_data: req.provision_migration_data,
            has_legacy_handoff: req.handoff_stage.is_some(),
            #[cfg(target_os = "linux")]
            expected_layout: req.offline_layout.as_ref().map(|pending| {
                imager_backend::write::transaction::ExpectedLayout {
                    binding: pending.binding.clone(),
                    reserved_bytes: pending.reserved_bytes,
                }
            }),
        },
        device,
        should_cancel,
        |progress| on_progress(progress.into()),
        |device| {
            #[cfg(target_os = "linux")]
            {
                validate_pending_descriptor(req, device)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device;
                Ok(())
            }
        },
        {
            #[cfg(all(target_os = "linux", feature = "installation"))]
            {
                crate::setup::portable_home_media::payload_capacity_for_free_bytes
            }
            #[cfg(not(target_os = "linux"))]
            {
                |_| {
                    Err(io::Error::other(
                        "Migration data provisioning requires Linux",
                    ))
                }
            }
            #[cfg(all(target_os = "linux", not(feature = "installation")))]
            {
                |_| Err(installation_medium_refused())
            }
        },
        || {
            crate::profile::encode_profile_block_with_windows(
                &req.profile,
                req.windows_preparation.as_ref().map(|held| &held.prepared),
            )
        },
    )
}

/// Bind an already-open block descriptor to the freshly reviewed device node.
#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) fn attest_target_descriptor(
    target: &TargetDrive,
    file: &std::fs::File,
) -> io::Result<()> {
    imager_backend::write::attest_target_descriptor(target, file)
}

/// Run the real job for `req` (source file → target device), streaming
/// [`Progress`] over `tx`. Only ever called on a worker thread when the
/// allow-gate selected [`WriteBackend::System`].
fn run_system_job(
    req: &WriteRequest,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
    authenticated_caller: Option<u32>,
) -> JobResult {
    if let Err(error) = validate_windows_preparation(req).and_then(|()| {
        req.windows_preparation
            .as_ref()
            .map_or(Ok(()), |held| held.validate(&req.target))
    }) {
        return JobResult::WriteFailed(error.to_string());
    }
    // Only the installation's medium layout needs the helper's caller.
    #[cfg(not(all(target_os = "linux", feature = "installation")))]
    let _ = authenticated_caller;
    imager_backend::write::job::run_system_job(
        &imager_backend::write::job::SystemWriteRequest {
            source: reviewed_source(req),
            target: req.target.clone(),
            expected_sha256: req.source.expected_sha256.clone(),
            installable: req.source.installable,
            provision_migration_data: req.provision_migration_data,
        },
        &AppWriteSetup {
            req,
            #[cfg(all(target_os = "linux", feature = "installation"))]
            authenticated_caller,
        },
        should_cancel,
        |progress| on_progress(progress.into()),
    )
}

struct AppWriteSetup<'a> {
    req: &'a WriteRequest,
    #[cfg(all(target_os = "linux", feature = "installation"))]
    authenticated_caller: Option<u32>,
}
impl imager_backend::write::job::WriteSetup for AppWriteSetup<'_> {
    fn output_preflight(&self) -> io::Result<()> {
        #[cfg(all(target_os = "linux", not(feature = "installation")))]
        if self.req.offline_layout.is_some() {
            return Err(installation_medium_refused());
        }
        #[cfg(all(target_os = "linux", feature = "installation"))]
        if let Some(layout) = &self.req.offline_layout {
            return self
                .authenticated_caller
                .ok_or_else(|| {
                    io::Error::other("Original migration requires authenticated helper custody")
                })
                .and_then(|uid| crate::setup::media_setup::validate_pending_caller(layout, uid));
        }
        Ok(())
    }

    fn validate_pending_source_disks(&self) -> io::Result<()> {
        validate_pending_source_disks(self.req)
    }

    fn write_descriptor(
        &self,
        device: &mut std::fs::File,
        should_cancel: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(imager_backend::write::helper::HelperProgress),
    ) -> JobResult {
        #[cfg(any(test, target_os = "linux"))]
        {
            run_job_file_bound(self.req, device, &|| should_cancel(), |progress| {
                on_progress(progress.into())
            })
        }
        #[cfg(not(any(test, target_os = "linux")))]
        {
            let _ = (device, should_cancel, on_progress);
            JobResult::WriteFailed("Migration data provisioning requires Linux".into())
        }
    }

    fn write_profile(
        &self,
        destination: &Path,
        should_cancel: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        crate::profile::write_windows_profile_block_bound(
            destination,
            self.req.target.size,
            &self.req.profile,
            self.req
                .windows_preparation
                .as_ref()
                .map(|held| &held.prepared),
            &|| should_cancel(),
        )
    }

    fn apply_handoff(&self, destination: &Path) -> io::Result<()> {
        if let Some(stage) = self.req.handoff_stage.as_deref() {
            apply_handoff_stage_to_device(
                destination,
                stage,
                self.req.source.size,
                self.req.target.size,
            )?;
        }
        Ok(())
    }

    fn provision_after_write(&self, should_cancel: &dyn Fn() -> bool) -> io::Result<()> {
        #[cfg(all(target_os = "linux", not(feature = "installation")))]
        if self.req.offline_layout.is_some() {
            return Err(installation_medium_refused());
        }
        #[cfg(all(target_os = "linux", feature = "installation"))]
        if let Some(pending) = &self.req.offline_layout {
            return self
                .authenticated_caller
                .ok_or_else(|| io::Error::other("Original migration caller custody missing"))
                .and_then(|uid| {
                    crate::setup::media_setup::provision_after_write(
                        &self.req.target,
                        pending,
                        uid,
                        &|| should_cancel(),
                    )
                });
        }
        let _ = should_cancel;
        Ok(())
    }

    fn release_handoff(&self) {
        release_handoff_stage(self.req);
    }
}

#[cfg(all(
    target_os = "linux",
    any(
        feature = "flatpak-media-writer",
        all(test, feature = "native-capture-qualification")
    )
))]
fn open_udisks_device(target: &TargetDrive) -> io::Result<std::fs::File> {
    imager_backend::write::udisks::open_udisks_device(target)
}

/// Qualification uses the actual canonical UDisks writer open, under the
/// same root-owned target lock. No fabricated target or raw path grant.
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
pub(crate) fn with_qualification_udisks_target<T>(
    target: &TargetDrive,
    callback: impl FnOnce(std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    if !target.is_media_target() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    with_locked_media_root(target, || {
        let device = open_udisks_device(target)?;
        attest_target_descriptor(target, &device)?;
        let result = callback(device);
        revalidate_target(target)?;
        result
    })
}

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn run_udisks_job(
    req: &WriteRequest,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    imager_backend::write::job::run_udisks_job(
        &req.target,
        &AppWriteSetup {
            req,
            authenticated_caller: None,
        },
        should_cancel,
        |progress| on_progress(progress.into()),
    )
}

#[cfg(test)]
use imager_backend::write::helper::create_progress_output;
#[cfg(all(test, target_os = "linux"))]
use imager_backend::write::helper::read_linux_helper_request;
#[cfg(test)]
use imager_backend::write::helper::{WINDOWS_ELEVATION_SCRIPT, elevated_helper_command};
#[cfg(test)]
use imager_backend::write::helper::{WRITE_HELPER_ARG, validate_helper_paths};

/// The staged tree holds the Wi-Fi passphrase and possibly a device key.
/// It goes away on every exit, including a write that never started.
fn release_handoff_stage(req: &WriteRequest) {
    if let Some(stage) = req.handoff_stage.as_deref() {
        let _ = std::fs::remove_dir_all(stage);
    }
}

/// Customize only a regular disk image through the shared installed-root path.
///
/// # Errors
/// Returns target metadata, mounted-root discovery, or copy errors.
pub(crate) fn apply_handoff_stage_to_image_file(image: &Path, staged: &Path) -> io::Result<()> {
    imager_backend::write::handoff::apply_handoff_stage_to_image_file(
        image,
        staged,
        apply_handoff_stage_onto_mounted_root,
    )
}

fn apply_handoff_stage_to_device(
    device: &Path,
    staged: &Path,
    image_len: u64,
    device_len: u64,
) -> io::Result<()> {
    imager_backend::write::handoff::apply_handoff_stage_to_device(
        device,
        staged,
        image_len,
        device_len,
        apply_handoff_stage_onto_mounted_root,
        crate::handoff::embed_setup_handoff,
    )
}

pub(crate) fn apply_handoff_stage_onto_mounted_root(mnt: &Path, staged: &Path) -> io::Result<()> {
    crate::handoff::install_staged_onto_mounted_root(mnt, staged)
}

#[cfg(test)]
use block_devices::installed_root::{
    root_listing_waits_for_filesystem_type, select_tilo_root_partition,
};
// Only the Linux tests mount an installed root or read a mountpoint listing.
#[cfg(test)]
#[cfg(target_os = "linux")]
use block_devices::installed_root::mountpoint_listing_is_busy;

#[cfg(test)]
#[cfg(target_os = "linux")]
fn copy_handoff_onto_installed_root(root_part: &str, staged: &Path) -> io::Result<()> {
    let mountpoint = tempfile::tempdir()?;
    block_devices::installed_root::with_mounted_root(root_part, mountpoint.path(), |mnt| {
        apply_handoff_stage_onto_mounted_root(mnt, staged)
    })
}

#[cfg(test)]
fn handoff_when_root_was_not_found(
    device: &Path,
    image_len: u64,
    device_len: u64,
    staged: &Path,
) -> io::Result<()> {
    imager_backend::write::handoff::handoff_when_root_was_not_found(
        device,
        image_len,
        device_len,
        staged,
        crate::handoff::embed_setup_handoff,
    )
}

/// Execute the privileged helper mode when `args` requests it. This is called
/// before the windowing runtime is initialized.
///
/// The request is deserialized from a user-private temporary file, then fully
/// validated and its live target identity is checked again by
/// `run_system_job`. Progress is append-only JSON so the unprivileged window
/// can remain responsive while the platform authentication broker waits.
///
/// # Errors
///
/// Returns a descriptive error for malformed arguments/request data or an I/O
/// failure in the helper protocol.
pub fn maybe_run_write_helper(args: &[String]) -> Result<Option<i32>, Box<dyn std::error::Error>> {
    let Some(authorized) = imager_backend::write::helper::read_authorized_helper_request(args)?
    else {
        return Ok(None);
    };
    let progress_path = &authorized.progress_path;
    let cancel_path = &authorized.cancel_path;
    let request_bytes = &authorized.bytes;
    let request: WriteRequest = serde_json::from_slice(&request_bytes)?;
    if begin_job(&request, &request.target.dev_path()) != JobStart::Started {
        return Err("write helper rejected an invalid or unsafe request".into());
    }
    #[cfg(target_os = "linux")]
    let request = {
        let mut request = request;
        request.source.path =
            imager_backend::write::helper::bind_linux_helper_source(&reviewed_source(&request))?
                .to_string_lossy()
                .into_owned();
        request
    };
    let exit = imager_backend::write::helper::serve_helper(
        progress_path,
        cancel_path,
        |should_cancel, on_progress| {
            run_system_job(
                &request,
                &|| should_cancel(),
                |progress| on_progress(progress.into()),
                authorized.caller_uid,
            )
        },
    )?;
    Ok(Some(exit))
}

fn spawn_privileged_helper(req: WriteRequest, control: &JobControl, tx: &Sender<Progress>) {
    let result = run_privileged_helper(req, control, tx);
    if let Err(error) = result {
        let _ = tx.send(Progress {
            phase: JobPhase::Failed,
            error: Some(format!(
                "Could not start the authorized writer: {error}. Check the authorization prompt and try again."
            )),
            ..Progress::default()
        });
    }
}

fn reviewed_source(req: &WriteRequest) -> imager_backend::write::helper::ReviewedSource {
    imager_backend::write::helper::ReviewedSource {
        path: req.source.path.clone().into(),
        size: req.source.size,
        identity: req.source.identity,
    }
}

fn run_privileged_helper(
    req: WriteRequest,
    control: &JobControl,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let source = reviewed_source(&req);
    let bytes = serde_json::to_vec(&req.without_secrets())?;
    imager_backend::write::helper::run_privileged_helper(
        &source,
        &bytes,
        &|| control.is_cancelled(),
        |progress| {
            let _ = tx.send(progress.into());
        },
    )
}

#[cfg(test)]
fn relay_helper_progress(
    child: &mut Child,
    progress_path: &Path,
    cancel_path: &Path,
    control: &JobControl,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    imager_backend::write::helper::relay_helper_progress(
        child,
        progress_path,
        cancel_path,
        &|| control.is_cancelled(),
        &mut |progress| {
            let _ = tx.send(progress.into());
        },
    )
}

#[cfg(test)]
fn relay_cancellation(control: &JobControl, cancel_path: &Path, sent: &mut bool) -> io::Result<()> {
    imager_backend::write::helper::relay_cancellation(&|| control.is_cancelled(), cancel_path, sent)
}

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
fn revalidate_target(target: &TargetDrive) -> io::Result<()> {
    imager_backend::write::revalidate_target(target)
}
#[cfg(test)]
fn target_identity_matches(selected: &TargetDrive, current: &TargetDrive) -> bool {
    imager_backend::write::target_identity_matches(selected, current)
}
#[cfg(test)]
use imager_backend::write::{macos_should_eject, run_finite_status};

/// Spawn a job on a worker thread and return the receiver the UI drains once
/// per frame. The returned [`JobStart`] is the validation result; a channel is
/// only returned when it is [`JobStart::Started`].
///
/// * Tests/shots → the request is refused and no worker is created.
/// * Live UI → platform authentication starts the narrow helper.
/// * Deliberate allow-gated root diagnostics → `run_system_job` runs in the
///   current process.
pub fn spawn_job(
    req: WriteRequest,
    typed: &str,
    backend: WriteBackend,
) -> (JobStart, Option<Receiver<Progress>>, Option<JobControl>) {
    let start = begin_job(&req, typed);
    if start != JobStart::Started {
        return (start, None, None);
    }
    if backend == WriteBackend::Disabled {
        return (JobStart::WriteDisabled, None, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let control = JobControl::new();
    let worker_control = control.clone();
    let spawned = std::thread::Builder::new()
        .name("imager-write".into())
        .spawn(move || match backend {
            WriteBackend::System => {
                let _ = run_system_job(
                    &req,
                    &|| worker_control.is_cancelled(),
                    |p| {
                        let _ = tx.send(p);
                    },
                    None,
                );
            }
            WriteBackend::PrivilegedHelper => {
                spawn_privileged_helper(req, &worker_control, &tx);
            }
            WriteBackend::Udisks2 => {
                #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
                let _ = run_udisks_job(&req, &|| worker_control.is_cancelled(), |p| {
                    let _ = tx.send(p);
                });
                #[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
                let _ = tx.send(Progress {
                    phase: JobPhase::Failed,
                    error: Some("UDisks2 media writing is unavailable in this build.".into()),
                    ..Progress::default()
                });
            }
            WriteBackend::Disabled => {}
        });
    let joined = match spawned {
        Ok(joined) => joined,
        Err(error) => {
            return (
                JobStart::InvalidProfile(crate::setup::strings::t_args(
                    "imager-write-worker-start-failed",
                    &[("error", error.to_string().as_str().into())],
                )),
                None,
                None,
            );
        }
    };
    control.bind_worker(joined);
    (JobStart::Started, Some(rx), Some(control))
}

fn validate_pending_source_disks(req: &WriteRequest) -> io::Result<()> {
    #[cfg(all(target_os = "linux", not(feature = "installation")))]
    if req.offline_layout.is_some() {
        return Err(installation_medium_refused());
    }
    #[cfg(all(target_os = "linux", feature = "installation"))]
    if let Some(pending) = &req.offline_layout {
        imager_backend::write::pending::validate_pending_source_disks(
            &req.target,
            pending.target_number,
            |number| {
                crate::setup::migration_handoff::validate_offline_source_disks(
                    &pending.source_plans,
                    number,
                )
            },
        )?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = req;
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_pending_descriptor(req: &WriteRequest, device: &std::fs::File) -> io::Result<()> {
    #[cfg(not(feature = "installation"))]
    if req.offline_layout.is_some() {
        let _ = device;
        return Err(installation_medium_refused());
    }
    #[cfg(feature = "installation")]
    if let Some(pending) = &req.offline_layout {
        imager_backend::write::pending::validate_pending_descriptor(
            &req.target,
            device,
            pending.target_number,
            |number| {
                crate::setup::migration_handoff::validate_offline_source_disks(
                    &pending.source_plans,
                    number,
                )
            },
        )?;
    }
    Ok(())
}

/// A request that lays out a prepared installation medium (original files
/// carried to the new system) needs the installation; the Tilo app's writer
/// writes images and refuses it before touching the drive.
#[cfg(all(target_os = "linux", not(feature = "installation")))]
fn installation_medium_refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Preparing an installation medium needs the Tilo Installer download",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};

    /// Target identity, confirmation and job admission.
    #[test]
    fn begin_refuses_mismatched_oversized_forbidden_or_invalid_jobs() {
        // Phase `target_lock_prefers_stable_hardware_identity_then_falls_back_to_path`.
        {
            let mut original = sample_targets().remove(0);
            original.stable_id = Some("usb-serial-123".into());
            let mut renumbered = original.clone();
            renumbered.name = "renumbered-device".into();
            assert_eq!(target_lock_name(&original), target_lock_name(&renumbered));

            original.stable_id = None;
            renumbered.stable_id = None;
            assert_ne!(target_lock_name(&original), target_lock_name(&renumbered));
        }

        // Phase `confirmation_must_match_target`.
        {
            let r = req();
            assert!(r.confirmation_matches("sdb"));
            assert!(r.confirmation_matches("/dev/sdb"));
            assert!(!r.confirmation_matches("sda"));
            assert!(!r.confirmation_matches(""));
            assert!(!r.confirmation_matches("nvme0n1"));
        }

        // Phase `begin_refuses_on_mismatched_confirmation`.
        {
            let r = req();
            assert_eq!(begin_job(&r, "sda"), JobStart::ConfirmationMismatch);
            assert_eq!(begin_job(&r, ""), JobStart::ConfirmationMismatch);
        }

        // Phase `begin_refuses_when_image_too_big`.
        {
            let mut r = req();
            r.source.size = 40_000_000_000; // bigger than the 30 GB stick
            assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
        }

        // Phase `begin_refuses_internal_and_system_targets`.
        {
            let mut r = req();
            r.target.removable = false;
            r.target.tran = Some("sata".into());
            assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
            r.target.removable = true;
            r.target.is_system = true;
            assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
        }

        // Phase `begin_refuses_invalid_profile_before_worker_start`.
        {
            let mut r = req();
            r.profile.keyboard = "not-a-layout".into();
            assert!(matches!(
                begin_job(&r, "sdb"),
                JobStart::InvalidProfile(message) if message.contains("Keyboard")
            ));
        }

        // Phase `target_revalidation_rejects_reused_device_name`.
        {
            let selected = req().target;
            let mut replacement = selected.clone();
            assert!(target_identity_matches(&selected, &replacement));
            replacement.stable_id = Some("another-device".into());
            assert!(!target_identity_matches(&selected, &replacement));
            replacement = selected.clone();
            replacement.is_system = true;
            assert!(!target_identity_matches(&selected, &replacement));
        }
    }

    #[cfg(unix)]
    #[test]
    fn target_lock_excludes_another_helper_handle_until_the_owner_releases_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.lock");
        let first = try_lock_file(&path).expect("first helper reserves target");
        let contention = try_lock_file(&path).expect_err("second helper must be excluded");
        assert_eq!(contention.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        let _reacquired = try_lock_file(&path).expect("kernel releases lock with its owner");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn target_lock_excludes_another_helper_handle_until_the_owner_releases_it() {
        let target = sample_targets().remove(0);
        let first = acquire_target_write_lock(&target).expect("first helper reserves target");
        let contention =
            acquire_target_write_lock(&target).expect_err("second helper must be excluded");
        assert_eq!(contention.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        acquire_target_write_lock(&target).expect("kernel releases lock with its owner");
    }

    fn req() -> WriteRequest {
        WriteRequest {
            source: sample_source(),
            target: sample_targets()[0].clone(), // the USB stick
            action: Action::Flash,
            profile: InstallProfile::default(),
            handoff_stage: None,
            windows_preparation: None,
            provision_migration_data: false,
            #[cfg(target_os = "linux")]
            offline_layout: None,
        }
    }

    /// Backend gating across builds and platforms.
    #[test]
    fn backend_selection_is_gated_and_never_simulates_writes() {
        // Phase `default_backend_is_disabled_without_gate`.
        #[cfg(not(feature = "host-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // No opt-in env var set → unavailable, never a fake success.
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            assert_eq!(WriteBackend::for_window(false), WriteBackend::Disabled);
            assert!(!allow_write());
        }

        // Phase `packaged_host_build_enables_the_short_lived_helper_without_an_environment_flag`.
        #[cfg(feature = "host-media-writer")]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            assert_eq!(
                WriteBackend::for_window(false),
                WriteBackend::PrivilegedHelper
            );
            assert!(WriteBackend::for_window(false).is_available());
        }

        // Phase `live_ui_uses_short_lived_privileged_helper`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            assert_eq!(
                WriteBackend::for_window(true),
                WriteBackend::PrivilegedHelper
            );
        }

        // Phase `flatpak_selects_udisks_without_enabling_direct_device_writes`.
        #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE")
                .set("TILO_IMAGER_FLATPAK", "1");
            assert_eq!(WriteBackend::for_window(false), WriteBackend::Udisks2);
            assert!(!allow_write(), "the sandbox never gains direct /dev access");
        }

        // Phase `only_disabled_backend_is_unavailable`.
        {
            assert!(!WriteBackend::Disabled.is_available());
            assert!(WriteBackend::System.is_available());
            assert!(WriteBackend::PrivilegedHelper.is_available());
            assert!(WriteBackend::Udisks2.is_available());
        }

        // Phase `system_backend_selected_only_with_gate_on`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // The real block-device backend is chosen ONLY when the operator opts
            // in explicitly; flipping the gate is the single switch between the
            // disabled path and the real device write.
            env.set("TILO_IMAGER_ALLOW_WRITE", "1");
            assert!(allow_write());
            assert_eq!(WriteBackend::for_window(false), WriteBackend::System);
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            let expected = if cfg!(feature = "host-media-writer") {
                WriteBackend::PrivilegedHelper
            } else {
                WriteBackend::Disabled
            };
            assert_eq!(WriteBackend::for_window(false), expected);
        }

        // Phase `spawn_refuses_instead_of_simulating_with_gate_off`.
        #[cfg(not(feature = "host-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // Validation remains pure, but the live start refuses rather than
            // reporting synthetic progress and success.
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            let r = req();
            assert_eq!(begin_job(&r, "sdb"), JobStart::Started);
            let (start, rx, control) = spawn_job(r, "sdb", WriteBackend::for_window(false));
            assert_eq!(start, JobStart::WriteDisabled);
            assert!(rx.is_none());
            assert!(control.is_none());
        }
    }

    /// Helper request integrity and elevation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn privileged_helper_requests_are_secret_free_private_and_exact() {
        // Phase `privileged_request_never_contains_user_or_encryption_secrets`.
        {
            let mut request = req();
            request.profile.user.password = "account-secret".into();
            request.profile.encryption_passphrase = "encryption-secret".into();
            let encoded = serde_json::to_string(&request.without_secrets()).unwrap();
            assert!(!encoded.contains("account-secret"));
            assert!(!encoded.contains("encryption-secret"));
            assert!(encoded.contains("\"password\":\"\""));
            assert!(encoded.contains("\"encryption_passphrase\":\"\""));
        }

        // Phase `helper_rejects_a_request_changed_after_authorization`.
        {
            #[cfg(target_os = "linux")]
            use std::os::unix::fs::PermissionsExt;

            #[cfg(target_os = "linux")]
            let mut env = tilo_test_env::ScopedEnv::new();
            #[cfg(target_os = "linux")]
            env.set("PKEXEC_UID", tilo_process::effective_uid().to_string());
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            let progress_path = work.path().join("progress.jsonl");
            let cancel_path = work.path().join("cancel");
            std::fs::write(&request_path, serde_json::to_vec(&req()).unwrap()).unwrap();
            std::fs::write(&progress_path, []).unwrap();
            #[cfg(target_os = "linux")]
            {
                std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            let args = vec![
                "imager".into(),
                WRITE_HELPER_ARG.into(),
                request_path.display().to_string(),
                progress_path.display().to_string(),
                "0".repeat(64),
                cancel_path.display().to_string(),
            ];
            let error = maybe_run_write_helper(&args).unwrap_err();
            #[cfg(target_os = "linux")]
            drop(env);
            assert!(error.to_string().contains("changed after authorization"));
            assert!(std::fs::read(progress_path).unwrap().is_empty());
        }

        // Phase `helper_rejects_a_non_private_job_directory`.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut env = tilo_test_env::ScopedEnv::new();
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            std::fs::write(&request_path, b"{}").unwrap();
            std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            env.set("PKEXEC_UID", tilo_process::effective_uid().to_string());

            let error = read_linux_helper_request(&request_path).unwrap_err();

            drop(env);
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("not private"));
        }

        // Phase `helper_request_reader_never_follows_a_symlink`.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::fs::symlink;

            let mut env = tilo_test_env::ScopedEnv::new();
            let work = tempfile::tempdir().unwrap();
            let outside = work.path().join("outside");
            let request = work.path().join("request.json");
            std::fs::write(&outside, b"root-only payload").unwrap();
            symlink(&outside, &request).unwrap();
            std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            env.set("PKEXEC_UID", tilo_process::effective_uid().to_string());

            let error = read_linux_helper_request(&request).unwrap_err();

            drop(env);
            assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        }

        // Phase `helper_never_follows_or_overwrites_a_planted_progress_path`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let progress_path = work.path().join("progress.jsonl");
            std::fs::write(&progress_path, b"keep-me").unwrap();
            let error = create_progress_output(&progress_path).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(progress_path).unwrap(), b"keep-me");
        }

        // Phase `linux_elevates_only_the_hidden_helper_mode`.
        #[cfg(target_os = "linux")]
        {
            let source = tempfile::tempfile().unwrap();
            let command = elevated_helper_command(
                Path::new("/opt/tilo-imager-bin"),
                Path::new("/tmp/request"),
                Path::new("/tmp/progress"),
                &"a".repeat(64),
                Path::new("/tmp/cancel"),
                source,
            )
            .unwrap();
            assert_eq!(command.get_program(), "pkexec");
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[0], "/opt/tilo-imager-bin");
            assert_eq!(args[1], WRITE_HELPER_ARG);
            assert_eq!(args.len(), 6);
        }

        // Phase `windows_elevation_script_native_quotes_paths_with_spaces`.
        {
            // PowerShell single-quoted strings do not treat backslash as an
            // escape. The wrapper must concatenate a literal `"`, not `\"`,
            // around both paths before Start-Process flattens ArgumentList.
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[1] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[2] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[4] + '\"')"));
            assert!(!WINDOWS_ELEVATION_SCRIPT.contains(r#"('\"' +"#));
        }
    }

    /// Cancellation markers, relay errors and command deadlines.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn helper_cancellation_and_child_processes_are_bounded_and_retryable() {
        // Phase `cancellation_marker_is_separate_from_digest_bound_request`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            let cancel_path = work.path().join("cancel");
            let request_bytes = serde_json::to_vec(&req()).unwrap();
            std::fs::write(&request_path, &request_bytes).unwrap();
            let digest_before = Sha256::digest(&request_bytes);
            let control = JobControl::new();
            control.cancel();
            let mut sent = false;

            relay_cancellation(&control, &cancel_path, &mut sent).unwrap();

            assert!(sent);
            assert!(cancel_path.is_file());
            assert_eq!(
                Sha256::digest(std::fs::read(request_path).unwrap()),
                digest_before
            );
        }

        // Phase `failed_cancellation_delivery_remains_retryable`.
        {
            let work = tempfile::tempdir().unwrap();
            let job_dir = work.path().join("tilo-imager-write-retry");
            let cancel_path = job_dir.join("cancel");
            let control = JobControl::new();
            control.cancel();
            let mut sent = false;

            assert!(relay_cancellation(&control, &cancel_path, &mut sent).is_err());
            assert!(!sent, "a failed marker write must be retried");
            std::fs::create_dir(&job_dir).unwrap();
            relay_cancellation(&control, &cancel_path, &mut sent).unwrap();

            assert!(sent);
            assert!(cancel_path.is_file());
        }

        // Phase `relay_error_waits_until_the_privileged_child_has_exited`.
        #[cfg(unix)]
        {
            let work = tempfile::tempdir().unwrap();
            let mut child = Command::new("sh")
                .args(["-c", "sleep 0.05"])
                .spawn()
                .unwrap();
            let (tx, _rx) = std::sync::mpsc::channel();
            let control = JobControl::new();

            // Opening a directory as the JSONL progress file fails immediately.
            let error = relay_helper_progress(
                &mut child,
                work.path(),
                &work.path().join("cancel"),
                &control,
                &tx,
            )
            .unwrap_err();

            assert_ne!(error.kind(), io::ErrorKind::NotFound);
            assert!(child.try_wait().unwrap().is_some());
        }

        // Phase `finite_platform_command_is_reaped_at_its_deadline`.
        #[cfg(unix)]
        {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 10"]);
            let started = ui::time::now();
            let error = run_finite_status(command, Duration::from_millis(20)).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        // Phase `helper_rejects_cancellation_marker_outside_private_job_directory`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let outside = tempfile::tempdir().unwrap();
            let request_path = work.path().join("request.json");
            std::fs::write(&request_path, b"{}").unwrap();

            let error = validate_helper_paths(
                &request_path,
                &work.path().join("progress.jsonl"),
                &outside.path().join("cancel"),
            )
            .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("outside its private job directory")
            );
        }
    }

    /// Profile block reservation, encoding, extraction and cancel.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_profile_block_is_reserved_secret_free_and_verified() {
        // Phase `q_os_media_reserves_room_for_the_profile`.
        {
            let mut r = req();
            r.source.installable = true;
            r.source.size = r.target.size - PROFILE_BLOCK_SIZE as u64 + 1;
            assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
        }

        // Phase `media_profile_is_secret_free_and_read_back_verified`.
        {
            let mut profile = InstallProfile {
                hostname: "studio".into(),
                ..InstallProfile::default()
            };
            profile.user.username = "ada".into();
            profile.user.password = "account-secret".into();
            profile.encryption_passphrase = "disk-secret".into();
            let block = encode_profile_block(&profile).unwrap();
            let text = String::from_utf8_lossy(&block);
            assert!(text.contains(PROFILE_MAGIC));
            assert!(text.contains("studio"));
            assert!(!text.contains("account-secret"));
            assert!(!text.contains("disk-secret"));

            let target = tempfile::NamedTempFile::new().unwrap();
            target
                .as_file()
                .set_len(PROFILE_BLOCK_SIZE as u64 * 2)
                .unwrap();
            write_profile_block(target.path(), PROFILE_BLOCK_SIZE as u64 * 2, &profile).unwrap();
            let bytes = std::fs::read(target.path()).unwrap();
            assert_eq!(&bytes[PROFILE_BLOCK_SIZE..], block.as_slice());
        }

        // Phase `invalid_media_profile_is_rejected_before_a_block_is_encoded`.
        {
            let profile = InstallProfile {
                hostname: "bad host".into(),
                ..InstallProfile::default()
            };
            let error = encode_profile_block(&profile).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("Hostname"));
        }

        // Phase `written_profile_is_consumed_by_the_live_extractor`.
        // Exercise the real Rust encoder/tail writer against the exact live-media
        // extractor instead of maintaining two merely similar format fixtures.
        #[cfg(target_os = "linux")]
        {
            let mut profile = InstallProfile {
                hostname: "studio".into(),
                keyboard: "de".into(),
                timezone: "Europe/Berlin".into(),
                ..InstallProfile::default()
            };
            profile.user.username = "ada".into();
            profile.user.full_name = "ada Example".into();
            profile.user.password = "must-not-cross".into();

            let work = tempfile::tempdir().unwrap();
            let media = work.path().join("media.img");
            let out = work.path().join("extracted");
            std::fs::File::create(&media)
                .unwrap()
                .set_len(PROFILE_BLOCK_SIZE as u64 * 2)
                .unwrap();
            write_profile_block(&media, PROFILE_BLOCK_SIZE as u64 * 2, &profile).unwrap();

            let extractor = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../os/usr/libexec/tilo-installer-profile-extract");
            let status = std::process::Command::new(extractor)
                .env("TILO_INSTALLER_PROFILE_DEVICE", &media)
                .env("TILO_INSTALLER_PROFILE_ALLOW_FILE", "1")
                .env("TILO_INSTALLER_PROFILE_OUT_DIR", &out)
                .status()
                .unwrap();
            assert!(status.success());

            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(out.join("profile.json")).unwrap()).unwrap();
            assert_eq!(value["hostname"], "studio");
            assert_eq!(value["username"], "ada");
            assert_eq!(value["keyboard"], "de");
            assert!(value.get("password").is_none());
            assert!(
                !String::from_utf8_lossy(&std::fs::read(media).unwrap()).contains("must-not-cross")
            );
        }

        // Phase `profile_write_honours_cancel_before_touching_the_tail`.
        {
            let target = tmp_with(b"leave this target unchanged");
            let before = std::fs::read(target.path()).unwrap();
            let error = write_profile_block_bound(
                target.path(),
                before.len() as u64,
                &InstallProfile::default(),
                &|| true,
            )
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(std::fs::read(target.path()).unwrap(), before);
        }
    }

    /// Stream copy, readback verification, job paths and eject.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_engine_copies_verifies_and_reports_end_to_end() {
        // Phase `stream_copy_reproduces_source_byte_for_byte`.
        {
            let data = sample_bytes(CHUNK * 2 + 12345); // multi-chunk + a ragged tail
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let mut last = 0u64;
            let n = stream_copy(src.path(), dst.path(), |b| last = b).unwrap();
            assert_eq!(n as usize, data.len());
            assert_eq!(last, n, "final progress equals total bytes");
            let back = std::fs::read(dst.path()).unwrap();
            assert_eq!(back, data, "written bytes match the source exactly");
        }

        // Phase `verify_readback_passes_on_identical_and_flags_the_diff`.
        {
            let data = sample_bytes(CHUNK + 999);
            let src = tmp_with(&data);
            let dst = tmp_with(&data);
            assert_eq!(
                verify_readback(src.path(), dst.path(), data.len() as u64, |_| {}).unwrap(),
                None,
                "identical files verify clean"
            );
            // Corrupt one byte deep in the second chunk and confirm it is caught.
            let mut corrupt = data.clone();
            let at = CHUNK + 100;
            corrupt[at] ^= 0xFF;
            let bad = tmp_with(&corrupt);
            assert_eq!(
                verify_readback(src.path(), bad.path(), data.len() as u64, |_| {}).unwrap(),
                Some(at as u64),
                "the first differing byte offset is reported"
            );
        }

        // Phase `run_job_paths_writes_and_verifies_end_to_end`.
        {
            let data = sample_bytes(CHUNK * 3 + 4096);
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let mut phases = Vec::new();
            let mut last = Progress::default();
            let result = run_job_paths(src.path(), dst.path(), data.len() as u64, |p| {
                if phases.last() != Some(&p.phase) {
                    phases.push(p.phase);
                }
                last = p;
            });
            assert_eq!(result, JobResult::Verified);
            assert!(result.is_success());
            // It really wrote the bytes and drove both non-terminal phases to
            // completion. Only `run_system_job` may announce terminal success,
            // after the optional profile write and device finalisation.
            assert_eq!(std::fs::read(dst.path()).unwrap(), data);
            assert_eq!(phases, vec![JobPhase::Writing, JobPhase::Verifying]);
            assert!((last.write_frac - 1.0).abs() < 1e-9);
            assert!((last.verify_frac - 1.0).abs() < 1e-9);
        }

        // Phase `authorized_descriptor_path_writes_verifies_and_embeds_profile`.
        {
            let data = sample_bytes(CHUNK + 917);
            let source = tmp_with(&data);
            let mut target = tempfile::tempfile().unwrap();
            let target_size = data.len() as u64 + PROFILE_BLOCK_SIZE as u64 + 4096;
            target.set_len(target_size).unwrap();
            let mut request = req();
            request.source.path = source.path().to_string_lossy().into_owned();
            request.source.size = data.len() as u64;
            request.source.identity = SourceIdentity::from_file(source.as_file()).unwrap();
            request.source.installable = true;
            request.source.expected_sha256 = None;
            request.target.size = target_size;

            let mut phases = Vec::new();
            let result = run_job_file_bound(&request, &mut target, &|| false, |progress| {
                if phases.last() != Some(&progress.phase) {
                    phases.push(progress.phase);
                }
            });

            assert_eq!(result, JobResult::Verified);
            assert_eq!(phases, vec![JobPhase::Writing, JobPhase::Verifying]);
            target.seek(SeekFrom::Start(0)).unwrap();
            let mut written = vec![0_u8; data.len()];
            target.read_exact(&mut written).unwrap();
            assert_eq!(written, data);
            target
                .seek(SeekFrom::Start(target_size - PROFILE_BLOCK_SIZE as u64))
                .unwrap();
            let mut profile = vec![0_u8; PROFILE_BLOCK_SIZE];
            target.read_exact(&mut profile).unwrap();
            assert!(profile.starts_with(PROFILE_MAGIC.as_bytes()));
        }

        // Phase `run_job_paths_reports_write_failure_on_bad_dest`.
        {
            let data = sample_bytes(1024);
            let src = tmp_with(&data);
            // A destination under a path that cannot exist → open fails → WriteFailed.
            let bad = std::path::Path::new("/proc/definitely/not/writable/imager.img");
            let result = run_job_paths(src.path(), bad, data.len() as u64, |_| {});
            assert!(matches!(result, JobResult::WriteFailed(_)));
        }

        // Phase `macos_ejects_only_after_a_fully_verified_job`.
        {
            assert!(macos_should_eject(&JobResult::Verified));
            assert!(!macos_should_eject(&JobResult::WriteFailed(
                "device disconnected".into()
            )));
            assert!(!macos_should_eject(&JobResult::VerifyMismatch(42)));
            assert!(!macos_should_eject(&JobResult::VerifyFailed(
                "read-back failed".into()
            )));
        }
    }

    // ---- the real engine, proven against temp files (never a device) -------

    fn tmp_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp");
        f.write_all(bytes).expect("write");
        f.flush().expect("flush");
        f
    }

    fn sample_bytes(n: usize) -> Vec<u8> {
        // A deterministic, non-trivial pattern spanning several chunks.
        (0..n).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    /// Cancellation during write and readback, and revalidation failure.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn cooperative_cancel_and_revalidation_failures_never_claim_success() {
        // Phase `cooperative_cancel_stops_between_write_chunks_and_never_verifies`.
        {
            let data = sample_bytes(CHUNK * 3);
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let control = JobControl::new();
            let signal = control.clone();
            let mut phases = Vec::new();

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                None,
                None,
                &|| control.is_cancelled(),
                |progress| {
                    phases.push(progress.phase);
                    signal.cancel();
                },
            );

            assert_eq!(result, JobResult::Cancelled);
            assert_eq!(phases, vec![JobPhase::Writing]);
            assert_eq!(std::fs::metadata(dst.path()).unwrap().len(), CHUNK as u64);
        }

        // Phase `cooperative_cancel_stops_during_readback_without_claiming_success`.
        {
            let data = sample_bytes(CHUNK * 3);
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let control = JobControl::new();
            let signal = control.clone();
            let mut saw_verify = false;

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                None,
                None,
                &|| control.is_cancelled(),
                |progress| {
                    if progress.phase == JobPhase::Verifying {
                        saw_verify = true;
                        signal.cancel();
                    }
                },
            );

            assert_eq!(result, JobResult::Cancelled);
            assert!(saw_verify);
        }

        // Phase `target_revalidation_failure_still_reports_an_actionable_terminal_event`.
        {
            let mut request = req();
            request.target.name = "tilo-test-device-that-cannot-exist".into();
            let mut updates = Vec::new();

            let result =
                run_system_job(&request, &|| false, |progress| updates.push(progress), None);

            assert!(matches!(result, JobResult::WriteFailed(_)));
            let terminal = updates.last().expect("terminal progress event");
            assert_eq!(terminal.phase, JobPhase::Failed);
            assert!(
                terminal.error.as_deref().is_some_and(|message| {
                    message.contains("changed or was disconnected")
                        && message.contains("Could not write")
                }),
                "the UI receives a useful cause even when writing never starts"
            );
        }
    }

    /// Source size, replacement, identity and digest checks during the write.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_source_must_stay_the_reviewed_file_and_digest() {
        // Phase `reviewed_source_size_fails_closed_after_truncation_or_growth`.
        {
            let data = sample_bytes(4096);

            let truncated = tmp_with(&data[..2048]);
            let truncated_dst = tempfile::NamedTempFile::new().unwrap();
            let result = run_job_paths(truncated.path(), truncated_dst.path(), 4096, |_| {});
            assert!(
                matches!(result, JobResult::WriteFailed(ref message) if message.contains("size changed"))
            );

            let grown = tmp_with(&data);
            let grown_dst = tempfile::NamedTempFile::new().unwrap();
            let result = run_job_paths(grown.path(), grown_dst.path(), 2048, |_| {});
            assert!(
                matches!(result, JobResult::WriteFailed(ref message) if message.contains("size changed"))
            );
        }

        // Phase `replaced_source_is_rejected_before_destination_is_opened`.
        {
            let data = sample_bytes(4096);
            let root = tempfile::tempdir().unwrap();
            let selected = root.path().join("selected.img");
            std::fs::write(&selected, &data).unwrap();
            let held = std::fs::File::open(&selected).unwrap();
            let identity = SourceIdentity::from_file(&held)
                .unwrap()
                .expect("test platform exposes file identity");
            std::fs::rename(&selected, root.path().join("reviewed.img")).unwrap();
            std::fs::write(&selected, &data).unwrap();
            let dst = tmp_with(b"leave destination untouched");

            let result = run_job_paths_bound(
                &selected,
                dst.path(),
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| panic!("a replaced source must not produce write progress"),
            );
            assert!(
                matches!(result, JobResult::WriteFailed(ref message) if message.contains("replaced"))
            );
            assert_eq!(
                std::fs::read(dst.path()).unwrap(),
                b"leave destination untouched"
            );

            let absent_destination = root.path().join("not-created.img");
            let result = run_job_paths_bound(
                &selected,
                &absent_destination,
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| panic!("a replaced source must not produce write progress"),
            );
            assert!(matches!(result, JobResult::WriteFailed(_)));
            assert!(!absent_destination.exists());

            // The held identity still authorizes the original file at its new path.
            let result = run_job_paths_bound(
                &root.path().join("reviewed.img"),
                dst.path(),
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| {},
            );
            assert_eq!(result, JobResult::Verified);
            assert_eq!(std::fs::read(dst.path()).unwrap(), data);
        }

        // Phase `descriptor_writer_rejects_another_file_with_identical_bytes_before_mutation`.
        {
            let data = sample_bytes(4096);
            let reviewed = tmp_with(&data);
            let replacement = tmp_with(&data);
            let mut target = tmp_with(b"leave destination untouched");
            target.as_file_mut().seek(SeekFrom::Start(7)).unwrap();
            let mut request = req();
            request.source.path = replacement.path().to_string_lossy().into_owned();
            request.source.size = data.len() as u64;
            request.source.identity = Some(
                SourceIdentity::from_file(reviewed.as_file())
                    .unwrap()
                    .expect("test platform exposes file identity"),
            );
            request.source.installable = false;

            let result = run_job_file_bound(&request, target.as_file_mut(), &|| false, |_| {
                panic!("a replaced source must not produce write progress");
            });

            assert!(matches!(result, JobResult::WriteFailed(_)));
            assert_eq!(target.as_file_mut().stream_position().unwrap(), 7);
            assert_eq!(
                std::fs::read(target.path()).unwrap(),
                b"leave destination untouched"
            );
        }

        // Phase `catalog_digest_is_checked_during_the_write`.
        {
            let data = sample_bytes(4096);
            let src = tmp_with(&data);
            let dst = tmp_with(b"leave destination untouched");
            let wrong_digest = "0".repeat(64);

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                SourceIdentity::from_file(src.as_file()).unwrap(),
                Some(&wrong_digest),
                &|| false,
                |_| {},
            );
            assert!(
                matches!(result, JobResult::WriteFailed(ref message) if message.contains("signed release catalog"))
            );
            assert_eq!(
                std::fs::read(dst.path()).unwrap(),
                b"leave destination untouched"
            );
        }
    }

    #[test]
    fn handoff_apply_selects_root_partition_and_copies_onto_a_mounted_fixture() {
        // Phase `select_tilo_root_partition_prefers_child_linux_fs`.
        {
            let lsblk = "\
/dev/loop7\n\
/dev/loop7p1 vfat\n\
/dev/loop7p2 btrfs\n\
/dev/loop7p3 swap\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop7", lsblk).as_deref(),
                Some("/dev/loop7p2")
            );
        }

        // Phase `select_tilo_root_partition_falls_back_to_whole_device`.
        {
            let lsblk = "/dev/loop9 ext4\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop9", lsblk).as_deref(),
                Some("/dev/loop9")
            );
            assert_eq!(
                select_tilo_root_partition("/dev/loop9", "/dev/loop9\n"),
                None
            );
        }

        // A partition node whose type has not arrived yet is not proof that
        // the disk has no installed root. A vfat-only listing is complete.
        assert!(root_listing_waits_for_filesystem_type(
            "/dev/sda",
            "/dev/sda\n/dev/sda1\n/dev/sda2 vfat\n"
        ));
        assert!(!root_listing_waits_for_filesystem_type(
            "/dev/sda",
            "/dev/sda\n/dev/sda1 vfat\n"
        ));

        // The Orange Pi disk is a loader hole, a FAT ESP, then the btrfs root.
        // The hole has no filesystem type; the write must land on the root.
        {
            let lsblk = "\
/dev/loop0\n\
/dev/loop0p1\n\
/dev/loop0p2 vfat\n\
/dev/loop0p3 btrfs\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop0", lsblk).as_deref(),
                Some("/dev/loop0p3")
            );
        }

        // An ext4 boot partition listed before the btrfs root is not the
        // installed system. The handoff has to land on the root.
        {
            let lsblk = "\
/dev/sda\n\
/dev/sda1 vfat\n\
/dev/sda2 ext4\n\
/dev/sda3 btrfs\n";
            assert_eq!(
                select_tilo_root_partition("/dev/sda", lsblk).as_deref(),
                Some("/dev/sda3")
            );
        }

        // The installer's ext4 boot partition is not a root. With no btrfs
        // beside it, the handoff stays out of that partition.
        {
            let lsblk = "\
/dev/sda\n\
/dev/sda1 vfat\n\
/dev/sda2 ext4 TILO_BOOT\n";
            assert_eq!(select_tilo_root_partition("/dev/sda", lsblk), None);
        }

        // The boot partition and the btrfs root together: skip the boot
        // partition and use the root.
        {
            let lsblk = "\
/dev/sda\n\
/dev/sda1 vfat\n\
/dev/sda2 ext4 TILO_BOOT\n\
/dev/sda3 btrfs\n";
            assert_eq!(
                select_tilo_root_partition("/dev/sda", lsblk).as_deref(),
                Some("/dev/sda3")
            );
        }

        // `loop1` is a prefix of `loop10`. The later disk's filesystem is
        // not this disk's root.
        {
            let lsblk = "\
/dev/loop1\n\
/dev/loop10 btrfs\n\
/dev/loop1p1 btrfs\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop1", lsblk).as_deref(),
                Some("/dev/loop1p1")
            );
        }

        // Phase `apply_handoff_stage_onto_mounted_root_copies_etc_and_var`.
        {
            let staged = tempfile::tempdir().unwrap();
            let conn = staged.path().join("etc/NetworkManager/system-connections");
            std::fs::create_dir_all(&conn).unwrap();
            std::fs::write(conn.join("tilo-setup.nmconnection"), b"ssid=studio\n").unwrap();
            std::fs::create_dir_all(staged.path().join("var/lib/tilo")).unwrap();
            std::fs::write(staged.path().join("var/lib/tilo/device.key"), b"secret").unwrap();

            let mnt = tempfile::tempdir().unwrap();
            let etc = mnt.path().join("ostree/deploy/tilo/deploy/abcd.0/etc");
            let var = mnt.path().join("ostree/deploy/tilo/var");
            std::fs::create_dir_all(&etc).unwrap();
            std::fs::create_dir_all(&var).unwrap();

            apply_handoff_stage_onto_mounted_root(mnt.path(), staged.path()).unwrap();
            assert_eq!(
                std::fs::read(
                    etc.join("NetworkManager/system-connections/tilo-setup.nmconnection")
                )
                .unwrap(),
                b"ssid=studio\n"
            );
            assert_eq!(
                std::fs::read(var.join("lib/tilo/device.key")).unwrap(),
                b"secret"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installed_root_mount_failure_does_not_hide_the_handoff() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("marker"), b"wifi-secret").unwrap();
        let device = tempfile::NamedTempFile::new().unwrap();
        let before = std::fs::read(device.path()).unwrap();
        let error = copy_handoff_onto_installed_root("/dev/tilo-no-such-root", staged.path())
            .expect_err("a missing installed root must fail the write");
        assert!(
            error.to_string().contains("could not mount"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(device.path()).unwrap(), before);
        assert!(
            !before
                .windows(20)
                .any(|window| window == b"TILO_SETUP_HANDOFF")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_image_file_is_not_a_successful_slack_handoff_when_it_cannot_be_inspected() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("marker"), b"wifi-secret").unwrap();
        let mut bytes = vec![0_u8; 520];
        bytes[512..520].copy_from_slice(b"EFI PART");
        let device = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(device.path(), &bytes).unwrap();
        let error = handoff_when_root_was_not_found(device.path(), 520, 520, staged.path())
            .expect_err("a GPT image that cannot be loop-mounted must fail");
        assert!(
            error.to_string().contains("were not applied"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(device.path()).unwrap(), bytes);

        let mut qcow = vec![0_u8; 16];
        qcow[..4].copy_from_slice(b"QFI\xfb");
        let qcow_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(qcow_file.path(), &qcow).unwrap();
        let error = handoff_when_root_was_not_found(qcow_file.path(), 16, 16, staged.path())
            .expect_err("a qcow2 image that cannot be loop-mounted must fail");
        assert!(
            error.to_string().contains("were not applied"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(qcow_file.path()).unwrap(), qcow);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_mountpoint_listing_means_the_disk_is_busy() {
        assert!(!mountpoint_listing_is_busy(""));
        assert!(!mountpoint_listing_is_busy("\n"));
        assert!(mountpoint_listing_is_busy("/run/media/tilo\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn image_customization_does_not_fake_a_home_file_slack_success() {
        let image = tempfile::NamedTempFile::new().unwrap();
        let before = b"not-a-disk-image";
        std::fs::write(image.path(), before).unwrap();
        let staged = tempfile::tempdir().unwrap();
        std::fs::create_dir(staged.path().join(crate::handoff::ACCOUNT_HOME_FILES)).unwrap();
        std::fs::write(
            staged
                .path()
                .join(crate::handoff::ACCOUNT_HOME_FILES)
                .join("SETUP.md"),
            b"private setup",
        )
        .unwrap();
        let error = apply_handoff_stage_to_image_file(image.path(), staged.path())
            .expect_err("a home-only request needs an installed root, not empty installer slack");
        assert!(error.to_string().contains("installed"), "{error}");
        assert_eq!(std::fs::read(image.path()).unwrap(), before);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn image_file_is_not_rejected_before_the_loop_mount() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("marker"), b"wifi-secret").unwrap();
        let device = tempfile::NamedTempFile::new().unwrap();
        let before = b"not-a-disk-image";
        std::fs::write(device.path(), before).unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        let observed = std::cell::Cell::new(None);
        let result = block_devices::installed_root::with_written_root_observed(
            device.path(),
            mountpoint.path(),
            |mnt| apply_handoff_stage_onto_mounted_root(mnt, staged.path()),
            |attached| observed.set(Some(attached)),
        );
        // A message-based check mistakes a failed post-attachment partition
        // probe for rejection before losetup. Observe the real bounded attempt.
        let attached = observed
            .get()
            .expect("a regular file must reach the loop attachment attempt");
        if attached {
            let error = result.expect_err("an attached file without partitions must fail closed");
            assert!(error.to_string().contains("installed root"), "{error}");
        } else {
            assert!(
                matches!(result, Ok(false)),
                "a failed loop attachment cannot report a copied root: {result:?}"
            );
        }
        assert_eq!(std::fs::read(device.path()).unwrap(), before);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_writer_provisions_large_home_media_and_embeds_profile_in_gpt_gap() {
        use std::os::unix::fs::FileExt;
        let (_source_dir, source_path, source) = crate::media_layout::tests::disk();
        source
            .set_len(4 * 1024 * 1024)
            .expect("exact installer fixture");
        let mut target = tempfile::tempfile().expect("target fixture");
        target
            .set_len(64 * 1024 * 1024)
            .expect("large media fixture");
        let mut request = req();
        request.source.path = source_path.to_string_lossy().into_owned();
        request.source.size = 4 * 1024 * 1024;
        request.source.identity = None;
        request.source.expected_sha256 = None;
        request.source.installable = true;
        request.target.size = 64 * 1024 * 1024;
        request.target.stable_id = Some("usb:fixture".into());
        request.provision_migration_data = true;
        assert_eq!(
            run_job_file_bound(&request, &mut target, &|| false, |_| {}),
            JobResult::Verified
        );
        let binding = crate::media_layout::binding(&target, request.target.size, "usb:fixture")
            .expect("actual writer GPT");
        let mut profile = vec![0; PROFILE_BLOCK_SIZE];
        target
            .read_exact_at(&mut profile, binding.profile_offset)
            .expect("profile gap");
        assert_eq!(
            profile,
            encode_profile_block(&request.profile).expect("canonical profile")
        );
        let mut boot = vec![0; 19];
        target
            .read_exact_at(&mut boot, 2048 * 512)
            .expect("boot payload");
        assert_eq!(boot, b"signed boot payload");
        let mut old = vec![0; PROFILE_BLOCK_SIZE];
        target
            .read_exact_at(&mut old, request.target.size - PROFILE_BLOCK_SIZE as u64)
            .expect("relocated backup");
        assert_ne!(old, profile);
    }
}
