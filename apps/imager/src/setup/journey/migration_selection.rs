//! Explicit portal/semantic grant, followed by private source-bound preparation.
use super::super::{
    flatpak_transfer::InstalledRef, migration::PinnedDirectory, migration_intent::MigrationIntent,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
};

#[derive(Debug)]
enum ReviewFailure {
    Grant,
    ChangedSource,
    UnsupportedSource,
    PrivateState,
    Capacity,
    Unavailable,
}
impl ReviewFailure {
    fn private_state(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::StorageFull {
            Self::Capacity
        } else {
            Self::PrivateState
        }
    }
    fn capture(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::Unsupported => Self::UnsupportedSource,
            std::io::ErrorKind::StorageFull => Self::Capacity,
            _ => Self::Unavailable,
        }
    }
    fn key(&self) -> &'static str {
        match self {
            Self::Grant => "installer-migration-grant-failed",
            Self::ChangedSource => "installer-migration-source-replaced",
            Self::UnsupportedSource => "installer-migration-source-unsupported",
            Self::PrivateState => "installer-migration-state-unavailable",
            Self::Capacity => "installer-migration-state-full",
            Self::Unavailable => "installer-migration-prepare-failed",
        }
    }
}

#[derive(Default)]
pub(super) struct Selection {
    picker: Option<ui::PortalChannel>,
    request: Option<(PathBuf, [bool; 3], Vec<InstalledRef>)>,
    worker: Option<Receiver<Result<MigrationIntent, ReviewFailure>>>,
    intent: Option<MigrationIntent>,
    state: &'static str,
    #[cfg(test)]
    pub(super) capture: Option<super::super::migration_intent::Capture>,
    #[cfg(test)]
    pub(super) permission_reply: Option<ui::PortalChannel>,
}
impl Selection {
    pub fn reviewed_intent(&self) -> Option<MigrationIntent> {
        self.intent.clone()
    }
    pub fn busy(&self) -> bool {
        self.picker.is_some() || self.worker.is_some()
    }
    pub fn choosing(&self) -> bool {
        self.picker.is_some()
    }
    pub fn request(&mut self, home: PathBuf, categories: [bool; 3], refs: Vec<InstalledRef>) {
        let picker = home_permission_request(&home);
        self.intent = None;
        self.request = Some((home, categories, refs));
        self.state = "permission";
        #[cfg(test)]
        let reply = self
            .permission_reply
            .take()
            .unwrap_or_else(|| picker.send());
        #[cfg(not(test))]
        let reply = picker.send();
        self.picker = Some(reply);
    }
    pub fn start(
        &mut self,
        selected: PathBuf,
        home: PathBuf,
        categories: [bool; 3],
        refs: Vec<InstalledRef>,
    ) {
        // The semantic grant completes the same request as the platform picker.
        // Dropping its receiver retires queued/late portal replies before work.
        self.picker = None;
        self.request = None;
        self.intent = None;
        self.state = "preparing";
        #[cfg(test)]
        let capture = self.capture;

        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| {
                let (selected_pin, expected_pin) = validate_home_grant(&selected, &home)?;
                let root = state_root().ok_or(ReviewFailure::PrivateState)?;
                let parent = ensure_private_tree(&root).map_err(ReviewFailure::private_state)?;
                let name = uuid::Uuid::new_v4().to_string();
                let child = parent
                    .create_child(std::ffi::OsStr::new(&name))
                    .map_err(ReviewFailure::private_state)?;
                child
                    .validate_destination()
                    .map_err(ReviewFailure::private_state)?;
                let directory = root.join(name);
                super::super::migration_durability::sync_directory(&parent.file)
                    .map_err(ReviewFailure::private_state)?;
                let policy = if categories[0] {
                    super::super::migration_handoff::HomePolicy::AccessReadWrite
                } else {
                    super::super::migration_handoff::HomePolicy::Copy
                };
                #[cfg(all(not(test), target_os = "linux"))]
                let intent = if categories[0] && categories[1] {
                    MigrationIntent::capture_computer_applications(
                        &selected, &directory, categories, refs,
                    )
                } else {
                    MigrationIntent::capture_with_policy(
                        &selected, &directory, categories, refs, policy,
                    )
                };
                #[cfg(all(not(test), not(target_os = "linux")))]
                let intent = MigrationIntent::capture_with_policy(
                    &selected, &directory, categories, refs, policy,
                );
                #[cfg(all(test, target_os = "linux"))]
                let intent = if let Some(capture) = capture {
                    MigrationIntent::capture_using(
                        &selected, &directory, categories, refs, policy, capture,
                    )
                } else if categories[0] && categories[1] {
                    MigrationIntent::capture_computer_applications(
                        &selected, &directory, categories, refs,
                    )
                } else {
                    MigrationIntent::capture_with_policy(
                        &selected, &directory, categories, refs, policy,
                    )
                };
                #[cfg(all(test, not(target_os = "linux")))]
                let intent = if let Some(capture) = capture {
                    MigrationIntent::capture_using(
                        &selected, &directory, categories, refs, policy, capture,
                    )
                } else {
                    MigrationIntent::capture_with_policy(
                        &selected, &directory, categories, refs, policy,
                    )
                };
                // Independently observed grant substitution takes precedence even
                // when capture failed; arbitrary InvalidData never implies it.
                reattest_grant(&selected_pin, &expected_pin)?;
                let intent = intent.map_err(ReviewFailure::capture)?;
                Ok(intent)
            })();
            let _ = sender.send(result);
        });
    }
    /// Terminal result advances the journey only after original source attestation.
    pub fn poll(&mut self) -> Option<Result<bool, &'static str>> {
        if let Some(picker) = &self.picker {
            let result = match picker.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => ui::PortalOutcome::Failed(String::new()),
            };
            self.picker = None;
            let request = self.request.take();
            match (result, request) {
                (ui::PortalOutcome::Opened(selected), Some((home, categories, refs))) => {
                    self.start(selected, home, categories, refs);
                    return Some(Ok(false));
                }
                (ui::PortalOutcome::Cancelled, _) => {
                    self.state = "cancelled";
                    return Some(Ok(false));
                }
                _ => {
                    self.state = "failed";
                    return Some(Err("installer-migration-grant-failed"));
                }
            }
        }
        let result = match self.worker.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(ReviewFailure::Unavailable),
        };
        self.worker = None;
        match result {
            Ok(intent) => {
                self.intent = Some(intent);
                self.state = "prepared";
                Some(Ok(true))
            }
            Err(error) => {
                self.state = "failed";
                Some(Err(error.key()))
            }
        }
    }
    #[cfg(test)]
    pub fn prepare_routing_fixture(&mut self, intent: MigrationIntent) {
        self.intent = Some(intent);
        self.state = "prepared";
    }
    pub fn application_minimum(&self) -> u64 {
        self.intent.as_ref().map_or(
            super::super::staging::MIN_TARGET_BYTES,
            MigrationIntent::application_minimum,
        )
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"state": if self.state.is_empty() {"idle"} else {self.state},
            "prepared": self.intent.is_some(),
            "private_plans": self.intent.as_ref().map(|intent| super::super::migration_store::NativePath::encode(intent.directory())),
            "operation": self.intent.as_ref().map(|intent| intent.source_plan().operation_id)})
    }
    /// Retire permission only; an active review worker must finish before navigation.
    pub fn cancel_permission(&mut self) {
        if self.worker.is_none() && self.picker.take().is_some() {
            self.request = None;
            self.state = "cancelled";
        }
    }
    pub fn invalidate(&mut self) {
        self.cancel_permission();
        self.intent = None;
        self.state = "idle";
    }
}
pub(super) fn home_permission_request(home: &Path) -> ui::PickRequest {
    ui::PickRequest::directory(
        &super::t_args(
            "installer-migration-home-picker",
            &[("home", home.display().to_string().into())],
        ),
        &super::t("installer-migration-home-allow"),
    )
    .initial_directory(home)
}
fn validate_home_grant(
    selected: &Path,
    expected: &Path,
) -> Result<(Arc<PinnedDirectory>, Arc<PinnedDirectory>), ReviewFailure> {
    let selected = PinnedDirectory::open_absolute(selected).map_err(|_| ReviewFailure::Grant)?;
    let expected = PinnedDirectory::open_absolute(expected).map_err(|_| ReviewFailure::Grant)?;
    selected.validate().map_err(|_| ReviewFailure::Grant)?;
    expected.validate().map_err(|_| ReviewFailure::Grant)?;
    if !super::super::nofollow::same_directory_identity(&selected.identity, &expected.identity) {
        return Err(ReviewFailure::Grant);
    }
    Ok((selected, expected))
}
fn reattest_grant(
    selected: &Arc<PinnedDirectory>,
    expected: &Arc<PinnedDirectory>,
) -> Result<(), ReviewFailure> {
    selected
        .validate()
        .map_err(|_| ReviewFailure::ChangedSource)?;
    expected
        .validate()
        .map_err(|_| ReviewFailure::ChangedSource)?;
    Ok(())
}
fn state_root() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    state
        .is_absolute()
        .then(|| state.join("tilo/installer/migrations"))
}
fn ensure_private_tree(path: &Path) -> std::io::Result<Arc<PinnedDirectory>> {
    match PinnedDirectory::open_absolute(path) {
        Ok(pin) => {
            pin.validate_destination()?;
            super::super::destination_security::check_private_file(&pin.file)?;
            Ok(pin)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = ensure_private_tree(path.parent().ok_or(error)?)?;
            let child = parent.create_child(
                path.file_name()
                    .ok_or_else(|| std::io::Error::other("Missing state name"))?,
            )?;
            super::super::migration_durability::sync_directory(&parent.file)?;
            Ok(child)
        }
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn wrong_selected_home_worker_reports_grant_failure_without_private_state_writes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(home.join("document"), b"original").unwrap();
        let state = root.path().join("state");
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_STATE_HOME", state.clone());
        let mut selection = super::Selection::default();
        selection.start(other, home.clone(), [true, false, false], Vec::new());
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        let result = loop {
            if let Some(result) = selection.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(result, Err("installer-migration-grant-failed"));
        assert!(selection.reviewed_intent().is_none());
        assert!(!state.exists());
        assert_eq!(std::fs::read(home.join("document")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn private_state_symlink_failure_does_not_request_home_permission_again() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let linked = root.path().join("linked-state");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::os::unix::fs::symlink(&state, &linked).unwrap();
        std::fs::write(home.join("document"), b"original").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_STATE_HOME", linked.clone());
        let mut selection = super::Selection::default();
        selection.start(home.clone(), home.clone(), [true, false, false], Vec::new());
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        let result = loop {
            if let Some(result) = selection.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(result, Err("installer-migration-state-unavailable"));
        assert!(selection.reviewed_intent().is_none());
        assert_eq!(std::fs::read(home.join("document")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(state).unwrap().count(), 0);
    }

    #[test]
    fn home_permission_request_names_and_opens_the_recognized_folder() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("Recognized home");
        std::fs::create_dir(&home).unwrap();
        let request = super::home_permission_request(&home);
        assert!(request.is_directory());
        assert_eq!(request.directory_hint(), Some(home.as_path()));
        assert!(request.title().contains(home.to_str().unwrap()));
        assert!(!request.title().contains("reviewed"));
        assert_eq!(
            request.accept_label(),
            super::super::t("installer-migration-home-allow")
        );
    }

    #[test]
    fn actual_grant_must_match_reviewed_home_and_never_follows_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&other).unwrap();
        assert!(super::validate_home_grant(&home, &home).is_ok());
        assert!(super::validate_home_grant(&other, &home).is_err());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        assert!(super::validate_home_grant(&link, &home).is_err());
    }

    #[test]
    fn granted_home_replacement_invalidates_both_source_pins() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("document"), b"original").unwrap();
        let (selected, expected) = super::validate_home_grant(&home, &home).unwrap();
        let retained = root.path().join("retained");
        std::fs::rename(&home, &retained).unwrap();
        std::fs::create_dir(&home).unwrap();
        assert!(selected.validate().is_err());
        assert!(expected.validate().is_err());
        assert_eq!(
            super::reattest_grant(&selected, &expected)
                .unwrap_err()
                .key(),
            "installer-migration-source-replaced"
        );
        assert_eq!(
            std::fs::read(retained.join("document")).unwrap(),
            b"original"
        );
    }
}
