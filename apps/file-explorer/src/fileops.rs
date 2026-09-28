//! File-system mutations behind the file explorer's edit commands:
//! new folder, rename, delete-to-trash, copy and move.
//!
//! Each operation is a small, independently-testable free function that
//! works on real paths and returns a `Result<_, String>` (the string is a
//! user-facing error the app surfaces as a toast). Destination-collision
//! handling is centralised in [`unique_destination`] so copy/move never
//! silently clobber an existing file.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

const MAX_DESTINATION_SUFFIX: u32 = 10_000;

/// Accept one portable filesystem leaf name, never a path expression.
///
/// Both action prompts and inline rename reach these operations. In
/// particular, `.` and `..` must not be accepted as names: either would make
/// a supposedly local operation address an ancestor directory instead.
fn leaf_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::i18n::t("files-name-empty"));
    }
    if host_fs::names::rejected_by_host(name) {
        return Err(crate::i18n::t("files-name-invalid"));
    }
    let path = Path::new(name);
    if name.contains('/')
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path.components().count() != 1
    {
        return Err(crate::i18n::t("files-name-slash"));
    }
    Ok(name)
}

/// Refuse a recursive directory copy or move before any destination exists.
/// Resolving the destination catches a symlink that points back into `src`.
fn ensure_destination_is_outside_source(src: &Path, dest_dir: &Path) -> Result<(), String> {
    if !std::fs::symlink_metadata(src)
        .map_err(|error| error.to_string())?
        .is_dir()
    {
        return Ok(());
    }
    let source = std::fs::canonicalize(src).map_err(|error| error.to_string())?;
    let destination = std::fs::canonicalize(dest_dir).map_err(|error| error.to_string())?;
    if destination.starts_with(&source) {
        return Err("cannot copy or move a folder into itself".into());
    }
    Ok(())
}

// --- trash ----------------------------------------------------------------

pub use host_fs::trash::{is_trash_dir, trash_dir};

/// Ensure the trash files directory exists, so opening an empty trash shows an
/// empty listing rather than a "no such directory" error. Returns the dir.
pub fn ensure_trash_dir() -> Option<PathBuf> {
    let d = trash_dir()?;
    let _ = std::fs::create_dir_all(&d);
    Some(d)
}

/// Snapshot the names currently visible in the home Trash.
///
/// Files uses this only to verify that a just-completed deletion produced an
/// exact, local restore target. Some filesystems use a different Trash or do
/// not expose one at all; in those cases the caller must simply not offer an
/// optimistic Undo control.
pub fn trash_entries_snapshot() -> Option<Vec<PathBuf>> {
    let dir = trash_dir()?;
    let entries = std::fs::read_dir(dir).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect(),
    )
}

/// Permanently remove everything in the trash. Returns how many items were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging its items fails.
pub fn empty_trash() -> Result<usize, String> {
    host_fs::trash::empty()
}

/// Restore the trashed items backing `entries` to their original locations.
/// Returns how many were restored.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// restoring the matched items fails (e.g. the original location no longer
/// exists).
pub fn restore_entries(entries: &[PathBuf]) -> Result<usize, String> {
    host_fs::trash::restore(entries)
}

/// Permanently delete the trashed items backing `entries` (file + info).
/// Returns how many were purged.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable, or listing or
/// purging the matched items fails.
pub fn purge_entries(entries: &[PathBuf]) -> Result<usize, String> {
    host_fs::trash::purge(entries)
}

/// Pick a non-colliding path for `name` inside `dir`. If `dir/name` is
/// free it's returned as-is; otherwise a numeric suffix is inserted
/// before the extension: `report.txt` → `report 2.txt` → `report 3.txt`.
///
/// # Errors
/// Returns an error if a candidate cannot be inspected or all supported suffixes
/// are occupied. Never falls back to an occupied directory entry.
pub fn unique_destination(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let is_free = |candidate: &Path| match std::fs::symlink_metadata(candidate) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(crate::i18n::t_args(
            "files-destination-inspect-failed",
            &[
                crate::i18n::arg("path", candidate.to_string_lossy().into_owned()),
                crate::i18n::arg("error", error.to_string()),
            ],
        )),
    };
    // A dangling link still occupies its directory entry. Following it here
    // would let a later copy create its target, or a move replace the link.
    let candidate = dir.join(name);
    if is_free(&candidate)? {
        return Ok(candidate);
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2..MAX_DESTINATION_SUFFIX {
        let trial = dir.join(suffixed_name(&stem, ext.as_deref(), n));
        if is_free(&trial)? {
            return Ok(trial);
        }
    }
    Err(crate::i18n::t_args(
        "files-destination-names-exhausted",
        &[
            crate::i18n::arg("name", name),
            crate::i18n::arg("path", dir.to_string_lossy().into_owned()),
        ],
    ))
}

