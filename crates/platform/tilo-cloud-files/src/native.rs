//! Independent imports: provider IDs become new local copies, never sync mounts.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{
    DestinationIdentity, ImportConsent, MoveInto, TransferOutcome as Outcome,
    TransferOutcome as Settled, TransferProgress, TransferReceipt as Change, valid_name,
};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use runtime_fs::cloud::{
    CloudClient, IMPORT_CHUNK_LIMIT, ImportEntry, ImportKind, ImportReadState, ImportReply,
    ImportRequest,
};

fn identity(dir: &Dir) -> std::io::Result<DestinationIdentity> {
    use cap_std::fs::MetadataExt as _;
    let metadata = dir.dir_metadata()?;
    Ok(DestinationIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified: None,
    })
}

fn output_identity(metadata: &cap_std::fs::Metadata) -> DestinationIdentity {
    use cap_std::fs::MetadataExt as _;
    DestinationIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified: Some((metadata.mtime(), metadata.mtime_nsec(), metadata.len())),
    }
}

fn object_identity(dir: &Dir, name: &Path) -> Result<DestinationIdentity, String> {
    let metadata = dir
        .symlink_metadata(name)
        .map_err(|error| error.to_string())?;
    Ok(output_identity(&metadata))
}

/// Take back only the admitted output, through directory descriptors. A
/// same-filesystem rename captures the object before its identity is checked
/// again; a concurrent replacement is restored, never sent to Trash.
///
/// # Errors
/// Refuses changed destinations or outputs, or failed Trash publication.
#[cfg(target_os = "linux")]
pub fn compensate(
    to: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
    move_into: MoveInto,
) -> Result<Change, String> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    let path = Path::new(to);
    let folder = path.parent().ok_or("import output has no parent")?;
    let name = path.file_name().ok_or("import output has no name")?;
    let root = anchored_folder(folder).map_err(|error| error.to_string())?;
    if identity(&root).map_err(|error| error.to_string())? != *destination
        || object_identity(&root, Path::new(name))? != *output
    {
        return Err("the import destination or output changed; refusing to undo".into());
    }
    let trash_path = host_fs::trash::ensure_trash_dir().map_err(|error| error.to_string())?;
    let trash = anchored_folder(&trash_path).map_err(|error| error.to_string())?;
    let info = anchored_folder(
        &trash_path
            .parent()
            .ok_or("Trash has no parent")?
            .join("info"),
    )
    .map_err(|error| error.to_string())?;
    let receipt = format!("cloud-import-{}", uuid::Uuid::new_v4());
    let info_name = format!("{receipt}.trashinfo");
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut metadata = info
        .open_with(&info_name, &options)
        .map_err(|error| error.to_string())?;
    let mut encoded = String::new();
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write as _;
        write!(encoded, "%{byte:02X}").map_err(|error| error.to_string())?;
    }
    let description = format!(
        "[Trash Info]\nPath={encoded}\nDeletionDate={}\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    );
    if let Err(error) = metadata
        .write_all(description.as_bytes())
        .and_then(|()| metadata.sync_all())
    {
        let _ = info.remove_file(&info_name);
        return Err(error.to_string());
    }
    let source = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd())).join(name);
    let captured = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd())).join(&receipt);
    if let Err(error) = host_fs::rename_noreplace(&source, &captured) {
        let _ = info.remove_file(&info_name);
        return Err(format!("cannot safely move import to Trash: {error}"));
    }
    if object_identity(&root, Path::new(&receipt))? != *output {
        let restored = host_fs::rename_noreplace(&captured, &source);
        let _ = info.remove_file(&info_name);
        return Err(format!(
            "the imported output changed during undo; refused replacement (restore: {restored:?})"
        ));
    }
    let target_dir = PathBuf::from(format!("/proc/self/fd/{}", trash.as_raw_fd()));
    let moved = match move_into(&captured, &target_dir) {
        Ok(moved) => moved,
        Err(error) => {
            let restored = host_fs::rename_noreplace(&captured, &source);
            let _ = info.remove_file(&info_name);
            return Err(format!(
                "cannot move import to Trash: {error} (restore: {restored:?})"
            ));
        }
    };
    let moved_name = moved.file_name().ok_or("trashed import has no name")?;
    if moved_name != std::ffi::OsStr::new(&receipt) {
        let mut actual = info
            .open_with(
                format!("{}.trashinfo", moved_name.to_string_lossy()),
                &options,
            )
            .map_err(|error| {
                format!(
                    "import moved to {} but Trash metadata failed: {error}",
                    trash_path.join(moved_name).display()
                )
            })?;
        actual
            .write_all(description.as_bytes())
            .and_then(|()| actual.sync_all())
            .map_err(|error| error.to_string())?;
        info.remove_file(&info_name)
            .map_err(|error| error.to_string())?;
    }
    let output = object_identity(&trash, Path::new(moved_name))?;
    Ok(Change::CloudTrashed {
        to: to.into(),
        entry: trash_path.join(moved_name).display().to_string(),
        destination: destination.clone(),
        output,
    })
}

