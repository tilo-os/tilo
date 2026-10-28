//! The manifest of a pinned source tree.
//!
//! A source tree is read from Git's object database, not from a checkout: the
//! working directory holds build output, and what an import has to reproduce
//! is what Git says is tracked. `git ls-tree -r -z <tree>` names every entry
//! with its mode, kind and blob id, and `git cat-file --batch` returns the
//! bytes for those blobs without a worktree or an index.
//!
//! The manifest keeps three things a content-addressed import needs to be
//! honest: the exact bytes, the mode that decides executability, and every
//! entry that must *not* be carried in — build output, caches, the object
//! database itself.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// What a path in the source tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A regular file with data (mode `100644` or `100755`).
    File {
        /// Whether the mode carries an execute bit (`100755`).
        executable: bool,
    },
    /// A symbolic link whose data is the link target (mode `120000`).
    Symlink,
    /// A submodule (mode `160000`): Git records a commit, not bytes.
    Submodule,
    /// A directory. Never a `ls-tree` entry on its own — a recursive listing
    /// names only leaves — but a path the import creates and a record it
    /// keeps a chain for, so a checkpoint can reconstruct the tree's shape.
    Directory,
}

impl Kind {
    /// The mode an import reproduces.
    #[must_use]
    pub fn mode(self) -> u32 {
        match self {
            Self::File { executable: false } => 0o644,
            // A link is never executable, and is never followed on read; a
            // directory (or a submodule, which is one) holds no bytes at all,
            // so the bits only decide who may enter it.
            Self::File { executable: true } | Self::Directory | Self::Submodule => 0o755,
            Self::Symlink => 0o777,
        }
    }
}

/// One tracked path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Repository-relative path, `/`-separated, exactly as Git spells it.
    pub path: String,
    pub kind: Kind,
    /// `blake3` of the entry's bytes. For a submodule, of its commit id.
    pub hash: String,
    pub bytes: u64,
}

/// Every tracked path of one commit, and the paths deliberately left behind.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
    pub excluded: Vec<String>,
    /// Where the tree came from: a commit id, or `worktree`.
    pub origin: String,
}

/// A path whose bytes are not carried into the workspace, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exclusion {
    pub suffix: &'static str,
    pub reason: &'static str,
}

/// Build and cache output that is never tracked but is always present in a
/// developer's tree. Recorded rather than silently dropped: an import that
/// leaves them out is a decision, and the receipt says so.
pub const BUILD_OUTPUT: &[Exclusion] = &[
    Exclusion {
        suffix: "/target/",
        reason: "cargo build output",
    },
    Exclusion {
        suffix: "/.git/",
        reason: "Git's own object database",
    },
    Exclusion {
        suffix: "/node_modules/",
        reason: "downloaded JavaScript packages",
    },
    Exclusion {
        suffix: "/.cargo-cache/",
        reason: "cargo home inside the tree",
    },
];

