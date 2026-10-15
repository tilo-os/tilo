//! Target-owner archive continuation on the authenticated installed-copy gate.
//! The receive grant, rather than a mutable receipt or synthetic setup, owns
//! the operation and the live channel throughout review and adoption.
use super::AuthenticatedCopyReceive;
use crate::setup::{
    destination_security,
    flatpak_transfer::portable::{ApplicationImport, CancellableOfflineRunner},
    migration::{PinnedDirectory, changed, check_cancel},
    portable_home_import::{
        AdoptionPreview, AdoptionResult, AdoptionWorker, HomeImportGrants, PortableHomeReview,
    },
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;

fn private_child(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let pin = PinnedDirectory::open_absolute(parent)?;
    pin.validate_destination()?;
    match crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(name)) {
        Ok(metadata) => {
            let child = pin.open_child(std::ffi::OsStr::new(name), &metadata)?;
            child.validate_destination()?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            pin.create_child(std::ffi::OsStr::new(name))?;
        }
        Err(error) => return Err(error),
    }
    pin.validate_destination()?;
    Ok(parent.join(name))
}
fn workspace(home: &Path, kind: &str, operation: &str) -> io::Result<PathBuf> {
    let mut path = home.to_path_buf();
    for component in [".local", "state", "tilo", kind, operation] {
        path = private_child(&path, component)?;
    }
    let pin = PinnedDirectory::open_absolute(&path)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(path)
}