/// Restore only the admitted Trash object to its unchanged destination.
///
/// # Errors
/// Refuses identity changes, collisions, unsupported platforms and I/O failures.
pub fn restore(
    to: &str,
    entry: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
    move_into: MoveInto,
) -> Result<Change, String> {
    let path = Path::new(to);
    let root = anchored_folder(path.parent().ok_or("import output has no parent")?)
        .map_err(|error| error.to_string())?;
    if identity(&root).map_err(|error| error.to_string())? != *destination {
        return Err("the import destination changed; refusing to restore".into());
    }
    let trashed = Path::new(entry);
    let trash = anchored_folder(trashed.parent().ok_or("Trash entry has no parent")?)
        .map_err(|error| error.to_string())?;
    if object_identity(
        &trash,
        Path::new(trashed.file_name().ok_or("Trash entry has no name")?),
    )? != *output
        || root
            .symlink_metadata(path.file_name().ok_or("import output has no name")?)
            .is_ok()
    {
        return Err("the import output or Trash entry changed; refusing to restore".into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        let source = PathBuf::from(format!("/proc/self/fd/{}", trash.as_raw_fd()))
            .join(trashed.file_name().ok_or("Trash entry has no name")?);
        let target = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()))
            .join(path.file_name().ok_or("import output has no name")?);
        let trash_path = PathBuf::from(format!("/proc/self/fd/{}", trash.as_raw_fd()));
        let captured_name = format!(".cloud-restore-{}", uuid::Uuid::new_v4());
        let captured_trash = trash_path.join(&captured_name);
        host_fs::rename_noreplace(&source, &captured_trash).map_err(|error| error.to_string())?;
        if object_identity(&trash, Path::new(&captured_name))? != *output {
            let rollback = host_fs::rename_noreplace(&captured_trash, &source);
            return Err(format!(
                "Trash entry changed during restore (rollback: {rollback:?})"
            ));
        }
        let root_path = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()));
        let captured = root_path.join(format!(".cloud-restore-{}", uuid::Uuid::new_v4()));
        if let Err(error) = host_fs::create_private_dir(&captured) {
            let rollback = host_fs::rename_noreplace(&captured_trash, &source);
            return Err(format!(
                "cannot stage restore: {error} (rollback: {rollback:?})"
            ));
        }
        let restored = match move_into(&captured_trash, &captured) {
            Ok(restored) => restored,
            Err(error) => {
                let rollback = host_fs::rename_noreplace(&captured_trash, &source);
                let _ = root.remove_dir(captured.file_name().ok_or("restore staging has no name")?);
                return Err(format!(
                    "cannot stage restore: {error} (rollback: {rollback:?})"
                ));
            }
        };
        if let Err(error) = host_fs::rename_noreplace(&restored, &target) {
            let rollback = move_into(&restored, &trash_path).and_then(|back| {
                host_fs::rename_noreplace(&back, &source).map_err(|error| error.to_string())
            });
            let _ = root.remove_dir(captured.file_name().ok_or("restore staging has no name")?);
            return Err(format!(
                "cannot restore imported output: {error} (rollback: {rollback:?})"
            ));
        }
        root.remove_dir(captured.file_name().ok_or("restore staging has no name")?)
            .map_err(|error| error.to_string())?;
        let info = anchored_folder(
            &trashed
                .parent()
                .and_then(Path::parent)
                .ok_or("Trash has no parent")?
                .join("info"),
        )
        .map_err(|error| error.to_string())?;
        info.remove_file(format!(
            "{}.trashinfo",
            trashed
                .file_name()
                .ok_or("Trash entry has no name")?
                .to_string_lossy()
        ))
        .map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "linux"))]
    return Err("safe cloud import restore is unavailable on this platform".into());
    let output = object_identity(
        &root,
        Path::new(path.file_name().ok_or("import output has no name")?),
    )?;
    Ok(Change::CloudImported {
        from: String::new(),
        to: to.into(),
        destination: destination.clone(),
        output,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn compensate(
    _: &str,
    _: &DestinationIdentity,
    _: &DestinationIdentity,
    _: MoveInto,
) -> Result<Change, String> {
    Err("safe cloud import undo is unavailable on this platform".into())
}

/// Admit only an existing, symlink-free local directory outside sync mounts.
///
/// # Errors
/// Refuses symlinks, sync-mount aliases, missing directories and I/O failures.
pub fn pin_destination<'a>(
    folder: &str,
    mounts: impl Iterator<Item = &'a Path>,
) -> Result<DestinationIdentity, String> {
    let dir = independent_folder(Path::new(folder), mounts)?;
    identity(&dir).map_err(|error| error.to_string())
}

fn independent_folder<'a>(
    folder: &Path,
    mounts: impl Iterator<Item = &'a Path>,
) -> Result<Dir, String> {
    let dir = anchored_folder(folder).map_err(|error| error.to_string())?;
    let destination = std::fs::canonicalize(folder).map_err(|error| error.to_string())?;
    for mount in mounts {
        let canonical = std::fs::canonicalize(mount).unwrap_or_else(|_| mount.to_path_buf());
        if folder.starts_with(mount) || destination.starts_with(&canonical) {
            return Err("import into a local folder outside synced accounts".into());
        }
        // Also reject aliases (such as bind mounts) whose paths differ but
        // whose directory identity is an ancestor of the destination.
        if let Ok(mounted) = anchored_folder(&canonical) {
            let mounted = identity(&mounted).map_err(|error| error.to_string())?;
            for ancestor in destination.ancestors() {
                let ancestor = anchored_folder(ancestor).map_err(|error| error.to_string())?;
                if identity(&ancestor).map_err(|error| error.to_string())? == mounted {
                    return Err("import into a local folder outside synced accounts".into());
                }
            }
        }
    }
    Ok(dir)
}

