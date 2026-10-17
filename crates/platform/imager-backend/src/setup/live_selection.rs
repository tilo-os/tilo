//! Source-grant attestation and private session storage for migration review.
use super::{flatpak_transfer::InstalledRef, migration_intent::MigrationIntent};
use std::path::Path;
#[derive(Debug)]
pub enum ReviewFailure {
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
    pub fn key(&self) -> &'static str {
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

pub fn review(
    selected: &Path,
    home: &Path,
    categories: [bool; 3],
    refs: Vec<InstalledRef>,
) -> Result<MigrationIntent, ReviewFailure> {
    review_inner(
        selected,
        home,
        categories,
        refs,
        #[cfg(any(test, feature = "test-support"))]
        None,
    )
}
#[cfg(any(test, feature = "test-support"))]
pub fn review_using(
    selected: &Path,
    home: &Path,
    categories: [bool; 3],
    refs: Vec<InstalledRef>,
    capture: Option<super::migration_intent::Capture>,
) -> Result<MigrationIntent, ReviewFailure> {
    review_inner(selected, home, categories, refs, capture)
}
fn review_inner(
    selected: &Path,
    home: &Path,
    categories: [bool; 3],
    refs: Vec<InstalledRef>,
    #[cfg(any(test, feature = "test-support"))] capture: Option<super::migration_intent::Capture>,
) -> Result<MigrationIntent, ReviewFailure> {
    #[cfg(target_os = "linux")]
    {
        native::review(
            selected,
            home,
            categories,
            refs,
            #[cfg(any(test, feature = "test-support"))]
            capture,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (selected, home, categories, refs);
        #[cfg(any(test, feature = "test-support"))]
        let _ = capture;
        Err(ReviewFailure::UnsupportedSource)
    }
}
#[cfg(target_os = "linux")]
pub mod native {
    use super::*;
    use crate::migration::PinnedDirectory;
    use std::{path::PathBuf, sync::Arc};
    pub fn review(
        selected: &Path,
        home: &Path,
        categories: [bool; 3],
        refs: Vec<InstalledRef>,
        #[cfg(any(test, feature = "test-support"))] capture: Option<
            super::super::migration_intent::Capture,
        >,
    ) -> Result<MigrationIntent, ReviewFailure> {
        let (selected_pin, expected_pin) = validate_home_grant(selected, home)?;
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
        crate::setup::migration_durability::sync_directory(&parent.file)
            .map_err(ReviewFailure::private_state)?;
        let policy = if categories[0] {
            crate::setup::migration_handoff::HomePolicy::AccessReadWrite
        } else {
            crate::setup::migration_handoff::HomePolicy::Copy
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let intent = MigrationIntent::capture_reviewed_with_policy(
            selected, &directory, categories, refs, policy,
        );
        #[cfg(any(test, feature = "test-support"))]
        let intent = if let Some(capture) = capture {
            MigrationIntent::capture_using(selected, &directory, categories, refs, policy, capture)
        } else {
            MigrationIntent::capture_reviewed_with_policy(
                selected, &directory, categories, refs, policy,
            )
        };
        // Independently observed grant substitution takes precedence even
        // when capture failed; arbitrary InvalidData never implies it.
        reattest_grant(&selected_pin, &expected_pin)?;
        let intent = intent.map_err(ReviewFailure::capture)?;
        Ok(intent)
    }
    pub fn validate_home_grant(
        selected: &Path,
        expected: &Path,
    ) -> Result<(Arc<PinnedDirectory>, Arc<PinnedDirectory>), ReviewFailure> {
        let selected =
            PinnedDirectory::open_absolute(selected).map_err(|_| ReviewFailure::Grant)?;
        let expected =
            PinnedDirectory::open_absolute(expected).map_err(|_| ReviewFailure::Grant)?;
        selected.validate().map_err(|_| ReviewFailure::Grant)?;
        expected.validate().map_err(|_| ReviewFailure::Grant)?;
        if !crate::nofollow::same_directory_identity(&selected.identity, &expected.identity) {
            return Err(ReviewFailure::Grant);
        }
        Ok((selected, expected))
    }
    pub fn reattest_grant(
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
                crate::destination_security::check_private_file(&pin.file)?;
                Ok(pin)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = ensure_private_tree(path.parent().ok_or(error)?)?;
                let child = parent.create_child(
                    path.file_name()
                        .ok_or_else(|| std::io::Error::other("Missing state name"))?,
                )?;
                crate::migration_durability::sync_directory(&parent.file)?;
                Ok(child)
            }
            Err(error) => Err(error),
        }
    }
}
