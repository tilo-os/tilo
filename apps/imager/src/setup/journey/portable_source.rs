//! Another-device migration consent. A held, source-bound review is kept until
//! the media writer consumes it. Original-source Copy authority is captured only
//! for explicit offline continuation; ordinary immutable transfer is unchanged.
use super::super::{
    flatpak_transfer::{NativeRunner, portable::PortableFlatpakSource},
    migration_intent::MigrationIntent,
    portable_home_media::HomeSource,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
};

#[path = "portable_source/mounted_permission.rs"]
mod mounted_permission;

use super::portable_source::Choices;
use imager_backend::journey_media::SourceGrant;
#[derive(Clone)]
struct MountedContext {
    grant: Arc<SourceGrant>,
    choices: Choices,
}
struct Reviewed {
    original: Option<Arc<OriginalSource>>,
    grant: Arc<SourceGrant>,
    choices: Choices,
    home: Option<Arc<HomeSource>>,
    applications: Option<Arc<PortableFlatpakSource>>,
}

/// Local original-source authority minted only from a completed, matching
/// directory grant. Neither a document action nor a path creates this token.
/// Its Copy intent is deliberately separate from Computer's shared-file intent.
use super::portable_source::OriginalSource;

pub(crate) struct NativeOriginalSource {
    grant: Arc<SourceGrant>,
    choices: Choices,
    intent: Mutex<Option<MigrationIntent>>,
}
impl OriginalSource {
    #[cfg(test)]
    pub(crate) fn granted_folder_fixture(home: &Path) -> Arc<Self> {
        let grant = validate_grant(home, home)
            .unwrap_or_else(|_| panic!("fixture requires matching held home pins"));
        Arc::new(Self::from_native(NativeOriginalSource {
            grant,
            choices: Choices {
                home: true,
                ..Choices::default()
            },
            intent: std::sync::Mutex::new(None),
        }))
    }
    fn validate(&self) -> std::io::Result<()> {
        self.grant.validate()
    }
    fn native_applications_only(&self) -> bool {
        !self.choices.home && self.choices.applications && !self.choices.system_refs.is_empty()
    }
    pub(crate) fn capture_applications_intent(
        &self,
        cancel: &AtomicBool,
    ) -> std::io::Result<MigrationIntent> {
        if !self.applications_only() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.capture_original_intent(cancel, true)
    }
    /// Off-thread only. This establishes an actual durable Copy intent; it does
    /// not establish an immutable epoch, raw-medium authority, or completed media.
    pub(crate) fn capture_copy_intent(
        &self,
        cancel: &AtomicBool,
    ) -> std::io::Result<MigrationIntent> {
        self.capture_original_intent(cancel, false)
    }
    fn capture_original_intent(
        &self,
        cancel: &AtomicBool,
        applications_only: bool,
    ) -> std::io::Result<MigrationIntent> {
        if applications_only != self.applications_only() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        super::super::migration::check_cancel(cancel)?;
        self.validate()?;
        let mut held = self
            .intent
            .lock()
            .map_err(|_| std::io::Error::other("Original source custody unavailable"))?;
        if let Some(intent) = &*held {
            if applications_only {
                intent.for_pending_applications()?;
            } else {
                intent.for_pending_offline()?;
            }
            self.validate()?;
            return Ok(intent.clone());
        }
        let identity = super::personal::preparation_directory().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Private preparation namespace unavailable",
            )
        })?;
        let root = identity.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
        let directory = root.join("original-source");
        let pin = super::personal::ensure_private_tree(&directory)?;
        pin.validate_destination()?;
        let result = self.capture_in(&directory, cancel, super::super::migration_handoff::capture);
        pin.validate_destination()?;
        let intent = result?;
        *held = Some(intent.clone());
        Ok(intent)
    }
    fn capture_in(
        &self,
        directory: &Path,
        cancel: &AtomicBool,
        capture: super::super::migration_intent::Capture,
    ) -> std::io::Result<MigrationIntent> {
        use super::super::migration_handoff::HomePolicy;
        super::super::migration::check_cancel(cancel)?;
        self.validate()?;
        if !self.choices.home && !self.applications_only() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let categories = [
            self.choices.home,
            self.choices.applications,
            self.choices.accounts,
        ];
        #[cfg(test)]
        let result = MigrationIntent::capture_using(
            self.grant.selected(),
            directory,
            categories,
            self.choices.system_refs.clone(),
            HomePolicy::Copy,
            capture,
        );
        #[cfg(not(test))]
        let result = {
            let _ = capture;
            MigrationIntent::capture_with_policy(
                self.grant.selected(),
                directory,
                categories,
                self.choices.system_refs.clone(),
                HomePolicy::Copy,
            )
        };
        // The held grant is independently reattested even on capture failure.
        self.validate()?;
        let intent = result?;
        super::super::migration::check_cancel(cancel)?;
        Ok(intent)
    }
}

