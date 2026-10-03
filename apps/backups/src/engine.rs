//! A small, dependency-light local backup repository.
//!
//! Each snapshot is written into a private staging directory, every regular
//! file is hashed while it is copied, and the directory is renamed into place
//! only after its manifest has reached disk. Interrupted jobs therefore never
//! appear in [`list_snapshots`]. Restores default to a new or empty directory
//! and verify every file before reporting success.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use ui::time::UNIX_EPOCH;
use walkdir::WalkDir;

const REPOSITORY_DIR: &str = ".tilo-backups";
const SNAPSHOTS_DIR: &str = "snapshots";
const MANIFEST: &str = "manifest.json";

#[derive(Debug)]
pub enum BackupError {
    Io(io::Error),
    InvalidSource(String),
    UnsafeDestination(String),
    InvalidSnapshot(String),
    Integrity(String),
    Json(serde_json::Error),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidSource(s)
            | Self::UnsafeDestination(s)
            | Self::InvalidSnapshot(s)
            | Self::Integrity(s) => f.write_str(s),
            Self::Json(e) => write!(f, "invalid backup metadata: {e}"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<io::Error> for BackupError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for BackupError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type Result<T> = std::result::Result<T, BackupError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub sha256: Option<String>,
    pub link_target: Option<PathBuf>,
    /// Unix permission bits; `null` on platforms without them. The key is
    /// required in every manifest.
    #[serde(deserialize_with = "Option::deserialize")]
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub version: u32,
    pub id: String,
    pub created_unix_seconds: u64,
    pub source_name: String,
    pub files: u64,
    pub bytes: u64,
    pub entries: Vec<BackupEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub current: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verification {
    pub files: u64,
    pub bytes: u64,
}

pub fn repository_path(destination: &Path) -> PathBuf {
    destination.join(REPOSITORY_DIR)
}

/// Validate source and destination before creating anything in the repository.
/// Nested paths are rejected in both directions to prevent recursive backups
/// and a restore/backup accidentally modifying its own source.
///
/// # Errors
///
/// Returns an error when the source cannot be resolved to a directory, the
/// destination cannot be created or resolved, or either path contains the
/// other.
pub fn validate_paths(source: &Path, destination: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = source.canonicalize().map_err(|e| {
        BackupError::InvalidSource(format!("Cannot open source {}: {e}", source.display()))
    })?;
    if !source.is_dir() {
        return Err(BackupError::InvalidSource(format!(
            "Backup source is not a folder: {}",
            source.display()
        )));
    }
    fs::create_dir_all(destination)?;
    let destination = destination.canonicalize()?;
    if source == destination || source.starts_with(&destination) || destination.starts_with(&source)
    {
        return Err(BackupError::UnsafeDestination(
            "Choose a destination outside the folder being backed up".into(),
        ));
    }
    Ok((source, destination))
}

/// Create a checksummed snapshot and report progress after each copied file.
///
/// # Errors
///
/// Returns an error when the paths are unsafe, a source entry cannot be read
/// or represented safely, or the snapshot data or manifest cannot be written.
pub fn create_snapshot(
    source: &Path,
    destination: &Path,
    mut on_progress: impl FnMut(&Progress),
) -> Result<BackupManifest> {
    let (source, destination) = validate_paths(source, destination)?;
    let snapshots = prepare_repository(&destination)?.join(SNAPSHOTS_DIR);
    fs::create_dir_all(&snapshots)?;
    let now = ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BackupError::InvalidSnapshot("System clock is before 1970".into()))?
        .as_secs();
    let mut suffix = 0_u32;
    let (id, stage, final_path) = loop {
        let id = if suffix == 0 {
            now.to_string()
        } else {
            format!("{now}-{suffix}")
        };
        let final_path = snapshots.join(&id);
        let stage = snapshots.join(format!(".{id}.partial-{}", std::process::id()));
        if !final_path.exists() && !stage.exists() {
            break (id, stage, final_path);
        }
        suffix = suffix.saturating_add(1);
    };
    fs::create_dir(&stage)?;
    let data = stage.join("data");
    fs::create_dir(&data)?;

    let result = build_snapshot(&source, &data, &id, now, &mut on_progress)
        .and_then(|manifest| finish_snapshot(&stage, &final_path, &manifest).map(|()| manifest));
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

fn build_snapshot(
    source: &Path,
    data: &Path,
    id: &str,
    now: u64,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<BackupManifest> {
    let mut entries = Vec::new();
    let mut progress = Progress::default();
    for item in WalkDir::new(source)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item.map_err(|e| BackupError::Io(io::Error::other(e)))?;
        let relative = item.path().strip_prefix(source).map_err(|_| {
            BackupError::InvalidSource("A source entry escaped the selected folder".into())
        })?;
        validate_relative(relative)?;
        let target = data.join(relative);
        let metadata = fs::symlink_metadata(item.path())?;
        let mode = host_fs::mode(&metadata);
        if metadata.is_dir() {
            fs::create_dir(&target)?;
            entries.push(BackupEntry {
                path: relative.to_path_buf(),
                kind: EntryKind::Directory,
                size: 0,
                sha256: None,
                link_target: None,
                mode,
            });
        } else if metadata.is_file() {
            let (size, digest) = copy_and_hash(item.path(), &target)?;
            progress.files = progress.files.saturating_add(1);
            progress.bytes = progress.bytes.saturating_add(size);
            progress.current = relative.to_path_buf();
            on_progress(&progress);
            entries.push(BackupEntry {
                path: relative.to_path_buf(),
                kind: EntryKind::File,
                size,
                sha256: Some(digest),
                link_target: None,
                mode,
            });
        } else if metadata.file_type().is_symlink() {
            let link_target = fs::read_link(item.path())?;
            create_symlink(&link_target, &target)?;
            entries.push(BackupEntry {
                path: relative.to_path_buf(),
                kind: EntryKind::Symlink,
                size: 0,
                sha256: None,
                link_target: Some(link_target),
                mode,
            });
        } else {
            return Err(BackupError::InvalidSource(format!(
                "Cannot safely back up special file {}",
                item.path().display()
            )));
        }
    }
    Ok(BackupManifest {
        version: 1,
        id: id.to_string(),
        created_unix_seconds: now,
        source_name: source.file_name().map_or_else(
            || source.display().to_string(),
            |s| s.to_string_lossy().into_owned(),
        ),
        files: progress.files,
        bytes: progress.bytes,
        entries,
    })
}

fn finish_snapshot(stage: &Path, final_path: &Path, manifest: &BackupManifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join(MANIFEST))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(stage, final_path)?;
    if let Some(parent) = final_path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Read valid snapshots from a destination, newest first.
///
/// # Errors
///
/// Returns an error when repository metadata cannot be read or a stored
/// manifest is invalid.
pub fn list_snapshots(destination: &Path) -> Result<Vec<BackupManifest>> {
    let repository = repository_path(destination);
    reject_symlink(&repository, "Backup repository")?;
    let root = repository.join(SNAPSHOTS_DIR);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut manifests = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        if !item.file_type()?.is_dir() || item.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        manifests.push(read_manifest(&item.path())?);
    }
    manifests.sort_by_key(|manifest| std::cmp::Reverse(manifest.created_unix_seconds));
    Ok(manifests)
}

/// Verify the metadata, structure, sizes, and checksums of a stored snapshot.
///
/// # Errors
///
/// Returns an error when the snapshot identifier or manifest is invalid, its
/// contents cannot be read, or any stored entry fails integrity validation.
pub fn verify_snapshot(destination: &Path, id: &str) -> Result<Verification> {
    validate_id(id)?;
    let root = repository_path(destination).join(SNAPSHOTS_DIR).join(id);
    reject_symlink(&root, "Snapshot")?;
    let manifest = read_manifest(&root)?;
    let mut result = Verification::default();
    for entry in &manifest.entries {
        validate_relative(&entry.path)?;
        ensure_no_symlink_ancestors(&root.join("data"), &entry.path)?;
        let path = root.join("data").join(&entry.path);
        match entry.kind {
            EntryKind::Directory if !path.is_dir() => {
                return integrity(&entry.path, "folder missing");
            }
            EntryKind::Symlink
                if fs::read_link(&path).ok().as_ref() != entry.link_target.as_ref() =>
            {
                return integrity(&entry.path, "link target changed");
            }
            EntryKind::File => {
                let (size, digest) = hash_file(&path)?;
                if size != entry.size || Some(digest) != entry.sha256 {
                    return integrity(&entry.path, "checksum does not match");
                }
                result.files = result.files.saturating_add(1);
                result.bytes = result.bytes.saturating_add(size);
            }
            _ => {}
        }
    }
    Ok(result)
}

/// Restore a snapshot and verify every copied file before placing it.
///
/// # Errors
///
/// Returns an error when the snapshot is invalid, the restore destination is
/// unsafe or non-empty without `overwrite`, or any file operation or integrity
/// check fails.
pub fn restore_snapshot(
    destination: &Path,
    id: &str,
    restore_to: &Path,
    overwrite: bool,
    mut on_progress: impl FnMut(&Progress),
) -> Result<Verification> {
    validate_id(id)?;
    let root = repository_path(destination).join(SNAPSHOTS_DIR).join(id);
    reject_symlink(&root, "Snapshot")?;
    let manifest = read_manifest(&root)?;
    let repo = repository_path(destination).canonicalize()?;
    let restore_to = prepare_restore_target(restore_to, &repo, overwrite)?;
    let stage = create_restore_stage(&restore_to)?;
    let result = restore_into_stage(&root, &manifest, &stage, &mut on_progress);
    match result {
        Ok(verification) => match publish_restored_snapshot(&stage, &restore_to, overwrite) {
            Ok(()) => Ok(verification),
            Err(error) => {
                let _ = fs::remove_dir_all(&stage);
                Err(error)
            }
        },
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            Err(error)
        }
    }
}

