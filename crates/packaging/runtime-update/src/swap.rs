//! Replacing the installed bundle with a verified new one.

use std::{
    io,
    path::{Path, PathBuf},
};

/// Replace the bundle at `current` with the one at `new`, which must be a
/// sibling so both moves are renames on one filesystem. There is never a
/// moment without a bundle at `current` longer than two renames, and if the
/// second rename fails the old bundle is put back.
pub fn replace_bundle(current: &Path, new: &Path) -> io::Result<()> {
    if current.parent() != new.parent() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the new bundle must be unpacked beside the installed one",
        ));
    }
    let retired = retired_path(current);
    std::fs::rename(current, &retired)?;
    if let Err(error) = std::fs::rename(new, current) {
        std::fs::rename(&retired, current)?;
        return Err(error);
    }
    std::fs::remove_dir_all(&retired)
}

fn retired_path(current: &Path) -> PathBuf {
    let mut name = current.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".replaced-{}", std::process::id()));
    current.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(root: &Path, name: &str, marker: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.join("Contents")).unwrap();
        std::fs::write(path.join("Contents/version"), marker).unwrap();
        path
    }

    #[test]
    fn the_new_bundle_takes_the_installed_place_and_the_old_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let current = bundle(dir.path(), "Tilo.app", "old");
        let new = bundle(dir.path(), ".Tilo.app.update", "new");
        replace_bundle(&current, &new).unwrap();
        assert_eq!(
            std::fs::read_to_string(current.join("Contents/version")).unwrap(),
            "new"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "only the installed bundle remains");
    }

    #[test]
    fn a_failed_swap_keeps_the_installed_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let current = bundle(dir.path(), "Tilo.app", "old");
        let missing = dir.path().join(".Tilo.app.update");
        assert!(replace_bundle(&current, &missing).is_err());
        assert_eq!(
            std::fs::read_to_string(current.join("Contents/version")).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_new_bundle_elsewhere_is_refused_before_anything_moves() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let current = bundle(dir.path(), "Tilo.app", "old");
        let new = bundle(other.path(), "Tilo.app", "new");
        assert_eq!(
            replace_bundle(&current, &new).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(current.join("Contents/version").exists());
    }
}