fn suffixed_name(stem: &str, extension: Option<&str>, suffix: u32) -> String {
    match extension {
        Some(extension) => format!("{stem} {suffix}.{extension}"),
        None => format!("{stem} {suffix}"),
    }
}

/// Create a new folder named `name` (deduplicated) inside `parent`.
///
/// # Errors
///
/// Returns an error if `name` is empty, a path expression (`/`, `.` or `..`),
/// or if creating the directory fails (e.g. `parent` is not writable).
pub fn new_folder(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = match leaf_name(name) {
        Ok(name) => name,
        Err(_) if name.trim().is_empty() => return Err(crate::i18n::t("files-folder-name-empty")),
        Err(error) => return Err(error),
    };
    let dest = unique_destination(parent, name)?;
    std::fs::create_dir(&dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Rename `path` to `new_name` within the same directory.
///
/// # Errors
///
/// Returns an error if `new_name` is empty or a path expression, if `path`
/// has no parent directory, if a file already exists at the destination name,
/// or if the OS rename fails.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = leaf_name(new_name)?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::i18n::t("files-no-parent"))?;
    let dest = parent.join(new_name);
    if dest == path {
        return Ok(dest);
    }
    if let Ok(destination) = std::fs::symlink_metadata(&dest) {
        // Case-insensitive filesystems resolve a new spelling to the source.
        // Do not mistake that for a collision, or follow links into the source.
        // symlink_metadata also protects dangling destinations, unlike exists().
        let same_source = !destination.file_type().is_symlink()
            && std::fs::symlink_metadata(path).is_ok_and(|source| !source.file_type().is_symlink())
            && std::fs::canonicalize(path)
                .ok()
                .zip(std::fs::canonicalize(&dest).ok())
                .is_some_and(|(source, destination)| source == destination);
        if !same_source {
            return Err(crate::i18n::t_args(
                "files-name-exists",
                &[crate::i18n::arg("name", new_name)],
            ));
        }
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Move `paths` to the trash. Falls back to a clear message if the
/// platform trash isn't available.
///
/// # Errors
///
/// Returns an error if the platform trash is unavailable or the delete
/// fails (e.g. a path no longer exists, or crosses a filesystem the trash
/// backend can't handle).
pub fn delete_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    trash::delete_all(paths).map_err(|e| e.to_string())
}

/// Recursively copy `src` into the directory `dest_dir`, choosing a
/// non-colliding name. Returns the created destination path.
///
/// # Errors
///
/// Returns an error if `src` has no file name, or the recursive copy fails
/// (a read/write/permission error partway through the tree).
pub fn copy_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, false, &mut NoopTransferObserver)
}

/// Move `src` into `dest_dir` without replacing a concurrent destination.
/// Linux, macOS and Windows use an atomic no-replace rename on the same
/// filesystem. The cross-filesystem fallback stages a complete private copy,
/// removes the source, and only then publishes the destination atomically.
///
/// # Errors
///
/// Returns an error if `src` has no file name, staging fails, the source cannot
/// be removed, or the staged tree cannot be published or restored.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, true, &mut NoopTransferObserver)
}

trait TransferObserver {
    fn allow_direct_rename(&self) -> bool {
        true
    }