/// Execute only the exact payload admitted by the host; consumes its permit.
pub fn execute(
    consent: ImportConsent,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
    mounts: &[PathBuf],
) -> Outcome {
    let Some(socket) = runtime_fs::cloud::default_files_socket() else {
        return Settled::Failed {
            reason: "cloud service is unavailable: no runtime directory".into(),
            partial: Vec::new(),
        };
    };
    execute_at(&socket, consent, cancel, progress, mounts)
}

/// Execute an admitted import against a chosen host service socket.
pub fn execute_at(
    socket: &Path,
    consent: ImportConsent,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
    mounts: &[PathBuf],
) -> Outcome {
    // No client is opened and no provider request is made without consuming
    // the host-issued permit. Source and destination come only from that grant.
    let payload = consent.into_payload();
    let account = payload.account.as_str();
    let entries = payload.entries.as_slice();
    let folder = payload.folder.as_str();
    let destination = &payload.destination;
    let mut changes = Vec::new();
    let mut status = TransferProgress {
        phase: "preparing".into(),
        ..Default::default()
    };
    progress(status.clone());
    let result = (|| {
        cancelled(cancel)?;
        let root = independent_folder(Path::new(folder), mounts.iter().map(PathBuf::as_path))?;
        if identity(&root).map_err(|error| error.to_string())? != *destination {
            return Err("the import destination changed since admission".into());
        }
        let mut client = CloudClient::connect(socket).map_err(|error| error.to_string())?;
        let mut worker = Worker {
            destination_path: Path::new(folder),
            destination,
            account,
            cancel,
            progress: &progress,
            status: &mut status,
            active: HashSet::new(),
        };
        for entry in entries {
            match worker.entry(&mut client, &root, entry) {
                Ok((name, output)) => {
                    changes.push(Change::CloudImported {
                        from: format!("cloud:{account}:{}", entry.id),
                        to: Path::new(folder).join(name).display().to_string(),
                        destination: destination.clone(),
                        output,
                    });
                }
                Err(error) => {
                    worker
                        .status
                        .failed
                        .push(format!("{}: {error}", entry.name));
                    (worker.progress)(worker.status.clone());
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                }
            }
        }
        if worker.status.failed.is_empty() {
            Ok(())
        } else {
            Err(worker.status.failed.join("; "))
        }
    })();
    status.phase = if result.is_ok() {
        "done"
    } else if cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else {
        "failed"
    }
    .into();
    progress(status);
    match result {
        Ok(()) => Settled::Done(changes),
        Err(reason) => Settled::Failed {
            reason,
            partial: changes,
        },
    }
}

