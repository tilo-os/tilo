//! Closed source inventory and exclusive candidate copies; no live editor save is implied.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

#[derive(Clone, Serialize)]
pub(super) struct Entry {
    bytes: u64,
    sha256: String,
    executable: bool,
}
pub(super) type Inventory = BTreeMap<String, Entry>;
pub(super) fn relative(path: &str) -> Result<(), String> {
    if path.split('/').count() > 128
        || path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path
            .split('/')
            .any(|part| matches!(part, ".git" | "target" | "__pycache__"))
    {
        return Err("source path is outside the closed source namespace".into());
    }
    Ok(())
}
fn regular(path: &Path) -> Result<(File, std::fs::Metadata), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("source entry is not a regular file".into());
    }
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    if !path.is_absolute() || components.is_empty() {
        return Err("source path is not absolute".into());
    }
    let mut file = File::open("/").map_err(|error| error.to_string())?;
    for (index, name) in components.iter().enumerate() {
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if index.checked_add(1) == Some(components.len()) {
                0
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: live directory fd, NUL-terminated component and fixed read-only flags.
        let descriptor = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: this successful openat returned a new, exclusively owned fd.
        file = unsafe { File::from_raw_fd(descriptor) };
    }
    let actual = file.metadata().map_err(|error| error.to_string())?;
    if actual.ino() != metadata.ino()
        || actual.dev() != metadata.dev()
        || actual.len() > 64 * 1024 * 1024
    {
        return Err("source file changed or exceeds bound".into());
    }
    Ok((file, actual))
}
fn walk(
    root: &Path,
    directory: &Path,
    entries: &mut Inventory,
    bounds: &mut (u64, usize),
) -> Result<(), String> {
    let directory_metadata =
        std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !directory_metadata.is_dir() {
        return Err("source directory is not an exact directory".into());
    }
    for item in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        bounds.1 = bounds
            .1
            .checked_add(1)
            .ok_or("source entry count overflow")?;
        if bounds.1 > 100_000 {
            return Err("source inventory entry bound exceeded".into());
        }
        let path = item.map_err(|error| error.to_string())?.path();
        let name = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_str()
            .ok_or("source path is not UTF-8")?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if name == "target" && metadata.is_dir() {
            if std::fs::read_dir(&path)
                .map_err(|error| error.to_string())?
                .next()
                .is_some()
            {
                return Err("source target mountpoint must be empty".into());
            }
            continue;
        }
        relative(name)?;
        if metadata.is_dir() {
            walk(root, &path, entries, bounds)?;
        } else {
            if entries.len() >= 100_000 {
                return Err("source inventory file bound exceeded".into());
            }
            let (mut file, actual) = regular(&path)?;
            let mut digest = Sha256::new();
            let mut buffer = vec![0_u8; 65536];
            let mut bytes = 0_u64;
            loop {
                let got = file.read(&mut buffer).map_err(|error| error.to_string())?;
                if got == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(u64::try_from(got).map_err(|error| error.to_string())?)
                    .ok_or("source size overflow")?;
                if bytes > actual.len() {
                    return Err("source file grew during inventory".into());
                }
                digest.update(
                    buffer
                        .get(..got)
                        .ok_or("source read count exceeds buffer")?,
                );
            }
            if bytes != actual.len() {
                return Err("source file changed during inventory".into());
            }
            bounds.0 = bounds.0.checked_add(bytes).ok_or("source size overflow")?;
            if bounds.0 > 1024 * 1024 * 1024 {
                return Err("source inventory byte bound exceeded".into());
            }
            entries.insert(
                name.to_owned(),
                Entry {
                    bytes,
                    sha256: hex::encode(digest.finalize()),
                    executable: actual.permissions().mode() & 0o100 != 0,
                },
            );
        }
    }
    Ok(())
}
pub(super) fn inventory(root: &Path) -> Result<Inventory, String> {
    if !root.is_absolute() || root.canonicalize().map_err(|error| error.to_string())? != root {
        return Err("source root is not an exact absolute directory".into());
    }
    let mut entries = Inventory::new();
    walk(root, root, &mut entries, &mut (0, 0))?;
    if entries.is_empty() {
        return Err("source inventory is empty".into());
    }
    Ok(entries)
}
pub(super) fn identity(entries: &Inventory) -> Result<String, String> {
    let bytes = serde_json::to_vec(entries).map_err(|error| error.to_string())?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
pub(super) fn verify(root: &Path, expected: &str) -> Result<(), String> {
    if identity(&inventory(root)?)? != expected {
        return Err("actual candidate source inventory changed".into());
    }
    Ok(())
}
pub(super) fn materialize(
    baseline: &Path,
    expected: &str,
    destination: &Path,
    replacements: &BTreeMap<String, Vec<u8>>,
) -> Result<Inventory, String> {
    let entries = inventory(baseline)?;
    if replacements.len() > 128
        || replacements
            .values()
            .any(|bytes| bytes.len() > 2 * 1024 * 1024)
    {
        return Err("candidate source replacements exceed bound".into());
    }
    if identity(&entries)? != expected
        || replacements.keys().any(|path| !entries.contains_key(path))
    {
        return Err("baseline identity or allowed replacement source changed".into());
    }
    std::fs::create_dir(destination).map_err(|error| error.to_string())?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    for (name, entry) in entries {
        relative(&name)?;
        let path = destination.join(&name);
        let parent = path.parent().ok_or("candidate parent absent")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let data = if let Some(bytes) = replacements.get(&name) {
            bytes.clone()
        } else {
            let (mut source, metadata) = regular(&baseline.join(&name))?;
            let mut data = Vec::new();
            Read::by_ref(&mut source)
                .take(metadata.len().saturating_add(1))
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            if u64::try_from(data.len()).map_err(|error| error.to_string())? != entry.bytes
                || hex::encode(Sha256::digest(&data)) != entry.sha256
            {
                return Err("baseline changed during candidate materialization".into());
            }
            data
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if entry.executable { 0o700 } else { 0o600 })
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| error.to_string())?;
        output
            .write_all(&data)
            .and_then(|()| output.sync_all())
            .map_err(|error| error.to_string())?;
    }
    std::fs::create_dir(destination.join("target")).map_err(|error| error.to_string())?;
    File::open(destination)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    if identity(&inventory(baseline)?)? != expected {
        return Err("baseline changed during candidate materialization".into());
    }
    inventory(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let owned = tempfile::tempdir().unwrap();
        let baseline = owned.path().canonicalize().unwrap().join("baseline");
        std::fs::create_dir(&baseline).unwrap();
        std::fs::write(baseline.join("source.rs"), b"old source\n").unwrap();
        std::fs::write(baseline.join("fixed.txt"), b"unchanged\n").unwrap();
        (owned, baseline)
    }

    #[test]
    fn source_projection_materializes_only_the_closed_source_into_a_fresh_candidate() {
        let (owned, baseline) = fixture();
        let before = inventory(&baseline).unwrap();
        let digest = identity(&before).unwrap();
        let candidate = owned.path().canonicalize().unwrap().join("candidate");
        let replacements = BTreeMap::from([("source.rs".into(), b"new source\n".to_vec())]);
        let result = materialize(&baseline, &digest, &candidate, &replacements).unwrap();
        assert_ne!(identity(&result).unwrap(), digest);
        assert_eq!(
            std::fs::read(candidate.join("source.rs")).unwrap(),
            b"new source\n"
        );
        assert_eq!(
            std::fs::read(candidate.join("fixed.txt")).unwrap(),
            b"unchanged\n"
        );
        assert_eq!(identity(&inventory(&baseline).unwrap()).unwrap(), digest);
        let candidate_digest = identity(&result).unwrap();
        verify(&candidate, &candidate_digest).unwrap();
        assert!(materialize(&baseline, &digest, &candidate, &replacements).is_err());
        assert_eq!(
            std::fs::read(candidate.join("source.rs")).unwrap(),
            b"new source\n"
        );
        std::fs::write(candidate.join("source.rs"), b"tampered source").unwrap();
        assert!(verify(&candidate, &candidate_digest).is_err());
        drop(owned);
    }

    #[test]
    fn source_projection_refuses_unknown_replacements_and_changed_baselines_before_creation() {
        let (owned, baseline) = fixture();
        let digest = identity(&inventory(&baseline).unwrap()).unwrap();
        let candidate = owned.path().canonicalize().unwrap().join("candidate");
        let unknown = BTreeMap::from([("grader.rs".into(), b"foreign".to_vec())]);
        assert!(materialize(&baseline, &digest, &candidate, &unknown).is_err());
        assert!(!candidate.exists());
        std::fs::write(baseline.join("source.rs"), b"changed externally").unwrap();
        assert!(materialize(&baseline, &digest, &candidate, &BTreeMap::new()).is_err());
        assert!(!candidate.exists());
        drop(owned);
    }

    #[test]
    fn source_projection_refuses_symlinks_history_and_build_outputs() {
        let (owned, baseline) = fixture();
        let outside = owned.path().join("outside");
        std::fs::write(&outside, b"private").unwrap();
        let link = baseline.join("source-link.rs");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(inventory(&baseline).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::create_dir(baseline.join(".git")).unwrap();
        assert!(inventory(&baseline).is_err());
        std::fs::remove_dir(baseline.join(".git")).unwrap();
        std::fs::create_dir(baseline.join("target")).unwrap();
        assert!(
            inventory(&baseline).is_ok(),
            "empty target is only a mountpoint"
        );
        std::fs::write(baseline.join("target/private"), b"build result").unwrap();
        assert!(inventory(&baseline).is_err());
        drop(owned);
    }
}