#[derive(Clone)]
enum ReviewFailure {
    Permission,
    SourceChanged,
    SourceUnavailable,
    UnsupportedMetadata,
    MountedSource {
        locations: Option<Arc<super::super::migration_review::MountedHomeLocations>>,
    },
    Applications,
    Other,
}
impl ReviewFailure {
    fn home(error: std::io::Error) -> Self {
        if super::super::migration_review::nested_home_mount(&error) {
            return Self::MountedSource {
                locations: super::super::migration_review::mounted_home_locations(&error),
            };
        }
        match super::super::portable_home_media::source_review_error(&error) {
            Some("installer-source-changed") => Self::SourceChanged,
            Some("installer-source-unavailable") => Self::SourceUnavailable,
            _ if error.kind() == std::io::ErrorKind::PermissionDenied => Self::Permission,
            _ if error.kind() == std::io::ErrorKind::Unsupported => Self::UnsupportedMetadata,
            _ => Self::Other,
        }
    }
    fn key(&self) -> &'static str {
        match self {
            Self::Permission => "installer-portable-permission-failed",
            Self::SourceChanged => "installer-source-changed",
            Self::SourceUnavailable => "installer-source-unavailable",
            Self::UnsupportedMetadata => "installer-portable-metadata-unsupported",
            Self::MountedSource { .. } => "installer-portable-mounted-source-unsupported",
            Self::Applications => "installer-portable-applications-review-failed",
            Self::Other => "installer-portable-source-failed",
        }
    }
}

