//! Root-native source worker; source custody remains inseparable from supervision.
//! No public dispatcher or frontend capability is enabled by this module.
use super::{
    home_capture::TrustedCaptureService, installed_retained_actor::InstalledRetainedLaunchInput,
};
use crate::retained_source::{
    projection::SourceProjection,
    workspace::{self, ControlStream as UnixStream},
};
use launcher::{
    owned_service::{
        self, RetainedIoAdmission, RetainedIoAuthority, RetainedIoCredentials, RetainedNativeOwner,
        ServiceAuthority, ServiceCancellation, ServiceLaunch, ServiceLeaseKey, SupervisedRun,
    },
    run::{IdentitySource, LaunchMode, RunOptions},
};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
    thread::JoinHandle,
    time::Duration,
};
pub(super) use workspace::working_parent;
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

struct Authority {
    input: Arc<InstalledRetainedLaunchInput>,
    cancel: Arc<AtomicBool>,
    context: [u8; 32],
    replacement: Option<Arc<super::retained_selected_edit::RootSelectedReplacementSeed>>,
    native_owner: Mutex<Option<RetainedNativeOwner>>,
}
impl ServiceAuthority for Authority {
    fn check(&self) -> io::Result<()> {
        self.input.revalidate(&self.cancel)?;
        if let Some(seed) = &self.replacement {
            if !seed.uses_input(&self.input) {
                return Err(refused());
            }
            seed.check()?;
        }
        Ok(())
    }
}
impl RetainedIoAuthority for Authority {
    fn native_history_verifier(&self) -> io::Result<Option<File>> {
        self.check()?;
        self.replacement
            .as_ref()
            .map(|seed| seed.verifier_file())
            .transpose()
    }

    fn selected_replacement_permit(&self) -> io::Result<Option<File>> {
        self.check()?;
        self.replacement
            .as_ref()
            .map(|seed| seed.permit_file())
            .transpose()
    }

