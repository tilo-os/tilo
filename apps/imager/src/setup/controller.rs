//! Process-local navigation and choices for the guided setup flow.
//!
//! The controller prepares a reviewable estimate. It does not grant file or
//! installation authority; platform workers must revalidate every path and
//! capability before doing work.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::folders::FolderScan;
use super::model::{Capabilities, Experience, FolderMode, FolderSelection, SetupPlan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupPage {
    Experience,
    Folders,
    Review,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderDestination {
    /// Task 5 resolves and revalidates the actual Tilo-managed local target.
    Unresolved,
    Resolved(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderChoice {
    pub scan: FolderScan,
    pub destination: FolderDestination,
    pub mode: FolderMode,
    pub mode_overridden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FolderIdentity(u64);

impl FolderIdentity {
    fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        loop {
            let value = NEXT.fetch_add(1, Ordering::Relaxed);
            if value != 0 {
                return Self(value);
            }
        }
    }

    fn token(self) -> String {
        format!("{value:016x}", value = self.0)
    }
}

/// Owns the in-process choices shown by the setup screens.
///
/// Navigation and folder choices are changed through methods so returning to
/// Review always requires an explicit navigation step after an edit.
#[derive(Clone, Debug)]
pub struct SetupController {
    page: SetupPage,
    default_mode: FolderMode,
    capabilities: Capabilities,
    chosen_experience: Option<Experience>,
    remembered_folders: Vec<FolderChoice>,
    remembered_folder_identities: Vec<FolderIdentity>,
    plan: SetupPlan,
}

impl SetupController {
    #[must_use]
    pub fn new(capabilities: Capabilities) -> Self {
        Self {
            page: SetupPage::Experience,
            default_mode: FolderMode::Copy,
            capabilities,
            chosen_experience: None,
            remembered_folders: Vec::new(),
            remembered_folder_identities: Vec::new(),
            plan: SetupPlan {
                // `chosen_experience` gates navigation, so this placeholder is
                // never treated as a reviewed choice.
                experience: Experience::Desktop,
                folders: Vec::new(),
                system_bytes: 0,
                staging_bytes: 0,
                headroom_bytes: 0,
            },
        }
    }

    /// Record explicit Desktop intent without asserting host support. The live
    /// Desktop flow remains the authority for release and install availability.
    pub(crate) fn review_desktop(&mut self) {
        self.chosen_experience = Some(Experience::Desktop);
        self.plan.experience = Experience::Desktop;
        self.page = SetupPage::Review;
    }

    /// Select an experience only when the probed capabilities support it.
    /// Returns whether the choice was accepted.
    pub fn choose_experience(&mut self, experience: Experience) -> bool {
        if !self.capabilities.supports(experience) {
            return false;
        }
        self.chosen_experience = Some(experience);
        self.plan.experience = experience;
        if self.page == SetupPage::Review {
            self.page = SetupPage::Experience;
        }
        true
    }

    /// Replace live capability facts without discarding the reviewed choices.
    pub fn set_capabilities(&mut self, capabilities: Capabilities) {
        self.capabilities = capabilities;
    }

    #[must_use]
    pub fn page(&self) -> SetupPage {
        self.page
    }

    #[must_use]
    pub fn default_mode(&self) -> FolderMode {
        self.default_mode
    }

    #[must_use]
    pub fn chosen_experience(&self) -> Option<Experience> {
        self.chosen_experience
    }

    /// Set the common folder policy. Per-folder overrides remain intact.
    /// Folders that inherit or explicitly choose `Later` stay remembered but
    /// are excluded from the effective plan.
    pub fn set_mode(&mut self, mode: FolderMode) {
        self.default_mode = mode;
        for folder in &mut self.remembered_folders {
            if !folder.mode_overridden {
                folder.mode = mode;
            }
        }
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
    }

    /// Add a completed estimate and the destination explicitly chosen for it.
    ///
    /// A successful scan demonstrates only that enumeration just succeeded.
    /// In particular it never establishes that two-way synchronization is safe.
    ///
    /// # Errors
    ///
    /// Returns an error when `destination` is empty, duplicates another
    /// remembered destination, or contains/is contained by one.
    pub fn add_folder(&mut self, scan: FolderScan, destination: PathBuf) -> Result<(), String> {
        if destination.as_os_str().is_empty() {
            return Err("folder destination cannot be empty".to_string());
        }
        if self.destination_conflicts(&destination) {
            return Err("folder destination overlaps another selected destination".to_string());
        }
        self.remembered_folders.push(FolderChoice {
            scan,
            destination: FolderDestination::Resolved(destination),
            mode: self.default_mode,
            mode_overridden: false,
        });
        self.remembered_folder_identities
            .push(FolderIdentity::fresh());
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
        Ok(())
    }

    /// Remember a selected source while leaving its actual local target for
    /// the trusted first-login flow to resolve and review.
    pub fn add_folder_unresolved(&mut self, scan: FolderScan) {
        self.remembered_folders.push(FolderChoice {
            scan,
            destination: FolderDestination::Unresolved,
            mode: self.default_mode,
            mode_overridden: false,
        });
        self.remembered_folder_identities
            .push(FolderIdentity::fresh());
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
    }

    /// Whether a proposed destination duplicates or nests with a remembered
    /// destination. This is a lexical planning guard, not a filesystem grant.
    #[must_use]
    pub fn destination_conflicts(&self, destination: &Path) -> bool {
        self.remembered_folders.iter().any(|folder| {
            let FolderDestination::Resolved(existing) = &folder.destination else {
                return false;
            };
            existing.starts_with(destination) || destination.starts_with(existing)
        })
    }

    /// Remove a remembered folder by the index shown in the folder screen.
    pub fn remove_folder(&mut self, index: usize) -> bool {
        if index >= self.remembered_folders.len() {
            return false;
        }
        self.remembered_folders.remove(index);
        self.remembered_folder_identities.remove(index);
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
        true
    }

    /// Give one remembered folder its own mode. Returns false for a stale index.
    pub fn set_folder_mode(&mut self, index: usize, mode: FolderMode) -> bool {
        let Some(folder) = self.remembered_folders.get_mut(index) else {
            return false;
        };
        folder.mode = mode;
        folder.mode_overridden = true;
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
        true
    }

    /// Return one folder to the common mode. Returns false for a stale index.
    pub fn clear_folder_mode_override(&mut self, index: usize) -> bool {
        let Some(folder) = self.remembered_folders.get_mut(index) else {
            return false;
        };
        folder.mode = self.default_mode;
        folder.mode_overridden = false;
        self.invalidate_folder_review();
        self.rebuild_effective_folders();
        true
    }

    /// Advance one explicit page after validating the current navigation gate.
    ///
    /// # Errors
    ///
    /// Returns an error when no supported experience is selected, the selected
    /// experience is no longer supported, or Review is already visible.
    #[allow(
        clippy::should_implement_trait,
        reason = "next is the setup wizard navigation verb required by its public contract"
    )]
    pub fn next(&mut self) -> Result<(), String> {
        match self.page {
            SetupPage::Experience => {
                let Some(experience) = self.chosen_experience else {
                    return Err("choose an available experience before continuing".to_string());
                };
                if !self.capabilities.supports(experience) {
                    return Err("the selected experience is no longer available".to_string());
                }
                self.page = SetupPage::Folders;
                Ok(())
            }
            SetupPage::Folders => {
                self.page = SetupPage::Review;
                Ok(())
            }
            SetupPage::Review => Err("review is the final setup page".to_string()),
        }
    }

    /// Move back one explicit page while retaining all choices.
    pub fn back(&mut self) {
        self.page = match self.page {
            SetupPage::Experience | SetupPage::Folders => SetupPage::Experience,
            SetupPage::Review => SetupPage::Folders,
        };
    }

    /// The current effective setup estimate consumed by Review.
    ///
    /// Folders whose resulting mode is `Later` remain remembered but are not
    /// exposed here. Explicit Copy or Connected overrides remain effective even
    /// when the common mode is `Later`.
    #[must_use]
    pub fn plan(&self) -> &SetupPlan {
        &self.plan
    }

    /// Remembered folder details for the folder screen, including scans whose
    /// resulting mode is `Later` and therefore absent from the effective plan.
    #[must_use]
    pub fn folders(&self) -> &[FolderChoice] {
        &self.remembered_folders
    }

    /// Return the opaque, process-local identity paired with this folder.
    /// The token is safe to publish locally because it contains no path data.
    #[must_use]
    pub fn folder_token(&self, index: usize) -> Option<String> {
        self.remembered_folder_identities
            .get(index)
            .copied()
            .map(FolderIdentity::token)
    }

    /// Resolve a previously published folder identity against the current
    /// selection order. Removed identities never retarget a surviving folder.
    #[must_use]
    pub fn folder_index(&self, token: &str) -> Option<usize> {
        self.remembered_folder_identities
            .iter()
            .position(|identity| identity.token() == token)
    }

    /// Count selected source bytes independently of whether a destination has
    /// been resolved. This is an estimate, never copy authorization.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected byte estimates overflow `u64`.
    pub fn selected_folder_bytes(&self) -> Result<u64, String> {
        self.remembered_folders
            .iter()
            .filter(|folder| folder.mode != FolderMode::Later)
            .try_fold(0_u64, |total, folder| {
                total
                    .checked_add(folder.scan.bytes)
                    .ok_or_else(|| "selected folder estimate overflowed".to_string())
            })
    }

    fn invalidate_folder_review(&mut self) {
        if self.page == SetupPage::Review {
            self.page = SetupPage::Folders;
        }
    }

    fn rebuild_effective_folders(&mut self) {
        self.plan.folders = self
            .remembered_folders
            .iter()
            .filter_map(|remembered| {
                let FolderDestination::Resolved(destination) = &remembered.destination else {
                    return None;
                };
                (remembered.mode != FolderMode::Later).then(|| FolderSelection {
                    source: remembered.scan.source.clone(),
                    destination: destination.clone(),
                    bytes: remembered.scan.bytes,
                    mode: remembered.mode,
                    readable: true,
                    safe_sync: false,
                })
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::setup::folders::FolderScan;
    use crate::setup::model::{Capabilities, Experience, FolderMode, HostOs};

    #[cfg(windows)]
    const TEST_ROOT: &str = r"C:\tilo-setup-controller-tests";
    #[cfg(not(windows))]
    const TEST_ROOT: &str = "/tilo-setup-controller-tests";

    fn test_path(name: &str) -> PathBuf {
        PathBuf::from(TEST_ROOT).join(name)
    }

    fn linux_desktop() -> Capabilities {
        Capabilities {
            host: HostOs::Linux,
            desktop_session: true,
            internal_dual_boot: false,
        }
    }

    fn scan(source: &str, bytes: u64) -> FolderScan {
        FolderScan {
            source: test_path(source),
            file_count: 1,
            bytes,
            empty_directory_count: 0,
            empty_directories: Vec::new(),
            skipped_count: 0,
            skipped: Vec::new(),
        }
    }

    /// Desktop support, back navigation and capability loss.
    #[test]
    fn setup_navigation_keeps_folder_choices_and_blocks_on_lost_capability() {
        // Phase `supported_linux_desktop_advances_and_windows_desktop_does_not`.
        {
            let mut linux = SetupController::new(linux_desktop());
            assert!(linux.choose_experience(Experience::Desktop));
            assert_eq!(linux.next(), Ok(()));
            assert_eq!(linux.page(), SetupPage::Folders);

            let mut windows = SetupController::new(Capabilities {
                host: HostOs::Windows,
                desktop_session: false,
                internal_dual_boot: false,
            });
            assert!(!windows.choose_experience(Experience::Desktop));
            assert!(windows.next().is_err());
            assert_eq!(windows.page(), SetupPage::Experience);
        }

        // Phase `back_retains_folder_selections`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/Documents", 12), test_path("tilo/Documents"))
                .unwrap();
            setup.next().unwrap();

            setup.back();
            assert_eq!(setup.page(), SetupPage::Folders);
            assert_eq!(setup.plan().folders.len(), 1);
            assert_eq!(setup.plan().folders[0].bytes, 12);
        }

        // Phase `live_capability_loss_blocks_navigation_without_forgetting_choices`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/Documents", 12), test_path("tilo/Documents"))
                .unwrap();
            setup.back();

            setup.set_capabilities(Capabilities {
                host: HostOs::Linux,
                desktop_session: false,
                internal_dual_boot: false,
            });

            assert!(setup.next().is_err());
            assert_eq!(setup.folders().len(), 1);
            assert_eq!(setup.default_mode(), FolderMode::Copy);
        }
    }

    /// Later choices, remembered folders and overrides.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn later_and_remembered_folders_shape_the_effective_plan() {
        // Phase `common_later_preserves_explicit_copy_and_connected_overrides`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert_eq!(setup.default_mode(), FolderMode::Copy);
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/one", 1), test_path("tilo/one"))
                .unwrap();
            setup
                .add_folder(scan("source/two", 2), test_path("tilo/two"))
                .unwrap();
            setup
                .add_folder(scan("source/three", 3), test_path("tilo/three"))
                .unwrap();
            setup
                .add_folder(scan("source/four", 4), test_path("tilo/four"))
                .unwrap();
            assert!(setup.set_folder_mode(1, FolderMode::Copy));
            assert!(setup.set_folder_mode(2, FolderMode::Connected));
            assert!(setup.set_folder_mode(3, FolderMode::Later));

            setup.set_mode(FolderMode::Later);

            assert_eq!(setup.plan().folders.len(), 2);
            assert_eq!(setup.plan().folders[0].source, test_path("source/two"));
            assert_eq!(setup.plan().folders[0].mode, FolderMode::Copy);
            assert_eq!(setup.plan().folders[1].source, test_path("source/three"));
            assert_eq!(setup.plan().folders[1].mode, FolderMode::Connected);
            assert_eq!(setup.folders()[0].mode, FolderMode::Later);
            assert!(!setup.folders()[0].mode_overridden);
            assert_eq!(setup.folders()[3].mode, FolderMode::Later);
            assert!(setup.folders()[3].mode_overridden);

            setup.set_mode(FolderMode::Copy);

            assert_eq!(setup.plan().folders.len(), 3);
            assert_eq!(setup.plan().folders[0].source, test_path("source/one"));
            assert_eq!(setup.plan().folders[1].source, test_path("source/two"));
            assert_eq!(setup.plan().folders[2].source, test_path("source/three"));
        }

        // Phase `later_hides_remembered_folders_from_the_effective_plan`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/Documents", 12), test_path("tilo/Documents"))
                .unwrap();

            setup.set_mode(FolderMode::Later);

            assert!(setup.plan().folders.is_empty());
            setup.set_mode(FolderMode::Copy);
            assert_eq!(setup.plan().folders.len(), 1);
            assert_eq!(
                setup.plan().folders[0].source,
                test_path("source/Documents")
            );
        }

        // Phase `remembered_folder_details_support_view_removal_and_override_reset`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/one", 7), test_path("tilo/one"))
                .unwrap();
            setup
                .add_folder(scan("source/two", 9), test_path("tilo/two"))
                .unwrap();
            assert!(setup.set_folder_mode(0, FolderMode::Connected));
            assert!(setup.clear_folder_mode_override(0));
            setup.set_mode(FolderMode::Later);

            assert_eq!(setup.folders().len(), 2);
            assert_eq!(setup.folders()[0].scan.file_count, 1);
            assert_eq!(setup.folders()[0].mode, FolderMode::Later);
            assert!(!setup.folders()[0].mode_overridden);
            assert!(setup.remove_folder(0));
            assert_eq!(setup.folders().len(), 1);
            assert_eq!(setup.folders()[0].scan.bytes, 9);
            assert!(!setup.remove_folder(4));
        }
    }

    /// Review invalidation, destination validation and unresolved destinations.
    #[test]
    fn review_is_invalidated_and_unsafe_destinations_never_authorize_copy() {
        // Phase `a_change_invalidates_review_and_a_scan_never_infers_safe_sync`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();
            setup
                .add_folder(scan("source/Documents", 12), test_path("tilo/Documents"))
                .unwrap();
            setup.next().unwrap();
            assert_eq!(setup.page(), SetupPage::Review);
            assert!(!setup.plan().folders[0].safe_sync);

            setup.set_mode(FolderMode::Connected);

            assert_eq!(setup.page(), SetupPage::Folders);
            assert!(!setup.plan().folders[0].safe_sync);
        }

        // Phase `duplicate_and_nested_destinations_are_rejected`.
        {
            let mut setup = SetupController::new(linux_desktop());
            setup
                .add_folder(scan("source/one", 1), test_path("tilo/Documents"))
                .unwrap();

            for (source, destination) in [
                ("source/two", "tilo/Documents"),
                ("source/three", "tilo/Documents/Projects"),
                ("source/four", "tilo"),
            ] {
                let error = setup
                    .add_folder(scan(source, 1), test_path(destination))
                    .unwrap_err();
                assert!(error.contains("destination overlaps"), "{error}");
            }

            assert_eq!(setup.folders().len(), 1);
        }

        // Phase `unresolved_destination_retains_reviewed_folder_intent_without_authorizing_copy`.
        {
            let mut setup = SetupController::new(linux_desktop());
            assert!(setup.choose_experience(Experience::Desktop));
            setup.next().unwrap();

            setup.add_folder_unresolved(scan("source/Documents", 12));

            assert_eq!(setup.folders().len(), 1);
            assert!(matches!(
                &setup.folders()[0].destination,
                FolderDestination::Unresolved
            ));
            assert!(setup.plan().folders.is_empty());
            assert_eq!(setup.selected_folder_bytes(), Ok(12));
        }
    }
}
