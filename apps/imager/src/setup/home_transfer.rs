//! Whole-home import, including hidden files, with a private durable fidelity
//! phase layered over the existing verified byte-copy engine. No source writes.
//! An import is a new private directory; it is never merged into a live home.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(in crate::setup) mod promotion;

#[cfg(target_os = "linux")]
pub(super) use linux::{HomeReview, review};

/// Resolve the home of the account running the installer. Merely resolving
/// this path does not scan it or authorize a transfer.
pub(super) fn current_home() -> Result<std::path::PathBuf, String> {
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
pub(in crate::setup) fn granted_root(
    path: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
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