impl Manifest {
    /// Whether `path` (repository-relative) belongs in the import.
    ///
    /// Git already refuses `target/` and `.git/` for a tracked path — a tree
    /// that tracked them would be broken — so this is the belt to the object
    /// database's braces. What it really guards is the *working directory*
    /// walk, which is how a tree without a `.git` is imported.
    #[must_use]
    pub fn excluded(path: &str) -> Option<&'static str> {
        let wrapped = format!("/{path}");
        BUILD_OUTPUT
            .iter()
            .find(|exclusion| wrapped.contains(exclusion.suffix))
            .map(|exclusion| exclusion.reason)
    }

    /// Total bytes of every carried entry.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    /// How many distinct blob ids the entries name: less than [`Self::len`]
    /// when several paths hold the same content.
    #[must_use]
    pub fn distinct_contents(&self) -> usize {
        let set: std::collections::BTreeSet<&str> = self
            .entries
            .iter()
            .map(|entry| entry.hash.as_str())
            .collect();
        set.len()
    }

    /// Entries by path, for a comparison against a second manifest.
    #[must_use]
    pub fn by_path(&self) -> BTreeMap<&str, &ManifestEntry> {
        self.entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum SourceTreeError {
    #[error("reading the working directory: {0}")]
    Io(std::io::ErrorKind),
}

/// A source tree read once, ready to be imported.
pub struct SourceTree {
    root: PathBuf,
    manifest: Manifest,
    blobs: BTreeMap<String, Vec<u8>>,
}

impl SourceTree {
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The bytes of a blob, by content hash.
    ///
    /// A hash the tree names without holding bytes for would be a hole in the
    /// import, so a caller has to say what it does with one rather than write
    /// an empty file in its place.
    #[must_use]
    pub fn blob(&self, hash: &str) -> Option<&[u8]> {
        self.blobs.get(hash).map(Vec::as_slice)
    }

    /// Read the working directory at `root` as a tree.
    ///
    /// What is on disk is what is imported, minus build output and the object
    /// database, so a warm build or Git's own storage cannot flatter either
    /// side of a comparison.
    ///
    /// # Errors
    ///
    /// Returns [`SourceTreeError`] when the directory cannot be read.
    pub fn from_directory(root: impl Into<PathBuf>) -> Result<Self, SourceTreeError> {
        let root = root.into();
        let mut manifest = Manifest {
            origin: "worktree".to_owned(),
            ..Manifest::default()
        };
        let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut stack = vec![root.clone()];
        while let Some(directory) = stack.pop() {
            let Some(relative) = directory.strip_prefix(&root).ok() else {
                continue;
            };
            if !relative.as_os_str().is_empty() {
                let path = relative.to_string_lossy().replace('\\', "/");
                if let Some(reason) = Manifest::excluded(&path) {
                    let mut line = String::new();
                    let _ = write!(line, "{path} ({reason})");
                    manifest.excluded.push(line);
                    continue;
                }
            }
            for entry in
                std::fs::read_dir(&directory).map_err(|error| SourceTreeError::Io(error.kind()))?
            {
                let entry = entry.map_err(|error| SourceTreeError::Io(error.kind()))?;
                let path = entry.path();
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    // A dangling link is still a tracked thing Git carries.
                    Err(_) => std::fs::symlink_metadata(&path)
                        .map_err(|error| SourceTreeError::Io(error.kind()))?,
                };
                if metadata.is_dir() {
                    stack.push(path);
                    continue;
                }
                let relative = path
                    .strip_prefix(&root)
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if let Some(reason) = Manifest::excluded(&relative) {
                    let mut line = String::new();
                    let _ = write!(line, "{relative} ({reason})");
                    manifest.excluded.push(line);
                    continue;
                }
                let (kind, bytes) = if metadata.file_type().is_symlink() {
                    let target = std::fs::read_link(&path)
                        .map_err(|error| SourceTreeError::Io(error.kind()))?;
                    (Kind::Symlink, target.to_string_lossy().as_bytes().to_vec())
                } else {
                    let executable =
                        std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111
                            != 0;
                    (
                        Kind::File { executable },
                        std::fs::read(&path).map_err(|error| SourceTreeError::Io(error.kind()))?,
                    )
                };
                let hash = blake3::hash(&bytes).to_hex().to_string();
                manifest.entries.push(ManifestEntry {
                    path: relative,
                    kind,
                    hash: hash.clone(),
                    bytes: bytes.len() as u64,
                });
                blobs.entry(hash).or_insert(bytes);
            }
        }
        manifest
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.excluded.sort();
        Ok(Self {
            root,
            manifest,
            blobs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_carry_executability_and_nothing_else_does() {
        assert_eq!(Kind::File { executable: true }.mode(), 0o755);
        assert_eq!(Kind::File { executable: false }.mode(), 0o644);
        assert_eq!(Kind::Directory.mode(), 0o755);
        assert_eq!(Kind::Submodule.mode(), 0o755);
        assert_eq!(Kind::Symlink.mode(), 0o777);
    }

    #[test]
    fn build_output_is_excluded_wherever_it_appears() {
        for path in [
            "target/debug/shell",
            "crates/system/fs/target/x",
            ".git/objects/pack/pack-1.pack",
            "apps/notes/node_modules/left-pad/index.js",
        ] {
            assert!(
                Manifest::excluded(path).is_some(),
                "{path} must not be imported"
            );
        }
        for path in [
            "crates/system/fs/src/lib.rs",
            "docs/targeting.md",
            ".github/x",
        ] {
            assert!(
                Manifest::excluded(path).is_none(),
                "{path} is source and must be imported"
            );
        }
    }
}
