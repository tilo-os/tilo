//! Canonical content identity shared by host cache lookup and daemon import.
//! Keep paths, executable bits and bytes in the identity; exclude only the
//! metadata the importer writes itself.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Written by the importer, so excluded from the content hash.
pub const RELEASE_DIR: &str = "usr/lib/extension-release.d";
/// Metadata beside the tree, also excluded from the hash.
pub const PROVENANCE: &str = "provenance.json";

/// One file or symlink a layer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEntry {
    /// Relative to the layer root, `/`-separated.
    pub path: String,
    pub content: EntryContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryContent {
    Link(String),
    File { sha256: String, executable: bool },
}

/// The layer's files and links in path order, without the importer's own
/// metadata. Directories are implied by the paths below them; empty ones
/// carry nothing a sysext needs.
///
/// # Errors
/// Unreadable entries, and special files (which cannot be layer payload) as
/// [`std::io::ErrorKind::InvalidData`].
pub fn entries(root: &Path) -> std::io::Result<Vec<LayerEntry>> {
    let mut paths = Vec::new();
    walk(root, Path::new(""), &mut paths)?;
    paths.sort();
    let mut entries = Vec::new();
    for relative in paths {
        let text = relative.to_string_lossy().replace('\\', "/");
        if text == PROVENANCE || text == RELEASE_DIR || text.starts_with(&format!("{RELEASE_DIR}/"))
        {
            continue;
        }
        let path = root.join(&relative);
        let meta = fs::symlink_metadata(&path)?;
        let kind = meta.file_type();
        let content = if kind.is_dir() {
            continue;
        } else if kind.is_symlink() {
            EntryContent::Link(fs::read_link(&path)?.to_string_lossy().into_owned())
        } else if kind.is_file() {
            EntryContent::File {
                sha256: file_digest(&path)?,
                executable: is_executable(&meta),
            }
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{text} is not a file, directory or symlink"),
            ));
        };
        entries.push(LayerEntry {
            path: text,
            content,
        });
    }
    Ok(entries)
}

/// Hash the layer's [`entries`], independent of timestamps.
///
/// # Errors
/// As [`entries`].
pub fn tree_hash(root: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    for entry in entries(root)? {
        let (tag, content) = match &entry.content {
            EntryContent::Link(target) => ("l", target.as_str()),
            EntryContent::File { sha256, executable } => {
                (if *executable { "x" } else { "f" }, sha256.as_str())
            }
        };
        for part in [tag, entry.path.as_str(), content] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn file_digest(path: &Path) -> std::io::Result<String> {
    use std::io::BufRead as _;
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        let read = chunk.len();
        reader.consume(read);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Collect every path below `root` (relative, depth-first), not following symlinks.
///
/// # Errors
/// I/O failures.
pub fn walk(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let is_dir = entry.file_type()?.is_dir();
        out.push(child.clone());
        if is_dir {
            walk(root, &child, out)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}
