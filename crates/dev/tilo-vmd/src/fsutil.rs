//! Small filesystem helpers: atomic writes, tree copies and sizes.

use std::fs;
use std::path::Path;

use tilo_vm_protocol::layer_hash::walk;

use crate::error::DaemonError;

/// Replace `path` durably, creating its parent when missing, so readers see
/// the old or the new content and never a torn file.
///
/// # Errors
/// I/O failures.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    tilo_files::native::atomic::write_creating_parent(path, bytes)
        .map_err(DaemonError::io(format!("write {}", path.display())))
}

/// Recursively copy `source` into the existing directory `dest`, keeping
/// symlinks as symlinks and preserving permission bits.
///
/// # Errors
/// I/O failures and special files.
pub fn copy_tree(source: &Path, dest: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir(&to)?;
            copy_tree(&from, &to)?;
        } else if kind.is_symlink() {
            symlink(&fs::read_link(&from)?, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} is a special file",
                from.display()
            )));
        }
    }
    Ok(())
}

/// Bytes a tree occupies on disk (allocated blocks, not apparent size, so
/// reflinked or sparse files are not double counted as much). Unreadable
/// entries count as zero.
pub fn allocated_bytes(root: &Path) -> u64 {
    let mut entries = Vec::new();
    if walk(root, Path::new(""), &mut entries).is_err() {
        return 0;
    }
    entries
        .iter()
        .filter_map(|entry| fs::symlink_metadata(root.join(entry)).ok())
        .map(|meta| allocated(&meta))
        .fold(0_u64, u64::saturating_add)
}

#[cfg(unix)]
fn allocated(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(meta: &fs::Metadata) -> u64 {
    meta.len()
}

#[cfg(unix)]
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
pub fn symlink(_target: &Path, link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(format!(
        "cannot create symlink {} on this platform",
        link.display()
    )))
}

#[cfg(test)]
#[cfg(unix)]
pub fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(test)]
#[cfg(not(unix))]
pub fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