/// Resolve a final restore target without creating it.  A failed verification
/// must not leave even a partially populated recovery directory behind.
fn prepare_restore_target(restore_to: &Path, repo: &Path, overwrite: bool) -> Result<PathBuf> {
    let parent = restore_to.parent().ok_or_else(|| {
        BackupError::UnsafeDestination("Restore destination has no parent folder".into())
    })?;
    let name = restore_to.file_name().ok_or_else(|| {
        BackupError::UnsafeDestination("Restore destination must name a folder".into())
    })?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let target = parent.join(name);
    if target.starts_with(repo) || repo.starts_with(&target) {
        return Err(BackupError::UnsafeDestination(
            "Restore outside the backup repository".into(),
        ));
    }
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BackupError::UnsafeDestination(
            "Restore destination must not be a symbolic link".into(),
        )),
        Ok(metadata) if !metadata.is_dir() => Err(BackupError::UnsafeDestination(
            "Restore destination is not a folder".into(),
        )),
        Ok(_) if !overwrite && fs::read_dir(&target)?.next().is_some() => {
            Err(BackupError::UnsafeDestination(
                "Choose an empty restore folder, or explicitly allow replacement".into(),
            ))
        }
        Ok(_) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
        Err(error) => Err(error.into()),
    }
}

fn create_restore_stage(target: &Path) -> Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        BackupError::UnsafeDestination("Restore destination has no parent folder".into())
    })?;
    for suffix in 0_u32..1024 {
        let stage = parent.join(format!(
            ".tilo-restore-{}-{suffix}.partial",
            std::process::id()
        ));
        match fs::create_dir(&stage) {
            Ok(()) => return Ok(stage),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(BackupError::UnsafeDestination(
        "Could not reserve a private restore staging folder".into(),
    ))
}

fn restore_into_stage(
    root: &Path,
    manifest: &BackupManifest,
    stage: &Path,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<Verification> {
    let mut progress = Progress::default();
    for entry in &manifest.entries {
        validate_relative(&entry.path)?;
        ensure_no_symlink_ancestors(stage, &entry.path)?;
        ensure_no_symlink_ancestors(&root.join("data"), &entry.path)?;
        let from = root.join("data").join(&entry.path);
        let to = stage.join(&entry.path);
        match entry.kind {
            EntryKind::Directory => {
                if let Ok(metadata) = fs::symlink_metadata(&to)
                    && !metadata.is_dir()
                {
                    return integrity(&entry.path, "restore target is not a folder");
                }
                fs::create_dir_all(&to)?;
            }
            EntryKind::Symlink => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                let link = entry
                    .link_target
                    .as_ref()
                    .ok_or_else(|| BackupError::InvalidSnapshot("Link has no target".into()))?;
                create_symlink(link, &to)?;
            }
            EntryKind::File => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                let temporary = to.with_extension(format!("tilo-restore-{}", std::process::id()));
                let (size, digest) = copy_and_hash(&from, &temporary)?;
                if size != entry.size || Some(digest) != entry.sha256 {
                    let _ = fs::remove_file(&temporary);
                    return integrity(&entry.path, "stored file failed verification");
                }
                fs::rename(&temporary, &to)?;
                progress.files = progress.files.saturating_add(1);
                progress.bytes = progress.bytes.saturating_add(size);
                progress.current.clone_from(&entry.path);
                on_progress(&progress);
            }
        }
        if !matches!(entry.kind, EntryKind::Symlink)
            && let Some(mode) = entry.mode
        {
            host_fs::set_mode(&to, mode)?;
        }
    }
    Ok(Verification {
        files: progress.files,
        bytes: progress.bytes,
    })
}