// Reject symlinks in every destination component, not only the final leaf.
fn anchored_folder(path: &Path) -> std::io::Result<Dir> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => dir = dir.open_dir_nofollow(name)?,
            _ => {
                return Err(std::io::Error::other(
                    "destination must not contain parent traversal",
                ));
            }
        }
    }
    Ok(dir)
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cloud import cancelled".into())
    } else {
        Ok(())
    }
}

fn request(
    client: &mut CloudClient,
    account: &str,
    request: ImportRequest,
) -> Result<ImportReply, String> {
    match client
        .import(account, request)
        .map_err(|error| error.to_string())?
    {
        ImportReply::Error(error) => Err(error.message),
        reply => Ok(reply),
    }
}

// Never recursively remove a staging path. Every removable object has a
// descriptor-derived receipt; unexpected children make the whole stage guarded.
struct StagedObject {
    parent: Dir,
    name: PathBuf,
    expected: DestinationIdentity,
    directory: Option<Dir>,
}

struct ImportStage {
    name: PathBuf,
    dir: Dir,
    expected: DestinationIdentity,
    objects: Vec<StagedObject>,
}

fn same_object(left: &DestinationIdentity, right: &DestinationIdentity) -> bool {
    left.device == right.device && left.inode == right.inode
}

#[cfg(target_os = "linux")]
fn rename_at(from: &Dir, source: &Path, to: &Dir, target: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    host_fs::rename_noreplace(
        &PathBuf::from(format!("/proc/self/fd/{}", from.as_raw_fd())).join(source),
        &PathBuf::from(format!("/proc/self/fd/{}", to.as_raw_fd())).join(target),
    )
}

#[cfg(not(target_os = "linux"))]
fn rename_at(_: &Dir, _: &Path, _: &Dir, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "safe cloud import publication is unavailable",
    ))
}

