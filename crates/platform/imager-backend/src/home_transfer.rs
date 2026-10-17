//! Whole-home import, including hidden files, with a private durable fidelity
//! phase layered over the existing verified byte-copy engine. No source writes.
//! An import is a new private directory; it is never merged into a live home.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod promotion;

#[cfg(target_os = "linux")]
pub use linux::{HomeReview, HomeWorker, review};
#[cfg(not(target_os = "linux"))]
pub use unsupported::{HomeReview, HomeWorker, review};

/// Validate fresh source, destination and journal grants before creating a review.
/// Raw paths are expectations, not transferable source authority.
///
/// # Errors
/// Refuses unsafe grants and changed inventories. Unsupported hosts return
/// [`std::io::ErrorKind::Unsupported`] before inspecting any supplied path.
pub fn review_io(
    source: &std::path::Path,
    destination: &std::path::Path,
    journal: &std::path::Path,
) -> std::io::Result<HomeReview> {
    #[cfg(target_os = "linux")]
    {
        linux::review(source, destination, journal).map_err(std::io::Error::other)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, destination, journal);
        Err(unsupported::error())
    }
}

#[cfg(not(target_os = "linux"))]
mod unsupported {
    use crate::migration::{CopyProgress, CopyReport};
    use crate::migration_review::InventorySummary;
    use crate::migration_worker::WorkerEvent;
    use std::{convert::Infallible, io, path::Path, sync::atomic::AtomicBool};

    pub(super) fn error() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Whole-home transfer is currently supported on Linux only.",
        )
    }

    /// Unavailable on this host; only the validating factory can create reviews.
    #[derive(Clone)]
    pub struct HomeReview {
        unavailable: Infallible,
        pub summary: InventorySummary,
    }

    /// Compatibility wrapper retaining the existing display-error API.
    pub fn review(source: &Path, destination: &Path, journal: &Path) -> Result<HomeReview, String> {
        super::review_io(source, destination, journal).map_err(|error| error.to_string())
    }

    impl HomeReview {
        pub fn destination(&self) -> &Path {
            match self.unavailable {}
        }

        pub fn recover(source: &Path, destination: &Path, journal: &Path) -> Result<Self, String> {
            review(source, destination, journal)
        }

        pub fn verify_after_applications(&self) -> Result<CopyReport, String> {
            Err(error().to_string())
        }

        pub fn verify_flatpak_installation(&self) -> Result<CopyReport, String> {
            Err(error().to_string())
        }

        pub fn verify_completed_tree(&self) -> Result<CopyReport, String> {
            Err(error().to_string())
        }

        pub fn retain_review(&self) -> Result<(), String> {
            Err(error().to_string())
        }

        pub fn start(&self) -> Result<HomeWorker, String> {
            Err(error().to_string())
        }

        pub fn retry(&self) -> Result<HomeWorker, String> {
            Err(error().to_string())
        }

        pub fn transfer(
            &self,
            _retry: bool,
            _cancel: &AtomicBool,
            _progress: impl FnMut(CopyProgress),
        ) -> Result<CopyReport, String> {
            Err(error().to_string())
        }
    }

    /// A worker cannot be created on an unsupported host.
    pub struct HomeWorker {
        unavailable: Infallible,
    }

    impl HomeWorker {
        pub fn cancel(&self) -> bool {
            match self.unavailable {}
        }

        pub fn try_next(&mut self) -> Option<WorkerEvent> {
            match self.unavailable {}
        }
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod unsupported_tests {
    #[test]
    fn review_factory_refuses_before_resolving_paths() {
        let path = std::path::Path::new("missing-relative-path");
        assert!(matches!(
            super::review_io(path, path, path),
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(super::review(path, path, path).is_err());
    }
}

/// Resolve the home of the account running the installer. Merely resolving
/// this path does not scan it or authorize a transfer.
///
/// # Errors
/// Refuses unsupported hosts, a missing or invalid home, and unsafe root resolution.
pub fn current_home() -> Result<std::path::PathBuf, String> {
    if !cfg!(target_os = "linux") {
        return Err("Whole-home transfer is currently supported on Linux only.".into());
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("The current account has no home directory.")?;
    if !home.is_absolute() || home.parent().is_none() {
        return Err("The current home directory is not a valid absolute directory.".into());
    }
    granted_root(&home)
        .map_err(|_| "The current home directory could not be resolved safely.".into())
}

/// Resolve only an explicitly granted root, such as Fedora's /home alias.
/// Members beneath the returned directory still use the no-follow migration
/// guards; this is not permission to resolve arbitrary inventory entries.
///
/// # Errors
/// Refuses relative or filesystem-root paths, non-directories, changed namespaces and I/O failures.
pub fn granted_root(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The granted root must be an absolute directory.",
        ));
    }
    let canonical = path.canonicalize()?;
    if canonical.parent().is_none() || !canonical.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The granted root must be a directory below the filesystem root.",
        ));
    }
    super::migration::PinnedDirectory::open_absolute(&canonical)?.validate()?;
    Ok(canonical)
}

#[cfg(all(test, target_os = "linux"))]
mod root_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn granted_home_alias_uses_canonical_root_and_retains_no_follow_member_guards() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let actual = base.join("var/home/person");
        std::fs::create_dir_all(actual.join(".config")).unwrap();
        symlink(base.join("var/home"), base.join("home")).unwrap();
        let granted = granted_root(&base.join("home/person")).unwrap();
        assert_eq!(granted, actual);
        let pin = super::super::migration::PinnedDirectory::open_absolute(&granted).unwrap();
        std::fs::rename(&actual, base.join("old-home")).unwrap();
        std::fs::create_dir(&actual).unwrap();
        assert!(pin.validate().is_err());
        symlink(base.join("old-home/.config"), actual.join(".config")).unwrap();
        assert!(
            super::super::migration::PinnedDirectory::open_absolute(&actual.join(".config"))
                .is_err()
        );
    }
}

/// On-disk home adoption facts shared with recovery helpers.
pub mod protocol;
/// Durable installed-home seed authority and witnesses.
pub mod seeds;
