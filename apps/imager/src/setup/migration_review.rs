//! Bounded metadata inventory binding a review to the copy engine's snapshots.
//! This is change detection, not a content snapshot or new copy authority.
use super::migration::{PinnedDirectory, changed};
use super::migration_journal::Snapshot;
use super::migration_store::NativePath;
use super::nofollow::metadata_nofollow;
use cap_primitives::fs::Metadata;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_ENTRIES: usize = 100_000;

#[derive(PartialEq, Eq, Serialize)]
struct Fact {
    kind: u8,
    device: u64,
    snapshot: Snapshot,
}
impl Fact {
    fn capture(metadata: &Metadata) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        Ok(Self {
            kind: if metadata.is_symlink() {
                0
            } else if metadata.is_dir() {
                1
            } else if metadata.is_file() {
                2
            } else {
                3
            },
            device: metadata.dev(),
            snapshot: Snapshot::new(metadata)?,
        })
    }
}

pub(super) struct SourceInventory {
    facts: BTreeMap<PathBuf, Fact>,
}
impl SourceInventory {
    pub(super) fn capture(root: &Arc<PinnedDirectory>) -> io::Result<Self> {
        let mut inventory = Self {
            facts: BTreeMap::new(),
        };
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }

    fn walk(
        &mut self,
        directory: &Arc<PinnedDirectory>,
        relative: &Path,
        depth: usize,
    ) -> io::Result<()> {
        directory.validate()?;
        let before = Metadata::from_file(&directory.file)?;
        self.insert(relative, &before)?;
        let opened = cap_std::fs::Dir::from_std_file(directory.file.try_clone()?);
        for entry in opened.entries()? {
            let name = entry?.file_name();
            let path = relative.join(&name);
            let metadata = metadata_nofollow(&directory.file, Path::new(&name))?;
            if metadata.is_dir() && !metadata.is_symlink() {
                if depth >= 128 {
                    return Err(changed(
                        "Source review exceeds depth 128; select a smaller folder.",
                    ));
                }
                self.walk(&directory.open_child(&name, &metadata)?, &path, depth + 1)?;
            } else {
                self.insert(&path, &metadata)?;
            }
        }
        directory.validate()?;
        if Fact::capture(&before)? != Fact::capture(&Metadata::from_file(&directory.file)?)? {
            return Err(changed(
                "Source changed during review; review the folder again.",
            ));
        }
        Ok(())
    }

    fn insert(&mut self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        if self.facts.len() >= MAX_ENTRIES || path.as_os_str().len() > 4096 {
            return Err(changed(
                "Source review exceeds 100000 entries or path length 4096; select a smaller folder.",
            ));
        }
        self.facts.insert(path.to_owned(), Fact::capture(metadata)?);
        Ok(())
    }

    pub(super) fn revision(&self) -> io::Result<[u8; 32]> {
        let mut digest = Sha256::new();
        for (path, fact) in &self.facts {
            let bytes = serde_json::to_vec(&(NativePath::encode(path), fact))?;
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(digest.finalize().into())
    }

    pub(super) fn check(&self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        if self.facts.get(path) != Some(&Fact::capture(metadata)?) {
            return Err(changed(
                "Source contents changed after review; explicitly review the folder again.",
            ));
        }
        Ok(())
    }

    pub(super) fn validate(&self, root: &Arc<PinnedDirectory>) -> io::Result<()> {
        if Self::capture(root)?.revision()? != self.revision()? {
            return Err(changed(
                "Source contents changed after review; explicitly review the folder again.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn review_depth_limit_refuses_without_creating_copy_output() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut path = root.clone();
        for _ in 0..129 {
            path.push("d");
            std::fs::create_dir(&path).unwrap();
        }
        let error = match SourceInventory::capture(&PinnedDirectory::open_absolute(&root).unwrap())
        {
            Ok(_) => panic!("deep inventory must require a smaller selection"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("select a smaller folder"));
    }
}