#[derive(Default)]
pub(crate) struct Selection {
    picker: Option<ui::PortalChannel>,
    expected: Option<(PathBuf, Choices)>,
    worker: Option<Receiver<(Result<Reviewed, ReviewFailure>, Option<MountedContext>)>>,
    cancel: Option<Arc<AtomicBool>>,
    reviewed: Option<Reviewed>,
    state: &'static str,
    mounted: Option<Arc<super::super::migration_review::MountedHomeLocations>>,
    mounted_permission: Option<mounted_permission::Controller>,
    retire_mounted_after_exit: bool,
}
impl Selection {
    pub fn busy(&self) -> bool {
        self.picker.is_some() || self.worker.is_some() || self.mounted_busy()
    }
    pub fn choosing(&self) -> bool {
        self.picker.is_some() || self.mounted_choosing()
    }
    pub fn request_with_choices(
        &mut self,
        home: PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-portable-source-busy");
        }
        let picker = super::migration_selection::home_permission_request(&home);
        self.reviewed = None;
        self.expected = Some((home, choices));
        self.state = "permission";
        self.picker = Some(picker.send());
        Ok(())
    }
    /// The caller supplies an explicit already-granted directory selection.
    /// It must denote the same home folder the person reviewed on this device.
    #[cfg(test)]
    pub fn start(&mut self, selected: PathBuf, expected: PathBuf) -> Result<(), &'static str> {
        self.start_with_choices(
            selected,
            expected,
            Choices {
                home: true,
                ..Choices::default()
            },
        )
    }
    pub fn start_with_choices(
        &mut self,
        selected: PathBuf,
        expected: PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        self.start_bound(selected, expected, choices, None)
    }
    /// Renew content consent using the original held directory permission.
    /// The worker refuses replacement of the directory or any granted ancestor.
    pub fn refresh(&mut self) -> Result<(), &'static str> {
        let (grant, choices) = if let Some(reviewed) = &self.reviewed {
            (reviewed.grant.clone(), reviewed.choices.clone())
        } else {
            {
                let context = self
                    .mounted_permission
                    .as_ref()
                    .ok_or("installer-portable-source-failed")?
                    .retained_context();
                (context.grant, context.choices)
            }
        };
        self.start_bound(
            grant.selected().clone(),
            grant.expected().clone(),
            choices,
            Some(grant),
        )
    }
    fn start_bound(
        &mut self,
        selected: PathBuf,
        expected: PathBuf,
        choices: Choices,
        previous: Option<Arc<SourceGrant>>,
    ) -> Result<(), &'static str> {
        if self.worker.is_some() || (self.mounted_busy() && !self.mounted_choosing()) {
            return Err("installer-portable-source-busy");
        }
        // An explicit already-granted semantic selection completes the same
        // permission operation as its platform picker, without a second UI door.
        self.picker = None;
        self.expected = None;
        self.reviewed = None;
        {
            self.mounted = None;
            if let Some(controller) = &mut self.mounted_permission {
                controller.cancel();
            }
            self.mounted_permission = None;
            self.retire_mounted_after_exit = false;
        }
        self.state = "reviewing";
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        self.cancel = Some(cancel);
        self.worker = Some(receiver);
        std::thread::spawn(move || {
            let mut mounted_context = None;
            let result = (|| {
                if let Some(previous) = &previous {
                    previous
                        .validate()
                        .map_err(|_| ReviewFailure::SourceChanged)?;
                }
                let admitted = validate_grant(&selected, &expected)?;
                if let Some(previous) = &previous
                    && !previous
                        .matches(&admitted)
                        .map_err(|_| ReviewFailure::SourceChanged)?
                {
                    return Err(ReviewFailure::SourceChanged);
                }
                let grant = previous.clone().unwrap_or(admitted);
                mounted_context = Some(MountedContext {
                    grant: grant.clone(),
                    choices: choices.clone(),
                });
                let home = if choices.home {
                    let storage = || {
                        let prepared = choices.preparation.clone().ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "Home inventory needs acknowledged preparation storage",
                            )
                        })?;
                        super::live_inventory::review(prepared)
                    };
                    let source = if choices.preparation.is_some() {
                        HomeSource::review_selected_media_with_working_storage(
                            &selected,
                            &choices.exclusions,
                            &cancelled,
                            &storage,
                        )
                    } else {
                        HomeSource::review_selected_for_media_preparation(
                            &selected,
                            &choices.exclusions,
                            &cancelled,
                        )
                    }
                    .map_err(ReviewFailure::home)?;
                    source
                        .validate_with_cancel(&cancelled)
                        .map_err(ReviewFailure::home)?;
                    Some(source)
                } else {
                    None
                };
                let applications =
                    if choices.applications && (!choices.system_refs.is_empty() || !choices.home) {
                        let source = if choices.home {
                            PortableFlatpakSource::review(
                                &NativeRunner,
                                &selected,
                                choices.system_refs.clone(),
                            )
                        } else {
                            PortableFlatpakSource::review_selected_state(
                                &NativeRunner,
                                &selected,
                                choices.system_refs.clone(),
                            )
                        }
                        .map_err(|_| ReviewFailure::Applications)?;
                        source.validate().map_err(|_| ReviewFailure::Applications)?;
                        Some(source)
                    } else {
                        None
                    };
                grant.validate().map_err(|_| ReviewFailure::SourceChanged)?;
                if cancelled.load(Ordering::SeqCst) {
                    return Err(ReviewFailure::SourceChanged);
                }
                if let Some(previous) = &previous {
                    previous
                        .validate()
                        .map_err(|_| ReviewFailure::SourceChanged)?;
                }
                let original = (if home.as_ref().is_some_and(|source| source.original_only()) {
                    true
                } else {
                    !choices.home
                        && choices.applications
                        && choices.system_refs.iter().any(|item| {
                            item.installation == super::super::flatpak_transfer::Installation::User
                        })
                })
                .then(|| {
                    Arc::new(OriginalSource::from_native(NativeOriginalSource {
                        grant: grant.clone(),
                        choices: choices.clone(),
                        intent: Mutex::new(None),
                    }))
                });
                Ok(Reviewed {
                    original,
                    grant,
                    choices,
                    home,
                    applications,
                })
            })();
            let _ = sender.send((result, mounted_context));
        });
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<bool, &'static str>> {
        if let Some(controller) = &mut self.mounted_permission
            && let Some(result) = controller.poll()
        {
            if self.retire_mounted_after_exit && !controller.busy() {
                self.mounted_permission = None;
                self.retire_mounted_after_exit = false;
                return Some(Ok(false));
            }
            return Some(match result {
                Ok(Some(reviewed)) => {
                    self.reviewed = Some(reviewed);
                    self.mounted_permission = None;
                    self.mounted = None;
                    self.state = "reviewed";
                    Ok(true)
                }
                Ok(None) => Ok(false),
                Err(error) => Err(error.key()),
            });
        }
        if let Some(picker) = &self.picker {
            let outcome = match picker.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => ui::PortalOutcome::Failed(String::new()),
            };
            self.picker = None;
            let expected = self.expected.take();
            match (outcome, expected) {
                (ui::PortalOutcome::Opened(selected), Some((expected, choices))) => {
                    if let Err(error) = self.start_with_choices(selected, expected, choices) {
                        return Some(Err(error));
                    }
                    return Some(Ok(false));
                }
                (ui::PortalOutcome::Cancelled, _) => {
                    self.state = "cancelled";
                    return Some(Ok(false));
                }
                _ => {
                    self.state = "failed";
                    return Some(Err("installer-portable-source-failed"));
                }
            }
        }
        let (result, mounted_context) = match self.worker.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => (Err(ReviewFailure::Other), None),
        };
        self.worker = None;
        let cancelled = self
            .cancel
            .take()
            .is_some_and(|cancel| cancel.load(Ordering::SeqCst));
        if cancelled {
            self.state = "cancelled";
            return Some(Ok(false));
        }
        match result {
            Ok(reviewed) => {
                self.reviewed = Some(reviewed);
                self.state = "reviewed";
                Some(Ok(true))
            }
            Err(error) => {
                if let ReviewFailure::MountedSource { locations } = &error {
                    self.mounted.clone_from(locations);
                    if let (Some(locations), Some(context)) = (locations, mounted_context) {
                        self.mounted_permission = Some(mounted_permission::Controller::new(
                            locations.clone(),
                            context,
                        ));
                    }
                }
                self.state = "failed";
                Some(Err(error.key()))
            }
        }
    }
    /// Retain the exact original grant across media preparation. Callers must
    /// choose offline continuation explicitly; SourceChanged never selects it.
    pub fn reviewed_original(&self) -> Option<Arc<OriginalSource>> {
        self.reviewed
            .as_ref()
            .and_then(|reviewed| reviewed.original.clone())
    }
    pub fn reviewed_handle(&self) -> Option<Arc<HomeSource>> {
        self.reviewed
            .as_ref()
            .and_then(|reviewed| reviewed.home.clone())
    }
    pub fn reviewed_applications(&self) -> Option<Arc<PortableFlatpakSource>> {
        self.reviewed
            .as_ref()
            .and_then(|reviewed| reviewed.applications.clone())
    }
    pub fn invalidate(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        self.picker = None;
        self.expected = None;
        self.reviewed = None;
        {
            self.mounted = None;
            if let Some(controller) = &mut self.mounted_permission {
                controller.cancel();
            }
            self.retire_mounted_after_exit = self.mounted_busy();
            if !self.retire_mounted_after_exit {
                self.mounted_permission = None;
            }
        }
        self.state = if self.busy() { "cancelling" } else { "idle" };
    }
    fn mounted_busy(&self) -> bool {
        {
            self.mounted_permission
                .as_ref()
                .is_some_and(mounted_permission::Controller::busy)
        }
    }
    fn mounted_choosing(&self) -> bool {
        {
            self.mounted_permission
                .as_ref()
                .is_some_and(mounted_permission::Controller::choosing)
        }
    }
    pub fn refresh_mounted(
        &mut self,
        expected: PathBuf,
        choices: Choices,
    ) -> Result<(), &'static str> {
        if !choices.home || !self.mounted_needs_restart() {
            return Err("installer-portable-source-failed");
        }
        let grant = self
            .mounted_permission
            .as_ref()
            .ok_or("installer-portable-source-failed")?
            .retained_context()
            .grant;
        if &expected != grant.expected() {
            return Err("installer-source-changed");
        }
        self.start_bound(grant.selected().clone(), expected, choices, Some(grant))
    }
    pub fn mounted_permission_active(&self) -> bool {
        self.mounted_permission.is_some()
    }
    pub fn mounted_needs_restart(&self) -> bool {
        self.mounted_permission
            .as_ref()
            .is_some_and(mounted_permission::Controller::needs_restart)
    }
    pub fn mounted_choices(&self) -> Vec<(String, String, bool)> {
        self.mounted_permission
            .as_ref()
            .map_or_else(Vec::new, mounted_permission::Controller::locations)
    }
    pub fn request_mounted(&mut self, location: String) -> Result<(), &'static str> {
        if self.picker.is_some() || self.worker.is_some() {
            return Err("installer-portable-source-busy");
        }
        self.mounted_permission
            .as_mut()
            .ok_or("installer-portable-source-failed")?
            .request(
                location,
                super::t("installer-mounted-home-picker-title"),
                super::t("installer-mounted-home-grant"),
            )
    }
    pub fn finish_mounted(&mut self) -> Result<(), &'static str> {
        self.mounted_permission
            .as_mut()
            .ok_or("installer-portable-source-failed")?
            .finish()
    }
    pub fn cancel_mounted(&mut self) {
        if let Some(controller) = &mut self.mounted_permission {
            controller.cancel_request();
        }
    }
    /// Reviewed relative display names only; these do not authorize mounted reads.
    pub fn mounted_locations(&self) -> Vec<String> {
        {
            self.mounted
                .as_ref()
                .map_or_else(Vec::new, |locations| locations.display_names())
        }
    }
    fn mounted_projection(&self) -> serde_json::Value {
        {
            self.mounted_permission.as_ref().map_or(
                serde_json::Value::Null,
                mounted_permission::Controller::projection,
            )
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        let summary = self
            .reviewed
            .as_ref()
            .and_then(|reviewed| reviewed.home.as_ref())
            .and_then(|home| home.summary().ok());
        serde_json::json!({"state":if self.state.is_empty(){"idle"}else{self.state},
            "capture":self.reviewed.as_ref().and_then(|reviewed| reviewed.home.as_ref()).map(|home| home.capture_status()),
            "mounted_locations": self.mounted_locations(),
            "mounted_permissions": self.mounted_projection(),
            "prepared":self.reviewed.is_some(), "applications":self.reviewed_applications().is_some(), "files":summary.as_ref().map(|s|s.files),
            "bytes":summary.as_ref().map(|s|s.bytes)})
    }
}
impl Drop for Selection {
    fn drop(&mut self) {
        self.invalidate();
    }
}
fn validate_grant(selected: &Path, expected: &Path) -> Result<Arc<SourceGrant>, ReviewFailure> {
    SourceGrant::admit(selected, expected)
        .map(Arc::new)
        .map_err(|_| ReviewFailure::Permission)
}