fn publish_restored_snapshot(stage: &Path, target: &Path, overwrite: bool) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BackupError::UnsafeDestination(
            "Restore destination changed into a symbolic link".into(),
        )),
        Ok(metadata) if !metadata.is_dir() => Err(BackupError::UnsafeDestination(
            "Restore destination is no longer a folder".into(),
        )),
        Ok(_) if !overwrite && fs::read_dir(target)?.next().is_some() => {
            Err(BackupError::UnsafeDestination(
                "Restore destination changed while recovery was running".into(),
            ))
        }
        Ok(_) => publish_restore_swap(stage, target, overwrite, &mut |from, to| {
            fs::rename(from, to)
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::rename(stage, target)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Replace an existing target only after the staged recovery point is ready.
/// If publishing the stage fails, put the original target back before
/// returning the error.  This matters for explicit `overwrite` callers, but
/// also makes the empty-folder UI promise robust against a full disk or a
/// concurrent rename at exactly the wrong moment.
fn publish_restore_swap(
    stage: &Path,
    target: &Path,
    overwrite: bool,
    rename: &mut impl FnMut(&Path, &Path) -> io::Result<()>,
) -> Result<()> {
    let previous = reserve_previous_restore_path(target)?;
    rename(target, &previous)?;
    if let Err(publish_error) = rename(stage, target) {
        return match rename(&previous, target) {
            Ok(()) => Err(publish_error.into()),
            Err(rollback_error) => Err(BackupError::UnsafeDestination(format!(
                "Restore publish failed ({publish_error}); could not restore the previous target ({rollback_error})"
            ))),
        };
    }
    // The new tree is now published. A failed cleanup leaves a sibling the
    // user can still recover from; it must not turn a completed restore into
    // a misleading failure.
    let _ = if overwrite {
        fs::remove_dir_all(previous)
    } else {
        fs::remove_dir(previous)
    };
    Ok(())
}

fn reserve_previous_restore_path(target: &Path) -> Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        BackupError::UnsafeDestination("Restore destination has no parent folder".into())
    })?;
    for suffix in 0_u32..1024 {
        let previous = parent.join(format!(
            ".tilo-restore-{}-{suffix}.previous",
            std::process::id()
        ));
        match fs::symlink_metadata(&previous) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(previous),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(BackupError::UnsafeDestination(
        "Could not reserve a rollback location for the restore target".into(),
    ))
}

