//! Canonical content identity shared by host cache lookup and daemon import.
//! Keep paths, executable bits and bytes in the identity; exclude only the
//! metadata the importer writes itself.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const RELEASE_DIR: &str = "usr/lib/extension-release.d";
const PROVENANCE: &str = "provenance.json";

/// Hash the layer's files and links in path order, independent of timestamps.
///
/// # Errors
/// Unreadable entries and special files (which cannot be layer payload).
pub fn tree_hash(root: &Path) -> std::io::Result<String> {
    let mut entries = Vec::new();
    walk(root, Path::new(""), &mut entries)?;
    entries.sort();
    let mut hasher = Sha256::new();
    for relative in entries {
        let text = relative.to_string_lossy().replace('\\', "/");
        if text == PROVENANCE || text == RELEASE_DIR || text.starts_with(&format!("{RELEASE_DIR}/"))
        {
            continue;
        }
        let path = root.join(&relative);
        let meta = fs::symlink_metadata(&path)?;
        let kind = meta.file_type();
        let (tag, content) = if kind.is_dir() {
            // Directories are implied by the paths below them; empty ones
            // carry nothing a sysext needs.
            continue;
        } else if kind.is_symlink() {
            let target = fs::read_link(&path)?;
            ("l", target.to_string_lossy().into_owned())
        } else if kind.is_file() {
            let tag = if is_executable(&meta) { "x" } else { "f" };
            (tag, file_digest(&path)?)
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{text} is not a file, directory or symlink"),
            ));
        };
        for part in [tag, text.as_str(), content.as_str()] {
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

fn walk(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
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
