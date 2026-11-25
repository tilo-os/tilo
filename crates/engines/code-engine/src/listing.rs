//! A folder as the project tree reads it: its folders and files, in an order a person reads,
//! leaving out what no one opens in a source tree.

use std::path::{Path, PathBuf};

pub use crate::disk::is_ignored_dir;

/// One folder's own entries, each by its name and where it stands.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    folders: Vec<(String, PathBuf)>,
    files: Vec<(String, PathBuf)>,
}

impl Listing {
    #[must_use]
    pub fn folders(&self) -> &[(String, PathBuf)] {
        &self.folders
    }

    #[must_use]
    pub fn files(&self) -> &[(String, PathBuf)] {
        &self.files
    }
}

/// The entries of the folder at `path`: its folders first, then its files, each run in the
/// order of its names, lowercased. Build output, VCS internals and virtual environments are
/// left out — a repository's `target/` alone is larger than any tree a person scrolls.
pub fn read_folder(path: &Path) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    for entry in std::fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let child_path = entry.path();
        // Skipped by *name*, not by type: in a git worktree `.git` is a file holding a gitdir
        // pointer, not a directory, and a directory-only test lets it through as a source file.
        if is_ignored_dir(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            listing.folders.push((name, child_path));
        } else if file_type.is_file() {
            listing.files.push((name, child_path));
        }
    }
    listing.folders.sort_by_key(|a| a.0.to_lowercase());
    listing.files.sort_by_key(|a| a.0.to_lowercase());
    Ok(listing)
}

/// Every folder from `root` down to the one holding `path`: what revealing a file in a tree
/// that was never expanded reads.
#[must_use]
pub fn read_folders_along(root: &Path, path: &Path) -> Vec<(PathBuf, std::io::Result<Listing>)> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Ok(rel) = parent.strip_prefix(root) else {
        return Vec::new();
    };
    let mut folder = root.to_path_buf();
    let mut read = vec![(folder.clone(), read_folder(&folder))];
    for component in rel.components() {
        folder = folder.join(component);
        read.push((folder.clone(), read_folder(&folder)));
    }
    read
}
