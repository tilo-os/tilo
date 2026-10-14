//! Independent imports: provider IDs become new local copies, never sync mounts.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use runtime_fs::cloud::{
    CloudClient, IMPORT_CHUNK_LIMIT, ImportEntry, ImportKind, ImportReadState, ImportReply,
    ImportRequest,
};
use ui::tilo_resource::Settled;

use crate::file_ops::{Change, Outcome};

/// The directory admitted for an import, not merely its mutable pathname.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinationIdentity {
    device: u64,
    inode: u64,
    /// Output metadata distinguishes reused inodes and changed copies. Directory
    /// admission deliberately excludes it: importing changes directory metadata.
    #[serde(default)]
    modified: Option<(i64, i64, u64)>,
}

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
#[cfg(target_os = "linux")]
pub(crate) fn compensate(
    to: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
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
    let moved = match crate::fileops::move_into(&captured, &target_dir) {
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

pub(crate) fn restore(
    to: &str,
    entry: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
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
        let restored = match crate::fileops::move_into(&captured_trash, &captured) {
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
            let rollback = crate::fileops::move_into(&restored, &trash_path).and_then(|back| {
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
pub(crate) fn compensate(
    _: &str,
    _: &DestinationIdentity,
    _: &DestinationIdentity,
) -> Result<Change, String> {
    Err("safe cloud import undo is unavailable on this platform".into())
}

/// Admit only an existing, symlink-free local directory outside sync mounts.
pub(crate) fn pin_destination<'a>(
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

/// Cumulative progress, delivered on the worker thread, not the UI thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    pub phase: String,
    pub completed: u64,
    pub bytes: u64,
    pub failed: Vec<String>,
}

pub(crate) fn execute(
    account: &str,
    entries: &[ImportEntry],
    folder: &str,
    destination: &DestinationIdentity,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
) -> Outcome {
    let Some(socket) = runtime_fs::cloud::default_files_socket() else {
        return Settled::Failed {
            reason: "cloud service is unavailable: no runtime directory".into(),
            partial: Vec::new(),
        };
    };
    execute_at(
        &socket,
        account,
        entries,
        folder,
        destination,
        cancel,
        progress,
    )
}

fn execute_at(
    socket: &Path,
    account: &str,
    entries: &[ImportEntry],
    folder: &str,
    destination: &DestinationIdentity,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
) -> Outcome {
    let mut changes = Vec::new();
    let mut status = TransferProgress {
        phase: "preparing".into(),
        ..Default::default()
    };
    progress(status.clone());
    let result = (|| {
        cancelled(cancel)?;
        let accounts = accounts::config_root()
            .map(|config| {
                crate::remote_accounts::load_in(&config, crate::model::user_home().as_deref())
            })
            .unwrap_or_default();
        let root = independent_folder(
            Path::new(folder),
            accounts
                .iter()
                .filter_map(|account| account.mount.as_deref()),
        )?;
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

/// Retry only top-level entries without a committed receipt. Descendant
/// progress never means that a partially staged root was published.
pub(crate) fn retain_uncommitted(
    account: &str,
    entries: &mut Vec<ImportEntry>,
    changes: &[Change],
) {
    entries.retain(|entry| {
        !changes.iter().any(|change| {
            matches!(change,
        Change::CloudImported { from, .. } if *from == format!("cloud:{account}:{}", entry.id))
        })
    });
}

pub(crate) fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("cloud entry has an invalid local name".into())
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
                let target = if index == 0 {
                    PathBuf::from(&entry.name)
                } else {
                    PathBuf::from(format!("{} ({index})", entry.name))
                };
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
                let name = PathBuf::from(if index == 0 {
                    entry.name.clone()
                } else {
                    format!("{} ({index})", entry.name)
                });
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
                self.status.completed += 1;
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
        let deadline = Instant::now() + Duration::from_secs(120);
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
            let length = u32::try_from((size - offset).min(u64::from(IMPORT_CHUNK_LIMIT)))
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
            if count == 0 || count > u64::from(length) || (eof && offset + count != size) {
                return Err("cloud read returned incomplete or excessive bytes".into());
            }
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            offset += count;
            self.status.bytes += count;
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

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_fs::cloud::{
        CLOUD_FRAMING, CloudRequest, CloudResponse, ImportError, ImportErrorKind,
    };
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    fn entry(name: &str, kind: ImportKind) -> ImportEntry {
        ImportEntry {
            id: name.into(),
            name: name.into(),
            kind,
            revision: Some("revision".into()),
            size: Some(999),
            content_writable: false,
        }
    }

    fn server(
        root: &Path,
        mut reply: impl FnMut(ImportRequest) -> ImportReply + Send + 'static,
    ) -> (PathBuf, std::thread::JoinHandle<()>) {
        let socket = root.join(format!("cloud-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(request) = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
            {
                let CloudRequest::Import { account, request } = request else {
                    panic!("not an import")
                };
                assert_eq!(account, "account");
                CLOUD_FRAMING
                    .write_value(&mut stream, &CloudResponse::Import(reply(request)))
                    .unwrap();
            }
        });
        (socket, handle)
    }

    fn bytes_reply(request: ImportRequest, closed: &Mutex<Vec<String>>) -> ImportReply {
        match request {
            ImportRequest::StartRead { id, revision } => {
                assert_eq!(revision.as_deref(), Some("revision"));
                ImportReply::Preparing { session: id }
            }
            ImportRequest::ReadStatus { .. } => {
                ImportReply::ReadState(ImportReadState::Ready { size: 6 })
            }
            ImportRequest::ReadChunk { offset, length, .. } => {
                assert_eq!(offset, 0);
                assert_eq!(length, 6);
                ImportReply::Chunk {
                    bytes: b"export".to_vec(),
                    eof: true,
                }
            }
            ImportRequest::CloseRead { session } => {
                closed.lock().unwrap().push(session);
                ImportReply::Closed
            }
            ImportRequest::Browse { .. } => panic!("unexpected browse request"),
        }
    }

    #[test]
    fn native_bytes_and_files_are_independent_copies_with_collision_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("report.pdf"), b"original").unwrap();
        // A pre-existing symlink must be treated as a collision, never followed.
        std::os::unix::fs::symlink(temp.path().join("outside"), dest.join("file")).unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &observed));
        let status = Mutex::new(TransferProgress::default());
        let outcome = execute_at(
            &socket,
            "account",
            &[
                entry("report.pdf", ImportKind::NativeExport),
                entry("file", ImportKind::File),
            ],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |progress| *status.lock().unwrap() = progress,
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 2);
        assert!(
            matches!(&outcome.result()[0], Change::CloudImported { to, .. } if to.ends_with("report.pdf (1)"))
        );
        assert_eq!(
            std::fs::read(dest.join("report.pdf (1)")).unwrap(),
            b"export"
        );
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
        assert_eq!(std::fs::read(dest.join("report.pdf")).unwrap(), b"original");
        assert!(!temp.path().join("outside").exists());
        assert_eq!(closed.lock().unwrap().len(), 2);
        assert_eq!(status.lock().unwrap().bytes, 12);
        assert_eq!(status.lock().unwrap().completed, 2);
        let undone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "import".into(),
            },
            Some(outcome.result()),
        );
        assert!(undone.is_done(), "{undone:?}");
        assert!(!dest.join("report.pdf (1)").exists());
        assert!(!dest.join("file (1)").exists());
        let redone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "undo-import".into(),
            },
            Some(undone.result()),
        );
        assert!(redone.is_done(), "{redone:?}");
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
        assert!(
            redone
                .result()
                .iter()
                .all(|change| matches!(change, Change::CloudImported { .. }))
        );
        std::fs::rename(dest.join("file (1)"), dest.join("saved")).unwrap();
        std::fs::write(dest.join("file (1)"), b"unrelated").unwrap();
        let refused = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "redo-import".into(),
            },
            Some(redone.result()),
        );
        assert!(!refused.is_done(), "{refused:?}");
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"unrelated");
        assert_eq!(std::fs::read(dest.join("report.pdf")).unwrap(), b"original");
    }

    #[test]
    fn compensation_refuses_replaced_outputs_and_renamed_or_replaced_destinations() {
        for replacement in ["output", "destination", "renamed", "symlink"] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) =
                server(temp.path(), move |request| bytes_reply(request, &closed));
            let outcome = execute_at(
                &socket,
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &AtomicBool::new(false),
                |_| {},
            );
            server.join().unwrap();
            assert!(outcome.is_done(), "{outcome:?}");
            // Exercise the serialized document receipt, not an in-memory helper.
            let changes: Vec<Change> =
                serde_json::from_slice(&serde_json::to_vec(outcome.result()).unwrap()).unwrap();
            match replacement {
                "output" => {
                    std::fs::rename(dest.join("file"), dest.join("original")).unwrap();
                    std::fs::write(dest.join("file"), b"unrelated").unwrap();
                }
                "symlink" => {
                    std::fs::rename(dest.join("file"), dest.join("original")).unwrap();
                    std::fs::write(temp.path().join("unrelated"), b"unrelated").unwrap();
                    std::os::unix::fs::symlink(temp.path().join("unrelated"), dest.join("file"))
                        .unwrap();
                }
                _ => {
                    std::fs::rename(&dest, temp.path().join("original")).unwrap();
                    if replacement == "destination" {
                        std::fs::create_dir(&dest).unwrap();
                        std::fs::write(dest.join("file"), b"unrelated").unwrap();
                    }
                }
            }
            let undone = crate::file_ops::execute(
                &crate::file_ops::FileOp::Revert {
                    op: "import".into(),
                },
                Some(&changes),
            );
            assert!(!undone.is_done(), "{replacement}: {undone:?}");
            assert!(undone.result().is_empty());
            if replacement == "output" || replacement == "destination" || replacement == "symlink" {
                assert_eq!(std::fs::read(dest.join("file")).unwrap(), b"unrelated");
            }
            let original = if replacement == "output" || replacement == "symlink" {
                dest.join("original")
            } else {
                temp.path().join("original/file")
            };
            assert_eq!(std::fs::read(original).unwrap(), b"export");
        }
    }

    #[test]
    fn compensation_preserves_user_edits_to_the_imported_inode() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &closed));
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 1);
        let changes: Vec<Change> =
            serde_json::from_slice(&serde_json::to_vec(outcome.result()).unwrap()).unwrap();
        let path = temp.path().join("file");
        let before = std::fs::metadata(&path).unwrap();
        std::fs::write(&path, b"independent user edits").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        let undone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "import".into(),
            },
            Some(&changes),
        );
        assert!(!undone.is_done(), "{undone:?}");
        assert!(undone.result().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"independent user edits");
    }

    #[test]
    fn cancellation_removes_partial_bytes_and_closes_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &observed));
        let cancel = AtomicBool::new(false);
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("partial", ImportKind::File)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &cancel,
            |progress| {
                if progress.bytes != 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert!(!temp.path().join("partial").exists());
        assert_eq!(*closed.lock().unwrap(), ["partial"]);
    }

    #[test]
    fn provider_failure_keeps_previous_copy_but_cleans_failed_entry() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| {
            if matches!(&request, ImportRequest::ReadStatus { session } if session == "bad") {
                return ImportReply::ReadState(ImportReadState::Failed(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "source changed".into(),
                }));
            }
            bytes_reply(request, &observed)
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[
                entry("good", ImportKind::File),
                entry("bad", ImportKind::File),
            ],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(std::fs::read(temp.path().join("good")).unwrap(), b"export");
        assert!(!temp.path().join("bad").exists());
        assert_eq!(*closed.lock().unwrap(), ["good", "bad"]);
    }

    #[test]
    fn folders_are_paginated_and_recursive() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, offset } => {
                let folder = folder.unwrap();
                let (entries, next_offset) = match (folder.as_str(), offset) {
                    ("folder", 0) => (vec![entry("nested", ImportKind::Folder)], Some(1)),
                    ("folder", 1) => (vec![entry("second", ImportKind::File)], None),
                    ("nested", 0) => (vec![entry("first", ImportKind::NativeExport)], None),
                    _ => panic!("unexpected page"),
                };
                ImportReply::Page {
                    folder,
                    entries,
                    next_offset,
                }
            }
            request => bytes_reply(request, &observed),
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("folder", ImportKind::Folder)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(
            std::fs::read(temp.path().join("folder/nested/first")).unwrap(),
            b"export"
        );
        assert_eq!(
            std::fs::read(temp.path().join("folder/second")).unwrap(),
            b"export"
        );
        assert_eq!(closed.lock().unwrap().len(), 2);
    }

    #[test]
    fn invalid_remote_names_remove_the_incomplete_folder() {
        let temp = tempfile::tempdir().unwrap();
        let (socket, server) = server(temp.path(), |request| match request {
            ImportRequest::Browse { .. } => ImportReply::Page {
                folder: "folder".into(),
                entries: vec![entry("../escape", ImportKind::File)],
                next_offset: None,
            },
            _ => panic!("malformed name must not start a read"),
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("folder", ImportKind::Folder)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert!(!temp.path().join("folder").exists());
    }

    #[test]
    fn queued_import_rejects_replaced_destination_before_provider_access() {
        for replace_parent in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let parent = temp.path().join("parent");
            let dest = parent.join("dest");
            std::fs::create_dir_all(&dest).unwrap();
            let pinned = pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap();
            let replaced = if replace_parent { &parent } else { &dest };
            std::fs::rename(replaced, temp.path().join("original")).unwrap();
            std::fs::create_dir_all(&dest).unwrap();
            let outcome = execute_at(
                &temp.path().join("absent.sock"),
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pinned,
                &AtomicBool::new(false),
                |_| {},
            );
            let Settled::Failed { reason, partial } = outcome else {
                panic!("replaced destination was accepted");
            };
            assert!(reason.contains("changed since admission"), "{reason}");
            assert!(partial.is_empty());
            assert!(!dest.join("file").exists());
        }
    }

    #[test]
    fn provider_failure_never_deletes_a_public_name_created_during_io() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let receiving = dest.clone();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| {
            if matches!(request, ImportRequest::ReadChunk { .. }) {
                assert!(
                    !receiving.join("file").exists(),
                    "incomplete bytes must be private"
                );
                std::fs::create_dir(receiving.join("file")).unwrap();
                std::fs::write(receiving.join("file/user"), b"user data").unwrap();
                return ImportReply::Error(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "provider failed".into(),
                });
            }
            bytes_reply(request, &closed)
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert_eq!(std::fs::read(dest.join("file/user")).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(dest).unwrap().count(), 1);
    }

    #[test]
    fn failed_descendant_keeps_whole_root_private_and_prior_receipts_committed() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, .. } => ImportReply::Page {
                folder: folder.unwrap(),
                entries: vec![
                    entry("copied-child", ImportKind::File),
                    entry("broken-child", ImportKind::File),
                ],
                next_offset: None,
            },
            ImportRequest::ReadStatus { session } if session == "broken-child" => {
                ImportReply::ReadState(ImportReadState::Failed(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "provider failed".into(),
                }))
            }
            request => bytes_reply(request, &closed),
        });
        let entries = [
            entry("good", ImportKind::File),
            entry("folder", ImportKind::Folder),
        ];
        let outcome = execute_at(
            &socket,
            "account",
            &entries,
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.reason().unwrap().contains("broken-child"));
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(std::fs::read(dest.join("good")).unwrap(), b"export");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
        // The retry surface filters committed top-level IDs, not descendant
        // progress. Thus a partially copied folder restarts exactly once.
        let mut remaining = entries.to_vec();
        retain_uncommitted("account", &mut remaining, outcome.result());
        assert_eq!(remaining.len(), 1);
        let closed = Mutex::new(Vec::new());
        let (socket, server) = self::server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, .. } => ImportReply::Page {
                folder: folder.unwrap(),
                entries: vec![entry("copied-child", ImportKind::File)],
                next_offset: None,
            },
            request => bytes_reply(request, &closed),
        });
        let retry = execute_at(
            &socket,
            "account",
            &remaining,
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(retry.is_done(), "{retry:?}");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 2);
        assert_eq!(
            std::fs::read(dest.join("folder/copied-child")).unwrap(),
            b"export"
        );
        assert!(!dest.join("good (1)").exists());
    }

    #[test]
    fn public_replacements_during_copy_are_never_cleaned_or_overwritten() {
        for cancel_copy in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) =
                server(temp.path(), move |request| bytes_reply(request, &closed));
            let cancel = AtomicBool::new(false);
            let touched = AtomicBool::new(false);
            let outcome = execute_at(
                &socket,
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &cancel,
                |status| {
                    if status.bytes == 6 && !touched.swap(true, Ordering::Relaxed) {
                        assert!(!dest.join("file").exists());
                        std::fs::write(dest.join("file"), b"user replacement").unwrap();
                        cancel.store(cancel_copy, Ordering::Relaxed);
                    }
                },
            );
            server.join().unwrap();
            assert_eq!(
                std::fs::read(dest.join("file")).unwrap(),
                b"user replacement"
            );
            assert_eq!(outcome.is_done(), !cancel_copy);
            if cancel_copy {
                assert!(outcome.result().is_empty());
                assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
            } else {
                assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
                assert_eq!(outcome.result().len(), 1);
            }
        }
    }

    #[test]
    fn tampered_private_stages_are_retained_without_deleting_user_objects() {
        for tamper in ["container", "file", "folder-child"] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) = server(temp.path(), move |request| match request {
                ImportRequest::Browse { folder, .. } => ImportReply::Page {
                    folder: folder.unwrap(),
                    entries: vec![entry("file", ImportKind::File)],
                    next_offset: None,
                },
                request => bytes_reply(request, &closed),
            });
            let cancel = AtomicBool::new(false);
            let touched = AtomicBool::new(false);
            let outcome = execute_at(
                &socket,
                "account",
                &[entry(
                    if tamper == "folder-child" {
                        "folder"
                    } else {
                        "file"
                    },
                    if tamper == "folder-child" {
                        ImportKind::Folder
                    } else {
                        ImportKind::File
                    },
                )],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &cancel,
                |status| {
                    if status.bytes == 6 && !touched.swap(true, Ordering::Relaxed) {
                        let stage = std::fs::read_dir(&dest)
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap()
                            .path();
                        assert!(
                            stage
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".cloud-import-")
                        );
                        match tamper {
                            "container" => {
                                std::fs::rename(&stage, dest.join("saved-stage")).unwrap();
                                std::fs::create_dir(&stage).unwrap();
                                std::fs::write(stage.join("user"), b"user data").unwrap();
                            }
                            "file" => {
                                std::fs::rename(stage.join("file"), stage.join("saved-file"))
                                    .unwrap();
                                std::fs::write(stage.join("file"), b"user data").unwrap();
                            }
                            _ => std::fs::write(stage.join("folder/user"), b"user data").unwrap(),
                        }
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            );
            server.join().unwrap();
            assert!(touched.load(Ordering::Relaxed));
            assert!(!outcome.is_done());
            assert!(outcome.result().is_empty());
            assert!(
                outcome
                    .reason()
                    .unwrap()
                    .contains("private staging retained"),
                "{outcome:?}"
            );
            let stage = std::fs::read_dir(&dest)
                .unwrap()
                .map(Result::unwrap)
                .find(|item| {
                    item.file_name()
                        .to_string_lossy()
                        .starts_with(".cloud-import-")
                })
                .unwrap()
                .path();
            let user = match tamper {
                "container" => stage.join("user"),
                "file" => stage.join("file"),
                _ => stage.join("folder/user"),
            };
            assert_eq!(std::fs::read(user).unwrap(), b"user data");
            assert!(!dest.join("file").exists());
            assert!(!dest.join("folder").exists());
        }
    }

    #[test]
    fn path_alias_introduced_during_io_cannot_receive_output_or_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        let original = temp.path().join("original");
        let unrelated = temp.path().join("unrelated");
        std::fs::create_dir(&dest).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("file"), b"user data").unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &closed));
        let changed = AtomicBool::new(false);
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |status| {
                if status.bytes == 6 && !changed.swap(true, Ordering::Relaxed) {
                    std::fs::rename(&dest, &original).unwrap();
                    std::os::unix::fs::symlink(&unrelated, &dest).unwrap();
                }
            },
        );
        server.join().unwrap();
        assert!(changed.load(Ordering::Relaxed));
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert_eq!(std::fs::read(unrelated.join("file")).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(original).unwrap().count(), 0);
    }

    #[test]
    fn admission_requires_existing_symlink_free_directory() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, b"original").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        for rejected in [
            file,
            alias,
            temp.path().join("missing"),
            temp.path().join(".."),
        ] {
            assert!(pin_destination(rejected.to_str().unwrap(), std::iter::empty()).is_err());
        }
    }

    #[test]
    fn execution_rechecks_current_sync_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().to_str().unwrap();
        let pinned = pin_destination(folder, std::iter::empty()).unwrap();
        assert!(independent_folder(temp.path(), std::iter::once(temp.path())).is_err());
        assert_eq!(pin_destination(folder, std::iter::empty()).unwrap(), pinned);
    }

    #[test]
    fn destination_components_cannot_be_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", temp.path().join("link")).unwrap();
        assert!(anchored_folder(&temp.path().join("link")).is_err());
    }
}