pub(super) struct InstalledCopyReceiveSession {
    grant: AuthenticatedCopyReceive,
    home: Option<PortableHomeReview>,
    applications: Option<ApplicationImport>,
    applications_workspace: PathBuf,
    adopting: Option<AdoptionWorker>,
    applications_restored: bool,
    cancel: Arc<AtomicBool>,
}
impl InstalledCopyReceiveSession {
    pub(super) fn review(
        mut grant: AuthenticatedCopyReceive,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        grant.check()?;
        check_cancel(&cancel)?;
        // Authority fixes the actual activated target owner; source UID is never
        // accepted as the receiver's filesystem or application owner.
        // SAFETY: these process credential queries take no pointers or arguments.
        let effective_uid = rustix::process::geteuid().as_raw();
        // SAFETY: this query observes this process real UID without side effects.
        let real_uid = rustix::process::getuid().as_raw();
        if effective_uid != grant.target_uid()
            || real_uid != grant.target_uid()
            || grant.target_uid() == 0
            || uuid::Uuid::parse_str(grant.operation())
                .map_err(io::Error::other)?
                .to_string()
                != grant.operation()
        {
            return Err(changed("Installed copy target owner changed"));
        }
        crate::setup::home_admission::installed()?;
        let home_path = grant.home().to_path_buf();
        let operation = grant.operation().to_owned();
        let home_workspace = workspace(&home_path, "portable-home", &operation)?;
        let application_operation = grant
            .applications_expectation()
            .map(|expected| {
                expected.validate()?;
                if expected.setup_binding != grant.setup_binding() || !grant.applications_selected()
                {
                    return Err(changed(
                        "Application archive belongs to another installed operation",
                    ));
                }
                Ok(expected.operation.clone())
            })
            .transpose()?;
        let applications_workspace = workspace(
            &home_path,
            "portable-applications",
            application_operation.as_deref().unwrap_or(&operation),
        )?;
        grant.check()?;
        let home_file = grant.take_home_file()?;
        let home = match (grant.home_expectation(), home_file) {
            (Some(expected), Some(file)) => {
                if expected.operation != operation
                    || expected.setup_binding != grant.setup_binding()
                {
                    return Err(changed(
                        "Home archive belongs to another installed operation",
                    ));
                }
                Some(PortableHomeReview::review_received(
                    HomeImportGrants {
                        package: grant.home_package_path()?.to_path_buf(),
                        home: home_path,
                        workspace: home_workspace,
                    },
                    expected,
                    file,
                    grant.secret(),
                    &cancel,
                    grant.applications_selected(),
                )?)
            }
            (None, None) => None,
            _ => return Err(changed("Installed Home archive delivery is incomplete")),
        };
        grant.check()?;
        let application_file = grant.take_applications_file()?;
        let applications = match (grant.applications_expectation(), application_file) {
            (Some(expected), Some(file)) => {
                if expected.setup_binding != grant.setup_binding() || !grant.applications_selected()
                {
                    return Err(changed(
                        "Application archive belongs to another installed operation",
                    ));
                }
                if expected.consumer_protocol == 2 {
                    crate::setup::applications_admission::installed()?;
                }
                Some(ApplicationImport::review_received(
                    expected,
                    file,
                    grant.secret(),
                    &applications_workspace.join("imported"),
                    &cancel,
                )?)
            }
            (None, None) if !grant.applications_selected() => None,
            _ => {
                return Err(changed(
                    "Installed applications archive delivery is incomplete",
                ));
            }
        };
        if home.is_none() && applications.is_none() {
            return Err(changed("Installed copy delivery has no selected archives"));
        }
        grant.check()?;
        check_cancel(&cancel)?;
        Ok(Self {
            grant,
            home,
            applications,
            applications_workspace,
            adopting: None,
            applications_restored: false,
            cancel,
        })
    }
    pub(super) fn prepare_home(&mut self) -> io::Result<Option<AdoptionPreview>> {
        if self.adopting.is_some() {
            return Err(changed("Home adoption is already running"));
        }
        self.grant.check()?;
        check_cancel(&self.cancel)?;
        let result = self
            .home
            .as_mut()
            .map(|home| home.prepare(Zeroizing::new(self.grant.secret().to_vec()), &self.cancel))
            .transpose()?;
        self.grant.check()?;
        Ok(result)
    }
    pub(super) fn start_adoption(&mut self) -> io::Result<()> {
        self.grant.check()?;
        check_cancel(&self.cancel)?;
        if self.adopting.is_some() {
            return Err(changed("Home adoption is already running"));
        }
        self.adopting = Some(
            self.home
                .as_mut()
                .ok_or_else(|| changed("No Home archive was selected"))?
                .start_adoption(&self.cancel)?,
        );
        Ok(())
    }
    pub(super) fn poll_adoption(&mut self) -> io::Result<Option<Result<AdoptionResult, String>>> {
        if let Err(error) = self.grant.check() {
            self.cancel();
            return Err(error);
        }
        let result = self.adopting.as_mut().and_then(AdoptionWorker::try_next);
        if result.is_some() {
            self.adopting = None;
        }
        Ok(result)
    }
    pub(super) fn restore_applications(&mut self) -> io::Result<usize> {
        self.grant.check()?;
        check_cancel(&self.cancel)?;
        if self.adopting.is_some() {
            return Err(changed("Home adoption has not stopped"));
        }
        let Some(applications) = &mut self.applications else {
            return Ok(0);
        };
        let runner = CancellableOfflineRunner(self.cancel.clone());
        let count = if let Some(home) = &mut self.home {
            applications.restore_after_home(
                home,
                &runner,
                self.grant.home(),
                &self.applications_workspace,
                &self.cancel,
            )?
        } else {
            applications.adopt_user_state(
                &runner,
                self.grant.home(),
                &self.applications_workspace,
                &self.cancel,
            )?;
            applications.restore(&runner, self.grant.home(), &self.cancel)?
        };
        self.grant.check()?;
        self.applications_restored = true;
        Ok(count)
    }
    pub(super) fn operation(&self) -> &str {
        self.grant.operation()
    }
    pub(super) fn selected(&self) -> (bool, bool) {
        (self.home.is_some(), self.applications.is_some())
    }
    pub(super) fn receive_command(
        &self,
        timeout: std::time::Duration,
    ) -> io::Result<Option<super::dispatcher::CopyCommand>> {
        self.grant.receive_command(timeout)
    }
    pub(super) fn send_event(&self, event: &super::dispatcher::CopyEvent) -> io::Result<()> {
        self.grant.send_event(event)
    }
    pub(super) fn verify_complete(&mut self) -> io::Result<super::dispatcher::CopyEvent> {
        self.grant.check()?;
        check_cancel(&self.cancel)?;
        if self.adopting.is_some() {
            return Err(changed("Home adoption is still running"));
        }
        let runner = CancellableOfflineRunner(self.cancel.clone());
        if let Some(applications) = &mut self.applications {
            if !self.applications_restored {
                return Err(changed("Selected applications have not been restored"));
            }
            applications.verify_received_completion(
                &runner,
                self.grant.home(),
                &self.applications_workspace,
                self.home.is_some(),
                &self.cancel,
            )?;
            if let Some(home) = &mut self.home {
                applications.verify_home_for_applications(
                    home,
                    &runner,
                    self.grant.home(),
                    &self.applications_workspace,
                    &self.cancel,
                )?;
            }
        } else if let Some(home) = &mut self.home {
            home.verify_completed_adoption(&self.cancel)?;
        }
        self.grant.check()?;
        Ok(super::dispatcher::CopyEvent::Complete {
            operation: self.grant.operation().to_owned(),
            home_sha256: self
                .grant
                .home_expectation()
                .map(|expected| expected.ciphertext_sha256),
            applications_sha256: self
                .grant
                .applications_expectation()
                .map(|expected| expected.ciphertext_sha256),
        })
    }
    pub(super) fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = &self.adopting {
            worker.cancel();
        }
    }
    /// Return true only after the actual promotion worker reports terminal state.
    /// A timeout leaves the session and receive grant held by the caller.
    pub(super) fn drain(&mut self, timeout: std::time::Duration) -> bool {
        self.cancel();
        let Some(deadline) = std::time::Instant::now().checked_add(timeout) else {
            return false;
        };
        while self.adopting.is_some() {
            if self
                .adopting
                .as_mut()
                .and_then(AdoptionWorker::try_next)
                .is_some()
            {
                self.adopting = None;
                break;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }
}