    fn source_credentials(&self) -> io::Result<RetainedIoCredentials> {
        self.check()?;
        let facts = self.input.original();
        RetainedIoCredentials::admitted(
            facts.uid(),
            facts.primary_gid(),
            facts.supplementary_gids(),
        )
    }
    fn native_owner_sha256(&self) -> io::Result<[u8; 32]> {
        self.input.native_owner_sha256(&self.cancel)
    }
    fn removal_namespace(&self) -> io::Result<File> {
        self.check()?;
        self.input.removal_namespace(&self.cancel)
    }
    fn retain_native_owner(&self, owner: RetainedNativeOwner) -> io::Result<()> {
        let mut slot = self
            .native_owner
            .lock()
            .map_err(|_| io::Error::other("Native custody poisoned"))?;
        if slot.is_some() {
            return Err(refused());
        }
        *slot = Some(owner);
        Ok(())
    }
    fn confirm_native_retirement(&self) -> io::Result<()> {
        let slot = self
            .native_owner
            .lock()
            .map_err(|_| io::Error::other("Native custody poisoned"))?;
        slot.as_ref().ok_or_else(refused)?.confirm_retirement()
    }
    fn target_uid(&self) -> u32 {
        self.input.binding().target_uid
    }
    fn context_sha256(&self) -> [u8; 32] {
        self.context
    }
}
/// Must live on the root controller's existing background job, never UI paint.
/// Dropping cancels and joins; a timeout cannot release the protected source.
mod pending_handoff;
pub(super) use pending_handoff::PendingHandoff;
struct SourceLifetimeGuard(Arc<AtomicBool>);
impl Drop for SourceLifetimeGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
pub(super) struct RetainedSourceWorker {
    source_live: Arc<AtomicBool>,
    read_input: Option<Arc<InstalledRetainedLaunchInput>>,
    replacement_seed: Option<Arc<super::retained_selected_edit::RootSelectedReplacementSeed>>,
    cancellation: ServiceCancellation,
    job: Option<JoinHandle<SupervisedRun>>,
    key: ServiceLeaseKey,
    parent: Option<File>,
    working: Option<tempfile::TempDir>,
    authority: Option<Arc<dyn RetainedIoAuthority>>,
    selected_file: Option<File>,
    endpoint: Option<owned_service::ServiceEndpoint>,
    stopped: bool,
    source_view: [u8; 32],
}
impl RetainedSourceWorker {
    pub(super) fn start(
        input: Arc<InstalledRetainedLaunchInput>,
        name: &OsStr,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<(Self, UnixStream)> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        input.revalidate(&cancel)?;
        let projection =
            SourceProjection::selected(input.directory(), name, &input.binding().context_sha256)?;
        Self::start_source(input, projection, false, cancel, None)
    }
    /// Admit only the actor's actual protected Home projection, never a path
    /// or a directory inferred from a selected file.
    pub(super) fn start_home(
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<(Self, UnixStream)> {
        input.revalidate(&cancel)?;
        let projection =
            SourceProjection::home(input.directory(), &input.binding().context_sha256)?;
        Self::start_source(input, projection, true, cancel, None)
    }
    /// Fresh editor-only source bootstrap, holding the genuine root seed for
    /// the entire service lifetime. Ordinary Home has no replacement permit.
    pub(super) fn start_home_for_editor(
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        seed: Arc<super::retained_selected_edit::RootSelectedReplacementSeed>,
    ) -> io::Result<(Self, UnixStream)> {
        input.revalidate(&cancel)?;
        if !seed.uses_input(&input) {
            return Err(refused());
        }
        seed.check()?;
        let projection =
            SourceProjection::home(input.directory(), &input.binding().context_sha256)?;
        Self::start_source(input, projection, true, cancel, Some(seed))
    }
    fn start_source(
        input: Arc<InstalledRetainedLaunchInput>,
        projection: SourceProjection,
        home: bool,
        cancel: Arc<AtomicBool>,
        replacement: Option<Arc<super::retained_selected_edit::RootSelectedReplacementSeed>>,
    ) -> io::Result<(Self, UnixStream)> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        input.revalidate(&cancel)?;
        let source_view = projection.context_sha256();
        let file = projection.duplicate_file()?;
        let read_input = input.clone();
        let replacement_seed = replacement.clone();
        let authority: Arc<dyn RetainedIoAuthority> = Arc::new(Authority {
            input,
            cancel,
            context: source_view,
            replacement,
            native_owner: Mutex::new(None),
        });
        let selected_file = file.try_clone()?;
        let owner_authority = Arc::clone(&authority);
        let service = TrustedCaptureService::native()?;
        let admission = RetainedIoAdmission::admit(file, authority)?
            .with_native_runtime(&service.root, &service.publisher)?
            .with_installation(service.installed.clone(), &service.service_publisher)?;
        let parent = working_parent()?;
        let working = workspace::temporary_directory("tilo-retained-source-")?;
        let key = ServiceLeaseKey::default();
        let cancellation = ServiceCancellation::default();
        let (send, receive) = mpsc::sync_channel(1);
        // Publish only the canonical endpoint, then return immediately so
        // supervision remains active during provider connection/readiness IO.
        let connected = Box::new(move |endpoint: owned_service::ServiceEndpoint| {
            send.send(endpoint)
                .map_err(|_| io::ErrorKind::BrokenPipe.into())
        });
        let launch = ServiceLaunch {
            manifest: service.manifest.clone(),
            payload: service.payload.clone(),
            options: RunOptions {
                no_services: true,
                expected_package_hash: Some(service.package_hash.clone()),
                app_id: Some(owned_service::RETAINED_IO_PACKAGE_ID.into()),
                data_home: Some(working.path().to_path_buf()),
                identity: IdentitySource::None,
                mode: LaunchMode::Service {
                    passthrough_argv: vec![
                        if home {
                            owned_service::RETAINED_HOME_IO_WORKER_ARGUMENT
                        } else {
                            owned_service::RETAINED_IO_WORKER_ARGUMENT
                        }
                        .into(),
                    ],
                },
                ..RunOptions::default()
            },
        };
        let child_cancel = cancellation.clone();
        let child_key = key.clone();
        let source_live = Arc::new(AtomicBool::new(true));
        let thread_live = source_live.clone();
        let job = std::thread::Builder::new()
            .name("retained-source-owner".into())
            .spawn(move || {
                let _source_lifetime = SourceLifetimeGuard(thread_live);
                owned_service::run_retained_io_connected(
                    launch,
                    admission,
                    child_cancel,
                    child_key,
                    Some(connected),
                )
            })?;
        let mut owner = Self {
            source_live,
            read_input: Some(read_input),
            replacement_seed,
            cancellation,
            job: Some(job),
            key,
            parent: Some(parent),
            working: Some(working),
            authority: Some(owner_authority),
            selected_file: Some(selected_file),
            endpoint: None,
            stopped: false,
            source_view,
        };
        if let Ok(endpoint) = receive.recv_timeout(Duration::from_secs(12)) {
            owner.endpoint = Some(endpoint);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                match owner
                    .endpoint
                    .as_ref()
                    .ok_or(io::ErrorKind::NotConnected)?
                    .connect()
                {
                    Ok(stream) => return Ok((owner, stream)),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                        ) && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        owner.retire()?;
                        return Err(error);
                    }
                }
            }
        } else {
            let cleanup = owner.retire();
            drop(receive);
            Err(cleanup.err().unwrap_or_else(|| {
                io::Error::other("Retained source did not establish its private endpoint")
            }))
        }
    }
    /// Derived from the exact root-selected file identity and certified context.
    pub(super) fn source_view_sha256(&self) -> [u8; 32] {
        self.source_view
    }
    /// Actual live service observation; retaining a context digest or endpoint
    /// Option alone is insufficient. The caller retains this owned worker.
    pub(super) fn check_current_custody(&self) -> io::Result<owned_service::ServiceCustody> {
        if self.stopped || self.job.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(refused());
        }
        let endpoint = self.endpoint.as_ref().ok_or_else(refused)?;
        let before = endpoint.custody()?;
        self.authority.as_ref().ok_or_else(refused)?.check()?;
        let after = endpoint.custody()?;
        if before != after || self.job.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(refused());
        }
        Ok(after)
    }
    pub(super) fn retire(&mut self) -> io::Result<()> {
        self.cancellation.cancel();
        if !self.stopped {
            let run = self.job.take().map(|job| job.join());
            let path = self
                .working
                .as_ref()
                .ok_or_else(|| io::Error::other("Source workspace missing"))?
                .path();
            let receipt = run.as_ref().is_some_and(|result| {
                result.as_ref().is_ok_and(|run| {
                    run.shutdown.as_ref().is_some_and(|receipt| {
                        receipt.matches(&self.key, path, owned_service::RETAINED_IO_PACKAGE_ID)
                    }) || run.result.as_ref().is_ok_and(|receipt| {
                        receipt.matches(&self.key, path, owned_service::RETAINED_IO_PACKAGE_ID)
                    })
                })
            });
            let kernel_stop = self
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.observed_stopped())
                .transpose()?
                .unwrap_or(false);
            self.stopped = receipt || kernel_stop;
            if !self.stopped {
                return Err(io::Error::other(
                    "Retained source shutdown was not observed; custody retained",
                ));
            }
        }
        if self
            .selected_file
            .as_ref()
            .ok_or_else(refused)?
            .metadata()?
            .is_dir()
        {
            self.authority
                .as_ref()
                .ok_or_else(refused)?
                .confirm_native_retirement()?;
        }
        self.endpoint.take();
        self.selected_file.take();
        self.authority.take();
        self.read_input.take();
        self.replacement_seed.take();
        Ok(())
    }
}
impl Drop for RetainedSourceWorker {
    fn drop(&mut self) {
        // Missing authority is not evidence that a launched worker stopped.
        if !self.stopped || self.authority.is_some() {
            let _ = self.retire();
        }
        if !self.stopped || self.authority.is_some() {
            // No observed exit or unresolved captures means no source release.
            // Root controller retains this owner on an error; if it unwinds,
            // keep exact custody until process teardown instead of erasing it.
            std::mem::forget(self.endpoint.take());
            std::mem::forget(self.selected_file.take());
            std::mem::forget(self.authority.take());
            std::mem::forget(self.read_input.take());
            std::mem::forget(self.replacement_seed.take());
            std::mem::forget(self.parent.take());
            if let Some(working) = self.working.take() {
                let _ = working.keep();
            }
        }
    }
}
#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    };

    // No service or source authority is minted by this fixture. A real owned
    // thread returns without any kernel stop receipt, or actually panics. The
    // production retirement path must preserve its held source/control files.
    fn unknown_worker(panic: bool) -> RetainedSourceWorker {
        let working = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let parent = File::open(working.path()).unwrap();
        let selected = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(working.path().join("selected"))
            .unwrap();
        let job = std::thread::spawn(move || {
            assert!(!panic, "controlled source supervisor panic");
            SupervisedRun {
                result: Err(launcher::run::RunError::Io(io::Error::other(
                    "no kernel stop observation",
                ))),
                shutdown: None,
            }
        });
        RetainedSourceWorker {
            source_live: Arc::new(AtomicBool::new(false)),
            read_input: None,
            replacement_seed: None,
            cancellation: ServiceCancellation::default(),
            job: Some(job),
            key: ServiceLeaseKey::default(),
            parent: Some(parent),
            working: Some(working),
            authority: None,
            selected_file: Some(selected),
            endpoint: None,
            stopped: false,
            source_view: [0; 32],
        }
    }
    #[test]
    fn missing_or_panicked_stop_keeps_source_and_control_custody_on_retries() {
        for panic in [false, true] {
            let mut owner = unknown_worker(panic);
            owner.source_view = [9; 32];
            assert!(
                owner.check_current_custody().is_err(),
                "source digest cannot replace actual endpoint custody"
            );
            let path = owner.working.as_ref().unwrap().path().to_owned();
            let expected = owner.selected_file.as_ref().unwrap().metadata().unwrap();
            assert!(owner.retire().is_err());
            assert!(
                owner.retire().is_err(),
                "consumed join handle cannot become successful stop"
            );
            assert!(!owner.stopped);
            assert!(
                owner.check_current_custody().is_err(),
                "unknown stop never becomes a live source epoch"
            );
            let held = owner.selected_file.as_ref().unwrap().metadata().unwrap();
            assert_eq!((held.dev(), held.ino()), (expected.dev(), expected.ino()));
            assert!(owner.parent.is_some());
            assert!(path.join("selected").exists());
            // The fixture spawned no process, so release only its owned test
            // files explicitly; do not manufacture a production stop receipt.
            owner.selected_file.take();
            owner.parent.take();
            owner.working.take().unwrap().close().unwrap();
        }
    }
    #[test]
    fn drop_after_unknown_stop_keeps_actual_file_descriptors_and_working_tree() {
        let mut owner = unknown_worker(true);
        let path = owner.working.as_ref().unwrap().path().to_owned();
        let selected_fd = owner.selected_file.as_ref().unwrap().as_raw_fd();
        let parent_fd = owner.parent.as_ref().unwrap().as_raw_fd();
        let expected = owner.selected_file.as_ref().unwrap().metadata().unwrap();
        assert!(owner.retire().is_err());
        drop(owner);
        assert!(
            path.join("selected").exists(),
            "unknown cleanup cannot remove control ancestry"
        );
        let mut observed: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(selected_fd, &raw mut observed) }, 0);
        assert_eq!(
            (observed.st_dev, observed.st_ino),
            (expected.dev(), expected.ino())
        );
        assert_eq!(unsafe { libc::fstat(parent_fd, &raw mut observed) }, 0);
        // These descriptors were deliberately retained by this exact fixture;
        // close only those owned objects after observing the regression.
        assert_eq!(unsafe { libc::close(selected_fd) }, 0);
        assert_eq!(unsafe { libc::close(parent_fd) }, 0);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[path = "retained_source_worker/history_read_handoff.rs"]
mod history_read_handoff;
pub use history_read_handoff::{HistoryReadHandoff, HistoryReadPublisher};