fn read_manifest(snapshot: &Path) -> Result<BackupManifest> {
    let bytes = fs::read(snapshot.join(MANIFEST))?;
    let manifest: BackupManifest = serde_json::from_slice(&bytes)?;
    if manifest.version != 1 {
        return Err(BackupError::InvalidSnapshot(format!(
            "Unsupported backup format {}",
            manifest.version
        )));
    }
    validate_id(&manifest.id)?;
    if snapshot.file_name().and_then(|v| v.to_str()) != Some(&manifest.id) {
        return Err(BackupError::InvalidSnapshot(
            "Snapshot identity does not match its folder".into(),
        ));
    }
    Ok(manifest)
}

fn prepare_repository(destination: &Path) -> Result<PathBuf> {
    let repository = repository_path(destination);
    reject_symlink(&repository, "Backup repository")?;
    fs::create_dir_all(&repository)?;
    let resolved = repository.canonicalize()?;
    if !resolved.starts_with(destination) {
        return Err(BackupError::UnsafeDestination(
            "The backup repository redirects outside the selected destination".into(),
        ));
    }
    Ok(resolved)
}

fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BackupError::UnsafeDestination(
            format!("{label} must not be a symbolic link"),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_digit() || c == b'-') {
        return Err(BackupError::InvalidSnapshot(
            "Invalid snapshot identifier".into(),
        ));
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BackupError::InvalidSnapshot(format!(
            "Unsafe path in backup: {}",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_no_symlink_ancestors(root: &Path, relative: &Path) -> Result<()> {
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return integrity(relative, "unsafe parent path");
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return integrity(relative, "a parent folder is a symbolic link");
            }
            Ok(metadata) if !metadata.is_dir() => {
                return integrity(relative, "a parent path is not a folder");
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn copy_and_hash(from: &Path, to: &Path) -> Result<(u64, String)> {
    let mut input = File::open(from)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(to)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buf = vec![0_u8; 128 * 1024].into_boxed_slice();
    loop {
        let read = input.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let Some(chunk) = buf.get(..read) else {
            return Err(BackupError::Io(io::Error::other(
                "reader returned more bytes than the buffer can hold",
            )));
        };
        output.write_all(chunk)?;
        hasher.update(chunk);
        total = total.saturating_add(read as u64);
    }
    output.sync_all()?;
    Ok((total, hex::encode(hasher.finalize())))
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buf = vec![0_u8; 128 * 1024].into_boxed_slice();
    loop {
        let read = input.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let Some(chunk) = buf.get(..read) else {
            return Err(BackupError::Io(io::Error::other(
                "reader returned more bytes than the buffer can hold",
            )));
        };
        hasher.update(chunk);
        total = total.saturating_add(read as u64);
    }
    Ok((total, hex::encode(hasher.finalize())))
}

fn integrity<T>(path: &Path, message: &str) -> Result<T> {
    Err(BackupError::Integrity(format!(
        "{}: {message}",
        path.display()
    )))
}

fn create_symlink(target: &Path, path: &Path) -> Result<()> {
    if !host_fs::HAS_SYMLINKS {
        return Err(BackupError::InvalidSource(
            "Symbolic links are not supported on this platform".into(),
        ));
    }
    host_fs::symlink(target, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries_require_the_mode_key() {
        let entry = r#"{"path":"a","kind":"file","size":0,"sha256":null,"link_target":null"#;
        assert!(serde_json::from_str::<BackupEntry>(&format!("{entry}}}")).is_err());
        let parsed: BackupEntry =
            serde_json::from_str(&format!("{entry},\"mode\":null}}")).unwrap();
        assert_eq!(parsed.mode, None);
    }

    #[test]
    fn create_verify_and_restore_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("notes")).unwrap();
        fs::write(source.join("hello.txt"), b"hello backup").unwrap();
        fs::write(source.join("notes/today.txt"), b"ship it").unwrap();
        let mut updates = Vec::new();
        let manifest = create_snapshot(&source, &destination, |p| updates.push(p.clone())).unwrap();
        assert_eq!(manifest.files, 2);
        assert_eq!(updates.last().unwrap().files, 2);
        assert_eq!(
            list_snapshots(&destination).unwrap(),
            vec![manifest.clone()]
        );
        assert_eq!(
            verify_snapshot(&destination, &manifest.id).unwrap().files,
            2
        );

        let restored = temp.path().join("restored");
        let result =
            restore_snapshot(&destination, &manifest.id, &restored, false, |_| {}).unwrap();
        assert_eq!(result.files, 2);
        assert_eq!(
            fs::read(restored.join("hello.txt")).unwrap(),
            b"hello backup"
        );
        assert_eq!(
            fs::read(restored.join("notes/today.txt")).unwrap(),
            b"ship it"
        );
    }

    /// Recursive destinations, corrupt snapshots, targets inside the repository,
    /// non-empty folders and a failed stage publish are all refused without
    /// clobbering data; each case uses its own temp dir.
    #[test]
    fn unsafe_or_corrupt_operations_are_refused_without_writing() {
        rejects_recursive_destinations();
        corruption_is_reported_and_not_restored();
        restore_refuses_a_target_inside_the_repository_before_writing_it();
        restore_refuses_nonempty_folder_by_default();
        failed_stage_publish_restores_the_previous_target();
    }

    /// Case of `unsafe_or_corrupt_operations_are_refused_without_writing`.
    fn rejects_recursive_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        assert!(matches!(
            validate_paths(&source, &source.join("backup")),
            Err(BackupError::UnsafeDestination(_))
        ));
    }

    /// Case of `unsafe_or_corrupt_operations_are_refused_without_writing`.
    fn corruption_is_reported_and_not_restored() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("important"), b"original").unwrap();
        let manifest = create_snapshot(&source, &destination, |_| {}).unwrap();
        let stored = repository_path(&destination)
            .join(SNAPSHOTS_DIR)
            .join(&manifest.id)
            .join("data/important");
        fs::write(stored, b"tampered").unwrap();
        assert!(matches!(
            verify_snapshot(&destination, &manifest.id),
            Err(BackupError::Integrity(_))
        ));
        let restored = temp.path().join("restored");
        assert!(restore_snapshot(&destination, &manifest.id, &restored, false, |_| {}).is_err());
        assert!(!restored.join("important").exists());
        assert!(
            !restored.exists(),
            "failed verification never publishes a partial recovery folder"
        );
    }

    /// Case of `unsafe_or_corrupt_operations_are_refused_without_writing`.
    fn restore_refuses_a_target_inside_the_repository_before_writing_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("important"), b"original").unwrap();
        let manifest = create_snapshot(&source, &destination, |_| {}).unwrap();
        let inside_repo = repository_path(&destination).join("recovery");

        assert!(matches!(
            restore_snapshot(&destination, &manifest.id, &inside_repo, false, |_| {}),
            Err(BackupError::UnsafeDestination(_))
        ));
        assert!(
            !inside_repo.exists(),
            "a refused target must not leave a directory in the repository"
        );
    }

    /// Case of `unsafe_or_corrupt_operations_are_refused_without_writing`.
    fn restore_refuses_nonempty_folder_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let restored = temp.path().join("restored");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&restored).unwrap();
        fs::write(source.join("file"), b"new").unwrap();
        fs::write(restored.join("file"), b"existing").unwrap();
        let manifest = create_snapshot(&source, &destination, |_| {}).unwrap();
        assert!(matches!(
            restore_snapshot(&destination, &manifest.id, &restored, false, |_| {}),
            Err(BackupError::UnsafeDestination(_))
        ));
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"existing");
    }

    /// Case of `unsafe_or_corrupt_operations_are_refused_without_writing`.
    fn failed_stage_publish_restores_the_previous_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("restore");
        let stage = temp.path().join("stage");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(target.join("previous"), b"keep me").unwrap();
        fs::write(stage.join("restored"), b"new data").unwrap();
        let mut calls = 0_u8;
        let result = publish_restore_swap(&stage, &target, true, &mut |from, to| {
            calls = calls.saturating_add(1);
            if calls == 2 {
                return Err(io::Error::other("injected stage publish failure"));
            }
            fs::rename(from, to)
        });

        assert!(result.is_err());
        assert_eq!(fs::read(target.join("previous")).unwrap(), b"keep me");
        assert!(stage.join("restored").exists());
    }

    /// Source symlinks are stored as links, a malicious restore ancestor link is
    /// replaced, and a repository symlink is rejected; each case uses its own
    /// temp dir.
    #[test]
    #[cfg(unix)]
    fn symlinks_are_preserved_or_rejected_but_never_followed() {
        source_symlinks_are_preserved_without_following_them();
        overwrite_replaces_a_malicious_symlink_ancestor_without_following_it();
        repository_symlink_is_rejected();
    }

    /// Case of `symlinks_are_preserved_or_rejected_but_never_followed`.
    #[cfg(unix)]
    fn source_symlinks_are_preserved_without_following_them() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"must not be copied").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("outside-link")).unwrap();
        let manifest = create_snapshot(&source, &destination, |_| {}).unwrap();
        assert_eq!(manifest.files, 0, "the link target was not traversed");
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.path == Path::new("outside-link"))
            .unwrap();
        assert_eq!(entry.kind, EntryKind::Symlink);
        let restored = temp.path().join("restored");
        restore_snapshot(&destination, &manifest.id, &restored, false, |_| {}).unwrap();
        assert_eq!(
            fs::read_link(restored.join("outside-link")).unwrap(),
            outside
        );
    }

    /// Case of `symlinks_are_preserved_or_rejected_but_never_followed`.
    #[cfg(unix)]
    fn overwrite_replaces_a_malicious_symlink_ancestor_without_following_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let restore = temp.path().join("restore");
        let outside = temp.path().join("outside");
        fs::create_dir_all(source.join("folder")).unwrap();
        fs::write(source.join("folder/file"), b"safe").unwrap();
        fs::create_dir_all(&restore).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let manifest = create_snapshot(&source, &destination, |_| {}).unwrap();
        std::os::unix::fs::symlink(&outside, restore.join("folder")).unwrap();
        restore_snapshot(&destination, &manifest.id, &restore, true, |_| {}).unwrap();
        assert!(!outside.join("file").exists());
        assert_eq!(fs::read(restore.join("folder/file")).unwrap(), b"safe");
    }

    /// Case of `symlinks_are_preserved_or_rejected_but_never_followed`.
    #[cfg(unix)]
    fn repository_symlink_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, repository_path(&destination)).unwrap();
        assert!(matches!(
            create_snapshot(&source, &destination, |_| {}),
            Err(BackupError::UnsafeDestination(_))
        ));
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }
}