    fn before_copy_source(&mut self, _source: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn before_publish(&mut self, _destination: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn before_source_remove(&mut self, _source: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

struct NoopTransferObserver;
impl TransferObserver for NoopTransferObserver {}

fn transfer_into_with(
    src: &Path,
    dest_dir: &Path,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, String> {
    ensure_destination_is_outside_source(src, dest_dir)?;
    let name = src
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| crate::i18n::t("files-source-no-name"))?;
    let source_parent = src
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let destination_dir = std::fs::canonicalize(dest_dir).ok();
    if remove_source && source_parent.is_some() && source_parent == destination_dir {
        return Err("the item is already in this folder".into());
    }

    let names = DestinationNames::new(&name);
    // Preserve the existing diagnostic for an inaccessible destination before
    // allocating staging storage beneath it.
    inspect_destination(&dest_dir.join(names.for_suffix(1)))?;

    if remove_source && observer.allow_direct_rename() {
        for suffix in 1..MAX_DESTINATION_SUFFIX {
            let destination = dest_dir.join(names.for_suffix(suffix));
            if !inspect_destination(&destination)? {
                continue;
            }
            observer
                .before_publish(&destination)
                .map_err(|error| error.to_string())?;
            match rename_noreplace(src, &destination) {
                Ok(()) => return Ok(destination),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => break,
            }
        }
    }

    let staging = stage_private_copy(src, dest_dir, observer).map_err(|error| error.to_string())?;
    if remove_source {
        let quarantine = match quarantine_source(src) {
            Ok(path) => path,
            Err(error) => return Err(staging.error_with_cleanup(error.to_string())),
        };
        let deletion = observer
            .before_source_remove(&quarantine)
            .and_then(|()| remove_path(&quarantine));
        if let Err(error) = deletion {
            return Err(restore_failed_cut(
                src,
                &quarantine,
                &staging,
                error.to_string(),
            ));
        }
    }

    match publish_staging(&staging.path, dest_dir, &names, observer) {
        Ok(destination) => {
            staging.finish_success();
            Ok(destination)
        }
        Err(publish_error) if remove_source => Err(restore_failed_cut(
            src,
            Path::new(""),
            &staging,
            publish_error,
        )),
        Err(error) => Err(staging.error_with_cleanup(error)),
    }
}

fn quarantine_source(src: &Path) -> std::io::Result<PathBuf> {
    let parent = src
        .parent()
        .ok_or_else(|| std::io::Error::other("source has no parent directory"))?;
    for _ in 0..32 {
        let quarantine = parent.join(format!(".tilo-transfer-source-{}", uuid::Uuid::new_v4()));
        match rename_noreplace(src, &quarantine) {
            Ok(()) => {
                if let Err(error) = host_fs::mark_private(&quarantine) {
                    return match rename_noreplace(&quarantine, src) {
                        Ok(()) => Err(error),
                        Err(rollback_error) => Err(std::io::Error::new(
                            rollback_error.kind(),
                            format!(
                                "{error}; source rollback failed: {rollback_error}; original retained at {}",
                                quarantine.display()
                            ),
                        )),
                    };
                }
                return Ok(quarantine);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate private source quarantine",
    ))
}

fn restore_failed_cut(
    source: &Path,
    quarantine: &Path,
    staging: &StagingPath,
    original_error: String,
) -> String {
    let quarantine_error = if quarantine.as_os_str().is_empty() {
        None
    } else {
        remove_path(quarantine).err()
    };
    match copy_tree_exclusive(&staging.path, source, &mut NoopTransferObserver) {
        Ok(()) => {
            let mut error = staging.error_with_cleanup(original_error);
            if let Some(quarantine_error) = quarantine_error {
                let _ = write!(
                    error,
                    "; partial source quarantine retained at {}: {quarantine_error}",
                    quarantine.display()
                );
            }
            error
        }
        Err(restore_error) => {
            staging.retain();
            format!(
                "{original_error}; source restoration failed: {restore_error}; staged data retained at {}",
                staging.path.display()
            )
        }
    }
}

struct DestinationNames {
    original: String,
    stem: String,
    extension: Option<String>,
}

impl DestinationNames {
    fn new(name: &str) -> Self {
        let path = Path::new(name);
        Self {
            original: name.to_owned(),
            stem: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            extension: path
                .extension()
                .map(|extension| extension.to_string_lossy().into_owned()),
        }
    }

    fn for_suffix(&self, suffix: u32) -> String {
        if suffix == 1 {
            self.original.clone()
        } else {
            suffixed_name(&self.stem, self.extension.as_deref(), suffix)
        }
    }
}

fn inspect_destination(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(destination_inspection_error(path, &error)),
    }
}

fn publish_staging(
    staging: &Path,
    dest_dir: &Path,
    names: &DestinationNames,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, String> {
    for suffix in 1..MAX_DESTINATION_SUFFIX {
        let destination = dest_dir.join(names.for_suffix(suffix));
        if !inspect_destination(&destination)? {
            continue;
        }
        observer
            .before_publish(&destination)
            .map_err(|error| error.to_string())?;
        match rename_noreplace(staging, &destination) {
            Ok(()) => return Ok(destination),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(crate::i18n::t_args(
        "files-destination-names-exhausted",
        &[
            crate::i18n::arg("name", names.original.clone()),
            crate::i18n::arg("path", dest_dir.to_string_lossy().into_owned()),
        ],
    ))
}

struct StagingPath {
    container: PathBuf,
    path: PathBuf,
    retain: std::cell::Cell<bool>,
}

impl StagingPath {
    fn retain(&self) {
        self.retain.set(true);
    }

    fn error_with_cleanup(&self, error: String) -> String {
        match remove_path(&self.container) {
            Ok(()) => {
                self.retain();
                error
            }
            Err(cleanup_error) => {
                self.retain();
                format!(
                    "{error}; private staging retained at {}: {cleanup_error}",
                    self.path.display()
                )
            }
        }
    }

    fn finish_success(&self) {
        // Publication moved the payload out, so failure here can leave only
        // an empty private container and must not make a successful transfer
        // retry under another suffix.
        if let Err(error) = remove_path(&self.container) {
            tracing::warn!(
                path = %self.container.display(),
                %error,
                "empty Files transfer staging container could not be removed"
            );
        }
        self.retain();
    }
}

impl Drop for StagingPath {
    fn drop(&mut self) {
        if !self.retain.get()
            && let Err(error) = remove_path(&self.container)
        {
            tracing::warn!(
                path = %self.container.display(),
                %error,
                "Files transfer staging cleanup failed during unwinding"
            );
        }
    }
}

fn stage_private_copy(
    src: &Path,
    dest_dir: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<StagingPath> {
    for _ in 0..32 {
        let container = dest_dir.join(format!(".tilo-transfer-{}", uuid::Uuid::new_v4()));
        match host_fs::create_private_dir(&container) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        let path = container.join("payload");
        match copy_tree_exclusive(src, &path, observer) {
            Ok(()) => {
                return Ok(StagingPath {
                    container,
                    path,
                    retain: std::cell::Cell::new(false),
                });
            }
            Err(error) => {
                return match remove_path(&container) {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(std::io::Error::new(
                        cleanup_error.kind(),
                        format!(
                            "{error}; private staging retained at {}: {cleanup_error}",
                            path.display()
                        ),
                    )),
                };
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate private transfer staging",
    ))
}

// Rename without replacement, private staging and symlink copies are the only
// host-specific pieces of the transfer algorithm; all staging, cleanup and
// retry policy is shared.
use host_fs::rename_noreplace;

fn destination_inspection_error(path: &Path, error: &std::io::Error) -> String {
    crate::i18n::t_args(
        "files-destination-inspect-failed",
        &[
            crate::i18n::arg("path", path.to_string_lossy().into_owned()),
            crate::i18n::arg("error", error.to_string()),
        ],
    )
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
        }
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_tree_exclusive(
    src: &Path,
    dest: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    let parent_path = src
        .parent()
        .ok_or_else(|| std::io::Error::other("source has no parent directory"))?;
    let name = src
        .file_name()
        .ok_or_else(|| std::io::Error::other("source has no file name"))?;
    let parent = cap_std::fs::Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    copy_capability_entry(&parent, name, src, dest, observer)
}

fn copy_capability_entry(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    display_path: &Path,
    dest: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};

    let initial = parent.symlink_metadata(name)?;
    observer.before_copy_source(display_path)?;
    if initial.file_type().is_symlink() {
        let target = parent.read_link_contents(name)?;
        return create_symlink(&target, dest);
    }

    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true);
    let opened = parent.open_with(name, &options)?.into_std();
    let metadata = opened.metadata()?;

    if metadata.is_dir() {
        std::fs::create_dir(dest)?;
        let source = cap_std::fs::Dir::from_std_file(opened);
        for entry in source.entries()? {
            let entry = entry?;
            let child_name = entry.file_name();
            copy_capability_entry(
                &source,
                &child_name,
                &display_path.join(&child_name),
                &dest.join(&child_name),
                observer,
            )?;
        }
        std::fs::set_permissions(dest, metadata.permissions())?;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(std::io::Error::other("source type changed during copy"));
    }

    let mut source = opened;
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    std::io::copy(&mut source, &mut destination)?;
    destination.set_permissions(metadata.permissions())
}

fn create_symlink(target: &Path, dest: &Path) -> std::io::Result<()> {
    if !host_fs::HAS_SYMLINKS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "copying symbolic links is not supported on this platform",
        ));
    }
    host_fs::symlink(target, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    type TestHook<'a> = Box<dyn FnMut(&Path) -> std::io::Result<()> + 'a>;

    #[derive(Default)]
    struct TestTransferObserver<'a> {
        force_staging: bool,
        before_copy_source: Option<TestHook<'a>>,
        before_publish: Option<TestHook<'a>>,
        before_source_remove: Option<TestHook<'a>>,
    }

    impl TransferObserver for TestTransferObserver<'_> {
        fn allow_direct_rename(&self) -> bool {
            !self.force_staging
        }

        fn before_copy_source(&mut self, source: &Path) -> std::io::Result<()> {
            self.before_copy_source
                .as_mut()
                .map_or(Ok(()), |hook| hook(source))
        }

        fn before_publish(&mut self, destination: &Path) -> std::io::Result<()> {
            self.before_publish
                .as_mut()
                .map_or(Ok(()), |hook| hook(destination))
        }

        fn before_source_remove(&mut self, source: &Path) -> std::io::Result<()> {
            self.before_source_remove
                .as_mut()
                .map_or(Ok(()), |hook| hook(source))
        }
    }

    fn assert_no_transfer_staging(directory: &Path) {
        assert!(
            fs::read_dir(directory).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tilo-transfer-")
            }),
            "private transfer staging was cleaned"
        );
    }

    fn assert_no_source_quarantine(directory: &Path) {
        assert!(
            fs::read_dir(directory).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tilo-transfer-source-")
            }),
            "source quarantine was cleaned"
        );
    }

    /// Windows mutations reject stream names without an XDG trash and junction copies never read outside the tree.
    #[test]
    #[cfg(windows)]
    fn windows_file_operations_refuse_streams_and_junction_escapes() {
        // --- phase: windows_mutations_reject_stream_names_and_do_not_present_an_xdg_trash ---
        {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("document.txt");
            fs::write(&source, b"keep this file").unwrap();
            for name in ["document.txt:hidden", "document.txt."] {
                assert!(rename(&source, name).is_err());
                assert!(new_folder(directory.path(), name).is_err());
            }
            assert_eq!(fs::read(&source).unwrap(), b"keep this file");
            assert!(trash_dir().is_none());
            assert!(restore_entries(std::slice::from_ref(&source)).is_err());
            assert!(purge_entries(std::slice::from_ref(&source)).is_err());
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        }

        // --- phase: copying_a_junction_never_reads_or_copies_the_outside_tree ---
        {
            let directory = tempfile::tempdir().unwrap();
            let outside = directory.path().join("outside");
            let source = directory.path().join("source");
            let destination = directory.path().join("destination");
            for path in [&outside, &source, &destination] {
                fs::create_dir(path).unwrap();
            }
            fs::write(outside.join("private.txt"), b"outside data").unwrap();
            let junction = source.join("link");
            let result = std::process::Command::new("cmd.exe")
                .args(["/c", "mklink", "/J"])
                .arg(&junction)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(copy_into(&source, &destination).is_err());
            assert!(!destination.join("source/link/private.txt").exists());
            assert_eq!(
                fs::read(outside.join("private.txt")).unwrap(),
                b"outside data"
            );
            fs::remove_dir(&junction).unwrap();
        }
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Destination names dedup, folders are created, renamed, copied and moved, trash is the XDG home trash and exhausted names never overwrite.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn basic_file_operations_dedup_and_never_overwrite() {
        // --- phase: unique_destination_dedups ---
        {
            let d = tmp();
            let root = d.path();
            // Free name is returned verbatim.
            assert_eq!(
                unique_destination(root, "a.txt").unwrap(),
                root.join("a.txt")
            );
            fs::write(root.join("a.txt"), b"x").unwrap();
            assert_eq!(
                unique_destination(root, "a.txt").unwrap(),
                root.join("a 2.txt")
            );
            fs::write(root.join("a 2.txt"), b"x").unwrap();
            assert_eq!(
                unique_destination(root, "a.txt").unwrap(),
                root.join("a 3.txt")
            );
            // Extension-less names.
            fs::create_dir(root.join("folder")).unwrap();
            assert_eq!(
                unique_destination(root, "folder").unwrap(),
                root.join("folder 2")
            );
        }

        // --- phase: new_folder_creates_and_dedups ---
        {
            let d = tmp();
            let p = new_folder(d.path(), "Stuff").unwrap();
            assert!(p.is_dir());
            let p2 = new_folder(d.path(), "Stuff").unwrap();
            assert_eq!(p2.file_name().unwrap().to_string_lossy(), "Stuff 2");
            assert!(new_folder(d.path(), "  ").is_err());
            assert!(new_folder(d.path(), "a/b").is_err());
            assert!(new_folder(d.path(), ".").is_err());
            assert!(new_folder(d.path(), "..").is_err());
            let unicode = new_folder(d.path(), "文書 📁").unwrap();
            assert!(unicode.is_dir());
        }

        // --- phase: rename_moves_within_dir ---
        {
            let d = tmp();
            let f = d.path().join("old.txt");
            fs::write(&f, b"x").unwrap();
            let renamed = rename(&f, "new.txt").unwrap();
            assert!(renamed.ends_with("new.txt"));
            assert!(!f.exists());
            assert!(renamed.exists());
            // Collision is refused.
            fs::write(d.path().join("taken.txt"), b"x").unwrap();
            assert!(rename(&renamed, "taken.txt").is_err());
            assert!(rename(&renamed, "..").is_err());
        }

        // --- phase: copy_into_recurses_and_dedups ---
        {
            let d = tmp();
            let src = d.path().join("dir");
            fs::create_dir(&src).unwrap();
            fs::write(src.join("inner.txt"), b"hello").unwrap();
            let destdir = d.path().join("dest");
            fs::create_dir(&destdir).unwrap();

            let copied = copy_into(&src, &destdir).unwrap();
            assert!(copied.join("inner.txt").exists());
            assert_eq!(fs::read(copied.join("inner.txt")).unwrap(), b"hello");
            // Original still present (copy, not move).
            assert!(src.join("inner.txt").exists());
            // A second copy into the same dir dedups the folder name.
            let copied2 = copy_into(&src, &destdir).unwrap();
            assert_eq!(copied2.file_name().unwrap().to_string_lossy(), "dir 2");
        }

        // --- phase: move_into_relocates ---
        {
            let d = tmp();
            let f = d.path().join("move-me.txt");
            fs::write(&f, b"x").unwrap();
            let destdir = d.path().join("dest");
            fs::create_dir(&destdir).unwrap();
            let moved = move_into(&f, &destdir).unwrap();
            assert!(moved.exists());
            assert!(!f.exists());
            assert_eq!(moved.parent().unwrap(), destdir);
        }

        // --- phase: trash_dir_is_the_xdg_home_trash_files_dir ---
        {
            // The Trash sidebar entry + the "am I in the trash?" check both hinge on
            // this path being the freedesktop home-trash files dir.
            if let Some(t) = trash_dir() {
                assert!(
                    t.ends_with("Trash/files"),
                    "trash dir is <data>/Trash/files: {t:?}"
                );
                assert!(is_trash_dir(&t), "the trash dir recognises itself");
                assert!(
                    !is_trash_dir(Path::new("/tmp/not-trash")),
                    "an unrelated dir is not the trash"
                );
            }
        }

        // --- phase: exhausted_destination_names_do_not_overwrite_existing_files ---
        {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            fs::write(&source, b"selected bytes").unwrap();
            fs::create_dir(&destination).unwrap();
            let occupied = destination.join("report.txt");
            fs::write(&occupied, b"existing bytes").unwrap();
            for suffix in 2..10_000 {
                fs::write(
                    destination.join(format!("report {suffix}.txt")),
                    b"suffix bytes",
                )
                .unwrap();
            }
            assert!(copy_into(&source, &destination).is_err());
            assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
            assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
            assert!(move_into(&source, &destination).is_err());
            assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
            assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 9_999);
            for suffix in 2..10_000 {
                assert_eq!(
                    fs::read(destination.join(format!("report {suffix}.txt"))).unwrap(),
                    b"suffix bytes"
                );
            }
        }
    }

    /// Copies and moves publish atomically despite racing roots and descendants, clean staging on failure and refuse destinations inside the source.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn copies_and_moves_publish_atomically_under_races() {
        // --- phase: copy_and_move_publish_after_a_racing_root_without_replacing_it ---
        {
            for remove_source in [false, true] {
                let directory = tmp();
                let source = directory.path().join("report.txt");
                let destination = directory.path().join("destination");
                fs::write(&source, b"selected bytes").unwrap();
                fs::create_dir(&destination).unwrap();

                let mut raced = false;
                let mut observer = TestTransferObserver {
                    force_staging: true,
                    before_publish: Some(Box::new(|candidate| {
                        if !raced {
                            raced = true;
                            fs::write(candidate, b"competitor bytes")?;
                        }
                        Ok(())
                    })),
                    ..Default::default()
                };
                let transferred =
                    transfer_into_with(&source, &destination, remove_source, &mut observer)
                        .unwrap();
                drop(observer);

                assert!(raced, "the test forced the post-selection collision");
                assert_eq!(transferred, destination.join("report 2.txt"));
                assert_eq!(
                    fs::read(destination.join("report.txt")).unwrap(),
                    b"competitor bytes"
                );
                assert_eq!(fs::read(&transferred).unwrap(), b"selected bytes");
                assert_eq!(source.exists(), !remove_source);
                assert_no_transfer_staging(&destination);
            }
        }

        // --- phase: direct_move_retries_when_the_selected_name_is_claimed ---
        {
            let directory = tmp();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            fs::write(&source, b"selected bytes").unwrap();
            fs::create_dir(&destination).unwrap();

            let mut raced = false;
            let mut observer = TestTransferObserver {
                before_publish: Some(Box::new(|candidate| {
                    if !raced {
                        raced = true;
                        fs::write(candidate, b"competitor bytes")?;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let moved = transfer_into_with(&source, &destination, true, &mut observer).unwrap();
            drop(observer);

            assert_eq!(moved, destination.join("report 2.txt"));
            assert_eq!(
                fs::read(destination.join("report.txt")).unwrap(),
                b"competitor bytes"
            );
            assert_eq!(fs::read(&moved).unwrap(), b"selected bytes");
            assert!(!source.exists());
        }

        // --- phase: racing_descendants_are_never_overwritten_by_directory_publication ---
        {
            let directory = tmp();
            let source = directory.path().join("project");
            let destination = directory.path().join("destination");
            let outside = directory.path().join("outside.txt");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("selected.txt"), b"selected bytes").unwrap();
            fs::write(&outside, b"outside bytes").unwrap();
            fs::create_dir(&destination).unwrap();

            let mut raced = false;
            let mut observer = TestTransferObserver {
                force_staging: true,
                before_publish: Some(Box::new(|candidate| {
                    if !raced {
                        raced = true;
                        fs::create_dir(candidate)?;
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(&outside, candidate.join("selected.txt"))?;
                        #[cfg(not(unix))]
                        fs::write(candidate.join("selected.txt"), b"competitor bytes")?;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let copied = transfer_into_with(&source, &destination, false, &mut observer).unwrap();
            drop(observer);

            assert_eq!(copied, destination.join("project 2"));
            #[cfg(unix)]
            assert_eq!(
                fs::read_link(destination.join("project/selected.txt")).unwrap(),
                outside
            );
            #[cfg(not(unix))]
            assert_eq!(
                fs::read(destination.join("project/selected.txt")).unwrap(),
                b"competitor bytes"
            );
            assert_eq!(
                fs::read(copied.join("selected.txt")).unwrap(),
                b"selected bytes"
            );
            assert_eq!(fs::read(&outside).unwrap(), b"outside bytes");
            assert_no_transfer_staging(&destination);
        }

        // --- phase: failed_recursive_copy_publishes_nothing_and_cleans_private_staging ---
        {
            fn failed_recursive_copy_publishes_nothing_and_cleans_private_staging() {
                let directory = tmp();
                let source = directory.path().join("project");
                let destination = directory.path().join("destination");
                fs::create_dir(&source).unwrap();
                fs::write(source.join("first.txt"), b"first bytes").unwrap();
                fs::write(source.join("second.txt"), b"second bytes").unwrap();
                fs::create_dir(&destination).unwrap();

                let root = source.clone();
                let mut visited_child = false;
                let mut observer = TestTransferObserver {
                    force_staging: true,
                    before_copy_source: Some(Box::new(move |path| {
                        if path != root {
                            if visited_child {
                                return Err(std::io::Error::other(
                                    "injected recursive copy failure",
                                ));
                            }
                            visited_child = true;
                        }
                        Ok(())
                    })),
                    ..Default::default()
                };
                let result = transfer_into_with(&source, &destination, false, &mut observer);

                assert!(result.is_err());
                assert!(source.join("first.txt").exists());
                assert!(source.join("second.txt").exists());
                assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
                assert_no_transfer_staging(&destination);
            }
            failed_recursive_copy_publishes_nothing_and_cleans_private_staging();
        }

        // --- phase: failed_cut_source_deletion_publishes_nothing_and_cleans_staging ---
        {
            let directory = tmp();
            let source = directory.path().join("project");
            let destination = directory.path().join("destination");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("first.txt"), b"first bytes").unwrap();
            fs::write(source.join("second.txt"), b"second bytes").unwrap();
            fs::create_dir(&destination).unwrap();

            let mut observer = TestTransferObserver {
                force_staging: true,
                before_source_remove: Some(Box::new(|quarantine| {
                    fs::remove_file(quarantine.join("first.txt"))?;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected partial source deletion failure",
                    ))
                })),
                ..Default::default()
            };
            let result = transfer_into_with(&source, &destination, true, &mut observer);

            assert!(result.is_err());
            assert_eq!(fs::read(source.join("first.txt")).unwrap(), b"first bytes");
            assert_eq!(
                fs::read(source.join("second.txt")).unwrap(),
                b"second bytes"
            );
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_no_transfer_staging(&destination);
            assert_no_source_quarantine(directory.path());

            let retried = move_into(&source, &destination).unwrap();
            assert_eq!(retried, destination.join("project"));
            assert_eq!(fs::read(retried.join("first.txt")).unwrap(), b"first bytes");
            assert_eq!(
                fs::read(retried.join("second.txt")).unwrap(),
                b"second bytes"
            );
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        }

        // --- phase: recursive_copy_and_move_refuse_a_destination_inside_the_source ---
        {
            let d = tmp();
            let source = d.path().join("source");
            let nested_destination = source.join("nested");
            fs::create_dir_all(&nested_destination).unwrap();
            fs::write(source.join("file.txt"), b"x").unwrap();

            assert!(copy_into(&source, &nested_destination).is_err());
            assert!(move_into(&source, &nested_destination).is_err());
            assert!(
                source.join("file.txt").exists(),
                "refusal has no side effect"
            );
        }
    }

    /// Symlink swaps are refused without traversal, symlinks and dangling entries are preserved, and metadata failures propagate.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[cfg(unix)]
    #[test]
    fn symlinks_and_metadata_failures_are_handled_without_following_links() {
        // --- phase: source_swap_to_symlink_is_refused_without_reading_its_target ---
        {
            use std::os::unix::fs::symlink;

            let directory = tmp();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            let outside = directory.path().join("outside.txt");
            fs::write(&source, b"selected bytes").unwrap();
            fs::write(&outside, b"outside secret").unwrap();
            fs::create_dir(&destination).unwrap();

            let selected = source.clone();
            let target = outside.clone();
            let mut swapped = false;
            let mut observer = TestTransferObserver {
                force_staging: true,
                before_copy_source: Some(Box::new(move |path| {
                    if path == selected && !swapped {
                        swapped = true;
                        fs::remove_file(path)?;
                        symlink(&target, path)?;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let result = transfer_into_with(&source, &destination, false, &mut observer);

            assert!(result.is_err());
            assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
            assert_eq!(fs::read_link(&source).unwrap(), outside);
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_no_transfer_staging(&destination);
        }

        // --- phase: source_directory_swap_to_symlink_is_refused_without_traversal ---
        {
            use std::os::unix::fs::symlink;

            let directory = tmp();
            let source = directory.path().join("project");
            let saved_source = directory.path().join("saved-project");
            let destination = directory.path().join("destination");
            let outside = directory.path().join("outside");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("secret.txt"), b"outside secret").unwrap();
            fs::create_dir(&destination).unwrap();

            let selected = source.clone();
            let saved = saved_source.clone();
            let target = outside.clone();
            let mut swapped = false;
            let mut observer = TestTransferObserver {
                force_staging: true,
                before_copy_source: Some(Box::new(move |path| {
                    if path == selected && !swapped {
                        swapped = true;
                        fs::rename(path, &saved)?;
                        symlink(&target, path)?;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let result = transfer_into_with(&source, &destination, false, &mut observer);

            assert!(result.is_err());
            assert_eq!(
                fs::read(outside.join("secret.txt")).unwrap(),
                b"outside secret"
            );
            assert!(saved_source.is_dir());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_no_transfer_staging(&destination);
        }

        // --- phase: copy_preserves_symlinks_without_reading_their_targets ---
        {
            use std::os::unix::fs::symlink;

            let d = tmp();
            let source = d.path().join("source");
            let destination = d.path().join("destination");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&destination).unwrap();
            // This deliberately points outside the selected source tree. A copy
            // must reproduce the link rather than dereferencing `/etc/passwd`.
            symlink("/etc/passwd", source.join("outside-link")).unwrap();
            symlink("missing-target", source.join("dangling-link")).unwrap();

            let copied = copy_into(&source, &destination).unwrap();
            let outside = copied.join("outside-link");
            let dangling = copied.join("dangling-link");
            assert!(
                fs::symlink_metadata(&outside)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read_link(&outside).unwrap(),
                PathBuf::from("/etc/passwd")
            );
            assert!(
                fs::symlink_metadata(&dangling)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read_link(&dangling).unwrap(),
                PathBuf::from("missing-target")
            );
        }

        // --- phase: copy_and_move_propagate_destination_metadata_failure ---
        {
            use std::os::unix::fs::symlink;
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            fs::write(&source, b"selected bytes").unwrap();
            symlink("destination", &destination).unwrap();
            let candidate = destination.join("report.txt");
            let metadata_error = fs::symlink_metadata(&candidate).unwrap_err();
            let expected = crate::i18n::t_args(
                "files-destination-inspect-failed",
                &[
                    crate::i18n::arg("path", candidate.to_string_lossy().into_owned()),
                    crate::i18n::arg("error", metadata_error.to_string()),
                ],
            );
            for result in [
                copy_into(&source, &destination),
                move_into(&source, &destination),
            ] {
                let error = result.unwrap_err();
                assert_eq!(error, expected);
                assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
                assert_eq!(
                    fs::read_link(&destination).unwrap(),
                    PathBuf::from("destination")
                );
            }
        }

        // --- phase: copy_and_move_preserve_dangling_destination_entries ---
        {
            use std::os::unix::fs::symlink;

            for move_source in [false, true] {
                for numbered_collision in [false, true] {
                    let directory = tempfile::tempdir().unwrap();
                    let source = directory.path().join("report.txt");
                    let destination = directory.path().join("destination");
                    let outside = directory.path().join("unselected-target.txt");
                    fs::write(&source, b"selected bytes").unwrap();
                    fs::create_dir(&destination).unwrap();
                    let occupied = destination.join("report.txt");
                    let (link, expected) = if numbered_collision {
                        fs::write(&occupied, b"existing bytes").unwrap();
                        (
                            destination.join("report 2.txt"),
                            destination.join("report 3.txt"),
                        )
                    } else {
                        (occupied.clone(), destination.join("report 2.txt"))
                    };
                    symlink(&outside, &link).unwrap();
                    let result = if move_source {
                        move_into(&source, &destination)
                    } else {
                        copy_into(&source, &destination)
                    }
                    .unwrap();
                    assert_eq!(result, expected);
                    assert_eq!(fs::read(&result).unwrap(), b"selected bytes");
                    assert_eq!(fs::read_link(&link).unwrap(), outside);
                    assert!(
                        !outside.exists(),
                        "copy must not write through the destination link"
                    );
                    assert_eq!(source.exists(), !move_source);
                    if !move_source {
                        assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
                    }
                    if numbered_collision {
                        assert_eq!(fs::read(&occupied).unwrap(), b"existing bytes");
                    }
                }
            }
        }
    }
}