impl ImportStage {
    fn new(parent: &Dir) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            let name = PathBuf::from(format!(".cloud-import-{}", uuid::Uuid::new_v4()));
            let path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd())).join(&name);
            host_fs::create_private_dir(&path).map_err(|error| error.to_string())?;
            let dir = parent
                .open_dir_nofollow(&name)
                .map_err(|error| error.to_string())?;
            let expected = identity(&dir).map_err(|error| error.to_string())?;
            Ok(Self {
                name,
                dir,
                expected,
                objects: Vec::new(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        Err("safe cloud import staging is unavailable".into())
    }

    fn validate(&self, parent: &Dir) -> Result<(), String> {
        if !same_object(&object_identity(parent, &self.name)?, &self.expected) {
            return Err("private import staging was replaced".into());
        }
        for object in &self.objects {
            let actual = object_identity(&object.parent, &object.name)?;
            let matches = if object.directory.is_some() {
                same_object(&actual, &object.expected)
            } else {
                actual == object.expected
            };
            if !matches {
                return Err(format!("staged object changed: {}", object.name.display()));
            }
        }
        self.validate_children(&self.dir)?;
        for object in &self.objects {
            if let Some(directory) = &object.directory {
                self.validate_children(directory)?;
            }
        }
        Ok(())
    }

    fn validate_children(&self, dir: &Dir) -> Result<(), String> {
        let directory = identity(dir).map_err(|error| error.to_string())?;
        for child in dir.entries().map_err(|error| error.to_string())? {
            let name = child.map_err(|error| error.to_string())?.file_name();
            let known = self.objects.iter().any(|object| {
                object.name == Path::new(&name)
                    && identity(&object.parent).is_ok_and(|parent| parent == directory)
            });
            if !known {
                return Err(format!(
                    "unexpected object in private staging: {}",
                    name.to_string_lossy()
                ));
            }
        }
        Ok(())
    }

    fn cleanup(&mut self, parent: &Dir) -> Result<(), String> {
        let result = (|| {
            self.validate(parent)?;
            // Capture each known leaf before checking its receipt again. A
            // replacement is restored, never unlinked. Directories are removed
            // only when empty, so concurrently added user files always survive.
            for object in self.objects.iter().rev() {
                let captured = PathBuf::from(format!(".cleanup-{}", uuid::Uuid::new_v4()));
                rename_at(&object.parent, &object.name, &object.parent, &captured)
                    .map_err(|error| error.to_string())?;
                let actual = object_identity(&object.parent, &captured)?;
                let matches = if object.directory.is_some() {
                    same_object(&actual, &object.expected)
                } else {
                    actual == object.expected
                };
                if !matches {
                    let restored =
                        rename_at(&object.parent, &captured, &object.parent, &object.name);
                    return Err(format!(
                        "staged object replaced; cleanup refused (restore: {restored:?})"
                    ));
                }
                let removed = if object.directory.is_some() {
                    object.parent.remove_dir(&captured)
                } else {
                    object.parent.remove_file(&captured)
                };
                if let Err(error) = removed {
                    let restored =
                        rename_at(&object.parent, &captured, &object.parent, &object.name);
                    return Err(format!("{error} (restore: {restored:?})"));
                }
            }
            // Capture the empty container too: never remove a replacement at
            // its old name, even when it happens to be empty.
            let captured = PathBuf::from(format!(".cloud-import-guard-{}", uuid::Uuid::new_v4()));
            rename_at(parent, &self.name, parent, &captured).map_err(|error| error.to_string())?;
            if !same_object(&object_identity(parent, &captured)?, &self.expected) {
                let restored = rename_at(parent, &captured, parent, &self.name);
                return Err(format!("staging replaced (restore: {restored:?})"));
            }
            self.name = captured;
            parent
                .remove_dir(&self.name)
                .map_err(|error| error.to_string())
        })();
        result.map_err(|error| {
            format!(
                "private staging retained at {}: {error}",
                self.name.display()
            )
        })
    }
}

struct Worker<'a, F> {
    destination_path: &'a Path,
    destination: &'a DestinationIdentity,
    account: &'a str,
    cancel: &'a AtomicBool,
    progress: &'a F,
    status: &'a mut TransferProgress,
    active: HashSet<String>,
}

// Keep associations on imported files, but dots in folder names are ordinary
// characters. Reserve suffix bytes before trimming so even a full-size source
// name can be published without weakening no-replace collision handling.
fn collision_name(entry: &ImportEntry, index: u32) -> PathBuf {
    if index == 0 {
        return PathBuf::from(&entry.name);
    }
    let suffix = format!(" ({index})");
    let (stem, extension) = if entry.kind == ImportKind::Folder {
        (entry.name.as_str(), String::new())
    } else {
        entry
            .name
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or_else(
                || (entry.name.as_str(), String::new()),
                |(stem, extension)| (stem, format!(".{extension}")),
            )
    };
    let available = 255_usize.saturating_sub(suffix.len());
    let extension = utf8_prefix(&extension, available);
    let stem = utf8_prefix(stem, available.saturating_sub(extension.len()));
    PathBuf::from(format!("{stem}{suffix}{extension}"))
}

fn utf8_prefix(value: &str, limit: usize) -> &str {
    value
        .get(..value.floor_char_boundary(limit))
        .unwrap_or_default()
}