impl OriginalSource {
    fn from_native(native: NativeOriginalSource) -> Self {
        Self {
            native_payload: super::native_ownership::FrontendNativeCustody::new(native),
            applications_only: Self::native_applications_only,
        }
    }
}
impl std::ops::Deref for OriginalSource {
    type Target = NativeOriginalSource;
    fn deref(&self) -> &Self::Target {
        self.native_payload
            .get()
            .expect("original source retains its native owner")
    }
}
impl super::portable_source::SelectionState for Selection {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn busy(&self) -> bool {
        Selection::busy(self)
    }
    fn choosing(&self) -> bool {
        Selection::choosing(self)
    }
    fn refresh(&mut self) -> Result<(), &'static str> {
        Selection::refresh(self)
    }
    fn poll(&mut self) -> Option<Result<bool, &'static str>> {
        Selection::poll(self)
    }
    fn reviewed_original(&self) -> Option<Arc<OriginalSource>> {
        Selection::reviewed_original(self)
    }
    fn invalidate(&mut self) {
        Selection::invalidate(self)
    }
    fn mounted_permission_active(&self) -> bool {
        Selection::mounted_permission_active(self)
    }
    fn mounted_needs_restart(&self) -> bool {
        Selection::mounted_needs_restart(self)
    }
    fn mounted_choices(&self) -> Vec<(String, String, bool)> {
        Selection::mounted_choices(self)
    }
    fn request_mounted(&mut self, location: String) -> Result<(), &'static str> {
        Selection::request_mounted(self, location)
    }
    fn finish_mounted(&mut self) -> Result<(), &'static str> {
        Selection::finish_mounted(self)
    }
    fn cancel_mounted(&mut self) {
        Selection::cancel_mounted(self)
    }
    fn mounted_locations(&self) -> Vec<String> {
        Selection::mounted_locations(self)
    }
    fn projection(&self) -> serde_json::Value {
        Selection::projection(self)
    }
}
impl std::ops::Deref for super::portable_source::Selection {
    type Target = Selection;
    fn deref(&self) -> &Self::Target {
        self.native
            .get()
            .expect("native adapter installed")
            .as_any()
            .downcast_ref()
            .expect("matching native adapter")
    }
}
impl std::ops::DerefMut for super::portable_source::Selection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.native
            .get_mut()
            .expect("native adapter installed")
            .as_any_mut()
            .downcast_mut()
            .expect("matching native adapter")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn reviewed_original_copy_intent_is_durable_and_rejects_replaced_or_cancelled_grant() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("document"), b"original document").unwrap();
        let grant = validate_grant(&home, &home).unwrap_or_else(|_| panic!("matching held grant"));
        let original = OriginalSource::from_native(NativeOriginalSource {
            grant,
            choices: Choices {
                home: true,
                accounts: true,
                ..Choices::default()
            },
            intent: Mutex::new(None),
        });
        let journal = temp.path().join("journal");
        std::fs::create_dir(&journal).unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = AtomicBool::new(false);
        original
            .capture_in(
                &journal,
                &cancel,
                super::super::super::migration_handoff::capture_fixture,
            )
            .unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal.join("migration-intent.json")).unwrap())
                .unwrap();
        // Copy is the canonical omitted/default policy; Computer explicitly
        // persists access-read-write. Original categories survive unchanged.
        assert!(persisted["plan"].get("home_policy").is_none());
        assert_eq!(persisted["plan"]["categories"]["home"], true);
        assert_eq!(persisted["plan"]["categories"]["accounts"], true);
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"original document"
        );
        let cancelled = temp.path().join("cancelled");
        cancel.store(true, Ordering::SeqCst);
        assert!(
            original
                .capture_in(
                    &cancelled,
                    &cancel,
                    super::super::super::migration_handoff::capture_fixture
                )
                .is_err()
        );
        assert!(!cancelled.exists());
        cancel.store(false, Ordering::SeqCst);
        std::fs::rename(&home, temp.path().join("old-home")).unwrap();
        std::fs::create_dir(&home).unwrap();
        assert!(
            original
                .capture_in(
                    &cancelled,
                    &cancel,
                    super::super::super::migration_handoff::capture_fixture
                )
                .is_err()
        );
        assert!(!cancelled.exists());
        assert_eq!(
            std::fs::read(temp.path().join("old-home/document")).unwrap(),
            b"original document"
        );
    }

    /// A temporary directory on tmpfs, where Home review takes the live
    /// inventory route. Ext4 and XFS sources are deliberately not inventoried
    /// at review time (the original system's offline boot reads them), so a
    /// fixture there could not observe ACL policy refusals, byte summaries or
    /// changed contents. `/tmp` is not tmpfs everywhere, so `/dev/shm` is
    /// tried next.
    #[cfg(target_os = "linux")]
    fn live_review_tempdir() -> tempfile::TempDir {
        use std::os::fd::AsRawFd;
        let tmpfs = |root: &Path| {
            let Ok(directory) = std::fs::File::open(root) else {
                return false;
            };
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: a held directory fd and complete writable statfs storage.
            if unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: successful fstatfs initialized the output.
            unsafe { filesystem.assume_init() }.f_type == libc::TMPFS_MAGIC
        };
        let candidates = [std::env::temp_dir(), PathBuf::from("/dev/shm")];
        candidates
            .iter()
            .filter(|root| tmpfs(root))
            .find_map(|root| tempfile::tempdir_in(root).ok())
            .unwrap_or_else(|| {
                panic!("live Home review fixtures need a writable tmpfs at {candidates:?}")
            })
    }

    #[cfg(target_os = "linux")]
    fn install_acl(path: &Path, default: bool, named: bool) -> Vec<u8> {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(path).unwrap();
        let mut bytes = 2u32.to_le_bytes().to_vec();
        let mut entries = vec![(1u16, 7u16, u32::MAX)];
        if named {
            entries.push((2, 4, 60_000));
        }
        entries.push((4, 5, u32::MAX));
        entries.push((16, 5, u32::MAX));
        entries.push((32, 0, u32::MAX));
        for (tag, permissions, id) in entries {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&permissions.to_le_bytes());
            bytes.extend_from_slice(&id.to_le_bytes());
        }
        let name = if default {
            c"system.posix_acl_default"
        } else {
            c"system.posix_acl_access"
        };
        // SAFETY: held descriptor, fixed terminated name, and bounded live bytes.
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        assert_eq!(
            result,
            0,
            "actual Linux ACL fixture unavailable: {}",
            std::io::Error::last_os_error()
        );
        bytes
    }

    #[cfg(target_os = "linux")]
    fn read_acl(path: &Path, default: bool) -> Vec<u8> {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(path).unwrap();
        let name = if default {
            c"system.posix_acl_default"
        } else {
            c"system.posix_acl_access"
        };
        let mut bytes = vec![0u8; 65536];
        // SAFETY: held descriptor, fixed name and live bounded output buffer.
        let length = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        assert!(
            length >= 0,
            "actual ACL read failed: {}",
            std::io::Error::last_os_error()
        );
        bytes.truncate(usize::try_from(length).unwrap());
        bytes
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn named_file_acl_review_refuses_before_ready_without_permission_retry() {
        let temp = live_review_tempdir();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let document = home.join("document");
        std::fs::write(&document, b"original document").unwrap();
        let original = install_acl(&document, false, true);
        let mut selection = Selection::default();
        selection.start(home.clone(), home).unwrap();
        assert_eq!(
            settle(&mut selection),
            Err("installer-portable-metadata-unsupported")
        );
        assert_eq!(selection.projection()["prepared"], false);
        assert!(selection.reviewed_handle().is_none());
        assert_eq!(std::fs::read(&document).unwrap(), b"original document");
        // Re-read through the existing parser: unsupported policy remains intact.
        let error = super::super::super::portable_transfer::acl::policy(
            &std::fs::File::open(&document).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(read_acl(&document, false), original);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn named_default_directory_acl_review_refuses_before_ready() {
        let temp = live_review_tempdir();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let original = install_acl(&home, true, true);
        let mut selection = Selection::default();
        selection.start(home.clone(), home.clone()).unwrap();
        assert_eq!(
            settle(&mut selection),
            Err("installer-portable-metadata-unsupported")
        );
        assert!(selection.reviewed_handle().is_none());
        let error = super::super::super::portable_transfer::acl::policy(
            &std::fs::File::open(&home).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(read_acl(&home, true), original);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_file_and_default_acls_review_successfully() {
        let temp = live_review_tempdir();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let document = home.join("document");
        std::fs::write(&document, b"original document").unwrap();
        let file_acl = install_acl(&document, false, false);
        let default_acl = install_acl(&home, true, false);
        let mut selection = Selection::default();
        selection.start(home.clone(), home.clone()).unwrap();
        assert!(settle(&mut selection).unwrap());
        assert_eq!(selection.projection()["prepared"], true);
        selection.reviewed_handle().unwrap().validate().unwrap();
        assert_eq!(read_acl(&document, false), file_acl);
        assert_eq!(read_acl(&home, true), default_acl);
        assert_eq!(std::fs::read(&document).unwrap(), b"original document");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn wrong_selection_is_permission_but_replaced_approved_home_is_source_changed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let other = temp.path().join("other");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(home.join("document"), b"original").unwrap();
        let mut selection = Selection::default();
        selection.start(other, home.clone()).unwrap();
        assert_eq!(
            settle(&mut selection),
            Err("installer-portable-permission-failed")
        );
        assert!(selection.reviewed_handle().is_none());
        selection.start(home.clone(), home.clone()).unwrap();
        assert!(settle(&mut selection).unwrap());
        let retained = temp.path().join("retained-original");
        std::fs::rename(&home, &retained).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("document"), b"replacement").unwrap();
        selection.refresh().unwrap();
        assert_eq!(settle(&mut selection), Err("installer-source-changed"));
        assert!(selection.reviewed_handle().is_none());
        assert_eq!(
            std::fs::read(retained.join("document")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"replacement"
        );
    }

    fn settle(selection: &mut Selection) -> Result<bool, &'static str> {
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            if let Some(result) = selection.poll() {
                return result;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn consented_other_device_source_holds_review_without_same_disk_intent() {
        let temp = live_review_tempdir();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let file = home.join("document");
        std::fs::write(&file, b"original belongings").unwrap();
        let mut selection = Selection::default();
        assert!(selection.reviewed_handle().is_none());
        selection.start(home.clone(), home.clone()).unwrap();
        assert!(settle(&mut selection).unwrap());
        let review = selection.reviewed_handle().unwrap();
        assert_eq!(review.summary().unwrap().bytes, 19);
        assert_eq!(std::fs::read(&file).unwrap(), b"original belongings");
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            1,
            "review creates no plan or ciphertext files"
        );
        assert!(
            !selection
                .projection()
                .to_string()
                .contains(home.to_str().unwrap())
        );
        std::fs::write(&file, b"changed after source review").unwrap();
        assert!(
            review.validate().is_err(),
            "writer cannot use changed reviewed belongings"
        );
        selection.invalidate();
        assert!(selection.reviewed_handle().is_none());
        assert_eq!(selection.projection()["state"], "idle");
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn changed_contents_refresh_with_held_permission_but_directory_replacement_refuses() {
        let temp = live_review_tempdir();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("document"), b"before").unwrap();
        let mut selection = Selection::default();
        selection.start(home.clone(), home.clone()).unwrap();
        assert!(settle(&mut selection).unwrap());
        let previous = selection.reviewed_handle().unwrap();
        std::fs::write(home.join("document"), b"changed original document").unwrap();
        assert!(previous.validate().is_err());
        selection.refresh().unwrap();
        assert!(settle(&mut selection).unwrap());
        let renewed = selection.reviewed_handle().unwrap();
        renewed.validate().unwrap();
        assert!(!Arc::ptr_eq(&previous, &renewed));
        assert_eq!(renewed.summary().unwrap().bytes, 25);
        std::fs::rename(&home, temp.path().join("original-home")).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("document"), b"substituted document").unwrap();
        selection.refresh().unwrap();
        assert!(settle(&mut selection).is_err());
        assert!(selection.reviewed_handle().is_none());
        assert_eq!(
            std::fs::read(temp.path().join("original-home/document")).unwrap(),
            b"changed original document"
        );
    }
    #[test]
    fn wrong_home_and_symlink_grants_fail_and_explicit_retry_can_succeed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let other = temp.path().join("other");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&other).unwrap();
        let mut selection = Selection::default();
        selection.start(other, home.clone()).unwrap();
        assert!(settle(&mut selection).is_err());
        assert!(selection.reviewed_handle().is_none());
        let symlink = temp.path().join("alias");
        std::os::unix::fs::symlink(&home, &symlink).unwrap();
        selection.start(symlink, home.clone()).unwrap();
        assert!(settle(&mut selection).is_err());
        selection.start(home.clone(), home).unwrap();
        assert!(settle(&mut selection).unwrap());
        assert!(selection.reviewed_handle().is_some());
    }
    #[test]
    fn invalidating_inflight_review_cannot_publish_stale_consent() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("original"), b"unchanged").unwrap();
        let mut selection = Selection::default();
        selection.start(home.clone(), home.clone()).unwrap();
        selection.invalidate();
        assert!(!settle(&mut selection).unwrap());
        assert!(selection.reviewed_handle().is_none());
        assert_eq!(selection.projection()["state"], "cancelled");
        assert_eq!(std::fs::read(home.join("original")).unwrap(), b"unchanged");
    }
}