impl<F: Fn(TransferProgress)> Worker<'_, F> {
    fn entry(
        &mut self,
        client: &mut CloudClient,
        parent: &Dir,
        entry: &ImportEntry,
    ) -> Result<(PathBuf, DestinationIdentity), String> {
        cancelled(self.cancel)?;
        valid_name(&entry.name)?;
        if entry.kind == ImportKind::Unsupported {
            return Err("cloud entry cannot be exported".into());
        }
        self.create_entry(client, parent, entry)
    }

    fn create_entry(
        &mut self,
        client: &mut CloudClient,
        parent: &Dir,
        entry: &ImportEntry,
    ) -> Result<(PathBuf, DestinationIdentity), String> {
        let mut stage = ImportStage::new(parent)?;
        let result = self.copy_entry(client, &stage.dir, entry, &mut stage.objects);
        let result = result.and_then(|name| {
            cancelled(self.cancel)?;
            let current =
                anchored_folder(self.destination_path).map_err(|error| error.to_string())?;
            if identity(&current).map_err(|error| error.to_string())? != *self.destination {
                return Err("the import destination changed during transfer".into());
            }
            stage.validate(parent)?;
            let output = object_identity(&stage.dir, &name)?;
            for index in 0..10_000 {
                let target = collision_name(entry, index);
                match rename_at(&stage.dir, &name, parent, &target) {
                    Ok(()) => return Ok((target, output)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err("too many local name collisions".into())
        });
        match result {
            Ok(receipt) => {
                // The payload has moved out; only the empty private container remains.
                stage.objects.clear();
                if let Err(error) = stage.cleanup(parent) {
                    // Keep the committed receipt even if a third party populated the
                    // now-empty container. Nothing in it is ours to delete.
                    self.status.failed.push(error);
                }
                Ok(receipt)
            }
            Err(error) => match stage.cleanup(parent) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; {cleanup}")),
            },
        }
    }

    fn copy_entry(
        &mut self,
        client: &mut CloudClient,
        parent: &Dir,
        entry: &ImportEntry,
        objects: &mut Vec<StagedObject>,
    ) -> Result<PathBuf, String> {
        cancelled(self.cancel)?;
        valid_name(&entry.name).map_err(|error| format!("{}: {error}", entry.name))?;
        if entry.kind == ImportKind::Unsupported {
            return Err(format!("{}: cloud entry cannot be exported", entry.name));
        }
        if self.active.len() >= 128 || !self.active.insert(entry.id.clone()) {
            return Err(format!(
                "{}: cloud folder hierarchy is cyclic or too deep",
                entry.name
            ));
        }
        let result = (|| {
            for index in 0..10_000 {
                let name = collision_name(entry, index);
                let folder = entry.kind == ImportKind::Folder;
                let mut options = OpenOptions::new();
                options
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No);
                let created = if folder {
                    parent.create_dir(&name).map(|()| None)
                } else {
                    parent.open_with(&name, &options).map(Some)
                };
                let file = match created {
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.to_string()),
                    Ok(file) => file,
                };
                let directory = if folder {
                    Some(
                        parent
                            .open_dir_nofollow(&name)
                            .map_err(|error| error.to_string())?,
                    )
                } else {
                    None
                };
                // Hold descriptors, not mutable pathnames, throughout provider I/O.
                let metadata = if let Some(file) = &file {
                    file.metadata()
                } else {
                    directory
                        .as_ref()
                        .ok_or("missing cloud folder descriptor")?
                        .dir_metadata()
                }
                .map_err(|error| error.to_string())?;
                let slot = objects.len();
                objects.push(StagedObject {
                    parent: parent.try_clone().map_err(|error| error.to_string())?,
                    name: name.clone(),
                    expected: output_identity(&metadata),
                    directory: directory
                        .as_ref()
                        .map(Dir::try_clone)
                        .transpose()
                        .map_err(|error| error.to_string())?,
                });
                let result = if let Some(mut file) = file {
                    let result = self.read(client, entry, &mut file);
                    // Even a partial write belongs to this open file, not a replacement.
                    objects
                        .get_mut(slot)
                        .ok_or("missing staged cloud output")?
                        .expected =
                        output_identity(&file.metadata().map_err(|error| error.to_string())?);
                    result
                } else {
                    self.folder(
                        client,
                        entry,
                        directory
                            .as_ref()
                            .ok_or("missing cloud folder descriptor")?,
                        objects,
                    )
                };
                result?;
                self.status.completed = self.status.completed.saturating_add(1);
                (self.progress)(self.status.clone());
                return Ok(name);
            }
            Err("too many local name collisions".into())
        })();
        self.active.remove(&entry.id);
        result.map_err(|error| format!("{}: {error}", entry.name))
    }

    fn folder(
        &mut self,
        client: &mut CloudClient,
        entry: &ImportEntry,
        dir: &Dir,
        objects: &mut Vec<StagedObject>,
    ) -> Result<(), String> {
        let mut offset = 0;
        loop {
            cancelled(self.cancel)?;
            self.status.phase = "enumerating".into();
            (self.progress)(self.status.clone());
            let ImportReply::Page {
                entries,
                next_offset,
                ..
            } = request(
                client,
                self.account,
                ImportRequest::Browse {
                    folder: Some(entry.id.clone()),
                    offset,
                },
            )?
            else {
                return Err("unexpected cloud folder response".into());
            };
            for child in entries {
                self.copy_entry(client, dir, &child, objects)?;
            }
            match next_offset {
                None => return Ok(()),
                Some(next) if next > offset => offset = next,
                Some(_) => return Err("cloud folder pagination did not advance".into()),
            }
        }
    }

    fn read(
        &mut self,
        client: &mut CloudClient,
        entry: &ImportEntry,
        file: &mut cap_std::fs::File,
    ) -> Result<(), String> {
        self.status.phase = if entry.kind == ImportKind::NativeExport {
            "exporting"
        } else {
            "preparing"
        }
        .into();
        (self.progress)(self.status.clone());
        let ImportReply::Preparing { session } = request(
            client,
            self.account,
            ImportRequest::StartRead {
                id: entry.id.clone(),
                revision: entry.revision.clone(),
            },
        )?
        else {
            return Err("unexpected cloud read response".into());
        };
        let mut read = ReadSession {
            client,
            account: self.account,
            session,
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(120))
            .ok_or("cloud read deadline overflow")?;
        let size = loop {
            cancelled(self.cancel)?;
            match read.request(ImportRequest::ReadStatus {
                session: read.session.clone(),
            })? {
                ImportReply::ReadState(ImportReadState::Ready { size }) => break size,
                ImportReply::ReadState(ImportReadState::Failed(error)) => return Err(error.message),
                ImportReply::ReadState(ImportReadState::Preparing) if Instant::now() < deadline => {
                    std::thread::park_timeout(Duration::from_millis(25));
                }
                _ => return Err("cloud read preparation failed or timed out".into()),
            }
        };
        self.status.phase = "copying".into();
        let mut offset = 0;
        while offset < size {
            cancelled(self.cancel)?;
            let length = u32::try_from(
                size.saturating_sub(offset)
                    .min(u64::from(IMPORT_CHUNK_LIMIT)),
            )
            .map_err(|error| error.to_string())?;
            let ImportReply::Chunk { bytes, eof } = read.request(ImportRequest::ReadChunk {
                session: read.session.clone(),
                offset,
                length,
            })?
            else {
                return Err("unexpected cloud chunk response".into());
            };
            let count = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
            let next_offset = offset
                .checked_add(count)
                .ok_or("cloud read offset overflow")?;
            if count == 0 || count > u64::from(length) || (eof && next_offset != size) {
                return Err("cloud read returned incomplete or excessive bytes".into());
            }
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            offset = next_offset;
            self.status.bytes = self.status.bytes.saturating_add(count);
            (self.progress)(self.status.clone());
        }
        cancelled(self.cancel)?;
        file.sync_all().map_err(|error| error.to_string())
    }
}

// Always release a session, including provider failures and cancellation. The
// server's CloseRead is idempotent. A disconnected service also releases its reads.
struct ReadSession<'a> {
    client: &'a mut CloudClient,
    account: &'a str,
    session: String,
}

impl ReadSession<'_> {
    fn request(&mut self, message: ImportRequest) -> Result<ImportReply, String> {
        request(self.client, self.account, message)
    }
}

impl Drop for ReadSession<'_> {
    fn drop(&mut self) {
        let _ = self.client.import(
            self.account,
            ImportRequest::CloseRead {
                session: self.session.clone(),
            },
        );
    }
}
