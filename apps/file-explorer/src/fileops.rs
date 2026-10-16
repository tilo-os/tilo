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
pub(crate) fn leaf_name(name: &str) -> Result<&str, String> {
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
    host_fs::trash::ensure_trash_dir().ok()
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
    host_fs::trash::delete(paths)
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

/// Copy a mounted source into an independent, owner-writable local file.
/// Permissions are applied to newly created files in private staging, never
/// to the source or a published destination. Directories retain their modes.
///
/// # Errors
/// Returns an error if staging, copying or publication fails.
pub fn import_mounted_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, false, &mut MountedImportObserver)
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

/// Portable source capabilities passed to host admission hooks.
pub use cap_std::fs::{Dir as SourceDirectory, OpenOptions as SourceOpenOptions};

/// Host admission around the shared transfer algorithm. Paths supplied here
/// are presentation only; a hosting caller must supply descriptor-rooted
/// paths and revalidate its own authority at these boundaries.
pub trait TransferObserver {
    /// Open a leaf of a retained source directory. A host may implement its
    /// platform's nonblocking open without putting platform flags in the app.
    ///
    /// # Errors
    /// Returns a source opening or admission failure.
    fn open_source(
        &mut self,
        parent: &SourceDirectory,
        name: &std::ffi::OsStr,
        options: &SourceOpenOptions,
    ) -> std::io::Result<std::fs::File> {
        parent
            .open_with(name, options)
            .map(cap_std::fs::File::into_std)
    }

    /// Retain the staged root identity before any Cut deletion or publication.
    ///
    /// # Errors
    /// Refuses the transfer if its staged receipt cannot be retained.
    fn staged_source(&mut self, _staging: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Persist recovery intent before the source is sequestered.
    ///
    /// # Errors
    /// Refuses destructive I/O when its write-ahead receipt cannot be saved.
    fn before_source_quarantine(
        &mut self,
        _source: &Path,
        _quarantine: &Path,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn allow_direct_rename(&self) -> bool {
        true
    }

    /// Check live admission before reading each source entry.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_copy_source(&mut self, _source: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Check live admission immediately before publication.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_publish(&mut self, _destination: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Check live admission immediately before deleting a quarantined source.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_source_remove(&mut self, _source: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Revalidate current authority before restoring a Cut source.
    ///
    /// # Errors
    /// Refuses restoration when source or retained staging authority is lost.
    fn before_source_restore(&mut self, _source: &Path, _staged: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Remove the already-admitted quarantined source.
    ///
    /// # Errors
    /// Reports a filesystem removal failure, including partial deletion.
    fn remove_source(&mut self, source: &Path) -> std::io::Result<()> {
        remove_path(source)
    }

    /// Validate the sequestered leaf before any destructive step.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn after_source_quarantine(&mut self, _source: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Validate the actual opened descriptor before reading its contents.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn opened_source(&mut self, _path: &Path, _source: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    /// Optionally provide a target captured from the exact retained symlink.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn symlink_target(&mut self, _path: &Path) -> std::io::Result<Option<PathBuf>> {
        Ok(None)
    }

    /// Choose permissions for a freshly created destination file in private
    /// staging. Ordinary transfers preserve the source permissions.
    fn destination_permissions(&mut self, source: std::fs::Permissions) -> std::fs::Permissions {
        source
    }

    /// Copy bytes while observing cancellation and live admission.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn copy_bytes(
        &mut self,
        source: &mut std::fs::File,
        destination: &mut std::fs::File,
    ) -> std::io::Result<u64> {
        std::io::copy(source, destination)
    }
}

struct NoopTransferObserver;
impl TransferObserver for NoopTransferObserver {}

struct MountedImportObserver;
impl TransferObserver for MountedImportObserver {
    fn destination_permissions(&mut self, source: std::fs::Permissions) -> std::fs::Permissions {
        tilo_cloud_files::owner_writable(source)
    }
}

/// The same copy/cut staging and recovery with a host's live admission hooks.
/// This grants no filesystem access by itself.
///
/// # Errors
/// Refuses invalid destinations, collisions, I/O failures or a failed admission hook.
pub fn transfer_into_with(
    src: &Path,
    dest_dir: &Path,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .ok_or_else(|| crate::i18n::t("files-source-no-name"))?;
    transfer_into_named_with(src, dest_dir, name, remove_source, observer)
}

/// Transfer to one explicit leaf name with the same live admission and recovery.
/// This grants no filesystem access and never replaces an occupied destination.
///
/// # Errors
/// Refuses invalid names, lost authority, collisions and I/O failures.
pub fn transfer_into_named_with(
    src: &Path,
    dest_dir: &Path,
    name: &std::ffi::OsStr,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, String> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(crate::i18n::t("files-invalid-destination-name"));
    }
    ensure_destination_is_outside_source(src, dest_dir)?;
    let source_parent = src
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let destination_dir = std::fs::canonicalize(dest_dir).ok();
    if remove_source && source_parent.is_some() && source_parent == destination_dir {
        return Err("the item is already in this folder".into());
    }

    let names = DestinationNames::new(name);
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
    observer
        .staged_source(&staging.path)
        .map_err(|error| staging.error_with_cleanup(error.to_string()))?;
    if remove_source {
        // From sequestration onward, unwinding must retain the complete copy:
        // the source may already be partially or wholly deleted. Normal error
        // and success paths explicitly restore or clean this container.
        staging.retain();
        let quarantine = match quarantine_source(src, observer) {
            Ok(path) => path,
            Err(error) => return Err(staging.error_with_cleanup(error.to_string())),
        };
        if let Err(error) = observer
            .after_source_quarantine(&quarantine)
            .and_then(|()| observer.before_source_remove(&quarantine))
        {
            return Err(rollback_quarantined_source(
                src,
                &quarantine,
                &staging,
                observer,
                error.to_string(),
            ));
        }
        let deletion =
            host_fs::mark_private(&quarantine).and_then(|()| observer.remove_source(&quarantine));
        if let Err(error) = deletion {
            return Err(restore_failed_cut(
                src,
                &quarantine,
                &staging,
                error.to_string(),
                observer,
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
            observer,
        )),
        Err(error) => Err(staging.error_with_cleanup(error)),
    }
}

fn quarantine_source(src: &Path, observer: &mut impl TransferObserver) -> std::io::Result<PathBuf> {
    let parent = src
        .parent()
        .ok_or_else(|| std::io::Error::other("source has no parent directory"))?;
    for _ in 0..32 {
        let quarantine = parent.join(format!(".tilo-transfer-source-{}", uuid::Uuid::new_v4()));
        observer.before_source_quarantine(src, &quarantine)?;
        match rename_noreplace(src, &quarantine) {
            Ok(()) => return Ok(quarantine),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate private source quarantine",
    ))
}

fn rollback_quarantined_source(
    source: &Path,
    quarantine: &Path,
    staging: &StagingPath,
    observer: &mut impl TransferObserver,
    original_error: String,
) -> String {
    match observer
        .before_source_restore(source, quarantine)
        .and_then(|()| rename_noreplace(quarantine, source))
    {
        Ok(()) => staging.error_with_cleanup(original_error),
        Err(error) => {
            staging.retain();
            format!(
                "{original_error}; source retained at {}: {error}; staged data retained at {}",
                quarantine.display(),
                staging.path.display()
            )
        }
    }
}

struct RecoveryObserver<'a, T> {
    observer: &'a mut T,
    source: &'a Path,
    staged: &'a Path,
}

impl<T: TransferObserver> TransferObserver for RecoveryObserver<'_, T> {
    fn before_copy_source(&mut self, _: &Path) -> std::io::Result<()> {
        self.observer
            .before_source_restore(self.source, self.staged)
    }

    fn open_source(
        &mut self,
        parent: &SourceDirectory,
        name: &std::ffi::OsStr,
        options: &SourceOpenOptions,
    ) -> std::io::Result<std::fs::File> {
        self.observer.open_source(parent, name, options)
    }

    fn copy_bytes(
        &mut self,
        source: &mut std::fs::File,
        destination: &mut std::fs::File,
    ) -> std::io::Result<u64> {
        use std::io::{Read as _, Write as _};
        let mut buffer = vec![0; 256 * 1024];
        let mut total = 0_u64;
        loop {
            self.observer
                .before_source_restore(self.source, self.staged)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(total);
            }
            destination.write_all(
                buffer
                    .get(..count)
                    .ok_or_else(|| std::io::Error::other("invalid recovery read length"))?,
            )?;
            total = total.saturating_add(u64::try_from(count).map_err(std::io::Error::other)?);
        }
    }
}

fn restore_failed_cut(
    source: &Path,
    quarantine: &Path,
    staging: &StagingPath,
    original_error: String,
    observer: &mut impl TransferObserver,
) -> String {
    let mut recovery = RecoveryObserver {
        observer,
        source,
        staged: &staging.path,
    };
    let restored = recovery
        .before_copy_source(&staging.path)
        .and_then(|()| copy_tree_exclusive(&staging.path, source, &mut recovery));
    match restored {
        Ok(()) => {
            if let Err(error) = observer.before_source_restore(source, &staging.path) {
                staging.retain();
                return format!(
                    "{original_error}; recovery cleanup refused: {error}; staged data retained at {}",
                    staging.path.display()
                );
            }
            let quarantine_error = if quarantine.as_os_str().is_empty() {
                None
            } else {
                remove_path(quarantine).err()
            };
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
    original: std::ffi::OsString,
    stem: std::ffi::OsString,
    extension: Option<std::ffi::OsString>,
}

impl DestinationNames {
    fn new(name: &std::ffi::OsStr) -> Self {
        let path = Path::new(name);
        Self {
            original: name.to_os_string(),
            stem: path.file_stem().unwrap_or_default().to_os_string(),
            extension: path.extension().map(std::ffi::OsStr::to_os_string),
        }
    }

    fn for_suffix(&self, suffix: u32) -> std::ffi::OsString {
        if suffix == 1 {
            return self.original.clone();
        }
        let mut name = self.stem.clone();
        name.push(format!(" {suffix}"));
        if let Some(extension) = &self.extension {
            name.push(".");
            name.push(extension);
        }
        name
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
            crate::i18n::arg("name", names.original.to_string_lossy().into_owned()),
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
        if self.retain.get() && std::thread::panicking() {
            tracing::warn!(
                path = %self.path.display(),
                "Files Cut worker unwound; complete recovery copy retained"
            );
        }
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
    use cap_fs_ext::DirExt as _;

    let receiving = cap_std::fs::Dir::open_ambient_dir(dest_dir, cap_std::ambient_authority())?;
    for _ in 0..32 {
        let container = dest_dir.join(format!(".tilo-transfer-{}", uuid::Uuid::new_v4()));
        match host_fs::create_private_dir(&container) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        let path = container.join("payload");
        let name = container
            .file_name()
            .ok_or_else(|| std::io::Error::other("private transfer staging has no leaf name"))?;
        let copied = receiving.open_dir_nofollow(name).and_then(|directory| {
            copy_tree_to_directory(src, &directory, Path::new("payload"), observer)
        });
        match copied {
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
    let parent = dest
        .parent()
        .ok_or_else(|| std::io::Error::other("destination has no parent directory"))?;
    let name = dest
        .file_name()
        .ok_or_else(|| std::io::Error::other("destination has no file name"))?;
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    copy_tree_to_directory(src, &directory, Path::new(name), observer)
}

fn copy_tree_to_directory(
    src: &Path,
    destination: &cap_std::fs::Dir,
    name: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    let parent_path = src
        .parent()
        .ok_or_else(|| std::io::Error::other("source has no parent directory"))?;
    let source_name = src
        .file_name()
        .ok_or_else(|| std::io::Error::other("source has no file name"))?;
    let parent = cap_std::fs::Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    let destination = CopyDestination {
        root: destination,
        path: name.to_path_buf(),
    };
    copy_capability_entry(&parent, source_name, src, &destination, observer)
}

struct CopyDestination<'a> {
    root: &'a cap_std::fs::Dir,
    path: PathBuf,
}

impl CopyDestination<'_> {
    fn parent(&self) -> std::io::Result<(cap_std::fs::Dir, &std::ffi::OsStr)> {
        use cap_fs_ext::DirExt as _;

        let name = self
            .path
            .file_name()
            .ok_or_else(|| std::io::Error::other("destination has no leaf"))?;
        let mut directory = self.root.try_clone()?;
        if let Some(parent) = self.path.parent() {
            for component in parent.components() {
                let Component::Normal(name) = component else {
                    return Err(std::io::Error::other("destination escaped staging"));
                };
                directory = directory.open_dir_nofollow(name)?;
            }
        }
        Ok((directory, name))
    }

    fn child(&self, name: &std::ffi::OsStr) -> CopyDestination<'_> {
        CopyDestination {
            root: self.root,
            path: self.path.join(name),
        }
    }
}

fn copy_capability_entry(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    display_path: &Path,
    dest: &CopyDestination<'_>,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};

    let initial = parent.symlink_metadata(name)?;
    observer.before_copy_source(display_path)?;
    let (receiving, destination_name) = dest.parent()?;
    if initial.file_type().is_symlink() {
        let target = match observer.symlink_target(display_path)? {
            Some(target) => target,
            None => parent.read_link_contents(name)?,
        };
        return host_fs::symlink_at(&target, &receiving.into_std_file(), destination_name);
    }

    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true);
    let opened = observer.open_source(parent, name, &options)?;
    observer.opened_source(display_path, &opened)?;
    let metadata = opened.metadata()?;

    if metadata.is_dir() {
        receiving.create_dir(destination_name)?;
        let created = receiving.open_dir_nofollow(destination_name)?;
        let source = cap_std::fs::Dir::from_std_file(opened);
        for entry in source.entries()? {
            let entry = entry?;
            let child_name = entry.file_name();
            copy_capability_entry(
                &source,
                &child_name,
                &display_path.join(&child_name),
                &dest.child(&child_name),
                observer,
            )?;
        }
        created.set_permissions(
            ".",
            cap_std::fs::Permissions::from_std(metadata.permissions()),
        )?;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(std::io::Error::other("source type changed during copy"));
    }

    let mut source = opened;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut destination = receiving.open_with(destination_name, &options)?.into_std();
    observer.copy_bytes(&mut source, &mut destination)?;
    destination.set_permissions(observer.destination_permissions(metadata.permissions()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn opening_empty_trash_creates_owner_only_directories() {
        use std::os::unix::fs::MetadataExt as _;
        let _home = tilo_test_env::TestHome::new().expect("private test home");
        let files = ensure_trash_dir().expect("opening empty Trash");
        let trash = files.parent().expect("Trash root");
        let owner = fs::metadata(trash.parent().expect("data directory"))
            .expect("data metadata")
            .uid();
        let info = trash.join("info");
        for directory in [trash, files.as_path(), info.as_path()] {
            let metadata = fs::symlink_metadata(directory).expect("created Trash directory");
            assert!(metadata.is_dir());
            assert_eq!(metadata.uid(), owner);
            assert_eq!(metadata.mode() & 0o777, 0o700, "{}", directory.display());
        }
        assert_eq!(ensure_trash_dir().as_deref(), Some(files.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn opening_trash_does_not_rewrite_existing_directory_permissions() {
        use std::os::unix::fs::MetadataExt as _;
        let _home = tilo_test_env::TestHome::new().expect("private test home");
        let files = trash_dir().expect("test Trash path");
        fs::create_dir_all(&files).expect("existing nonprivate Trash fixture");
        let trash = files.parent().expect("Trash root");
        for directory in [trash, files.as_path()] {
            host_fs::set_mode(directory, 0o755).expect("explicit nonprivate fixture");
        }
        assert!(
            ensure_trash_dir().is_none(),
            "nonprivate Trash must be refused"
        );
        #[cfg(not(target_os = "macos"))]
        {
            let source = trash.parent().expect("data directory").join("notes.txt");
            fs::write(&source, b"keep private contents").expect("refused deletion source");
            let error = delete_to_trash(std::slice::from_ref(&source))
                .expect_err("deletion must refuse nonprivate Trash");
            assert!(error.contains("not a private directory owned by this user"));
            assert_eq!(
                fs::read(&source).expect("source retained"),
                b"keep private contents"
            );
            assert_eq!(
                fs::read_dir(&files)
                    .expect("no published private contents")
                    .count(),
                0
            );
        }
        for directory in [trash, files.as_path()] {
            assert_eq!(
                fs::symlink_metadata(directory)
                    .expect("untouched fixture")
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn legacy_delete_creates_owner_only_trash_metadata_and_payload_directories() {
        use std::os::unix::fs::MetadataExt as _;
        let home = tilo_test_env::TestHome::new().expect("private test home");
        let source = home.home().join("notes.txt");
        fs::write(&source, b"private notes").expect("source contents");
        delete_to_trash(std::slice::from_ref(&source)).expect("production legacy delete");
        assert!(!source.exists());
        for directory in ["Trash", "Trash/files", "Trash/info"] {
            let path = home.data().join(directory);
            let metadata = fs::symlink_metadata(&path).expect("created Trash directory");
            assert!(metadata.is_dir());
            assert_eq!(metadata.mode() & 0o777, 0o700, "{}", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn opening_trash_refuses_a_symlink_without_populating_its_target() {
        let _home = tilo_test_env::TestHome::new().expect("private test home");
        let outside = tempfile::tempdir().expect("unrelated directory");
        // The Trash root lives under the platform data dir, which is not
        // `$XDG_DATA_HOME` everywhere (macOS uses `~/Library/Application Support`).
        let files = trash_dir().expect("test Trash path");
        let trash = files.parent().expect("Trash root");
        fs::create_dir_all(trash.parent().expect("data directory")).expect("data directory");
        host_fs::symlink(outside.path(), trash).expect("symlink fixture");
        assert!(ensure_trash_dir().is_none());
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("untouched target")
                .count(),
            0
        );
    }

    type TestHook<'a> = Box<dyn FnMut(&Path) -> std::io::Result<()> + 'a>;

    #[derive(Default)]
    struct TestTransferObserver<'a> {
        force_staging: bool,
        before_copy_source: Option<TestHook<'a>>,
        before_publish: Option<TestHook<'a>>,
        before_source_remove: Option<TestHook<'a>>,
        remove_source: Option<TestHook<'a>>,
        after_source_quarantine: Option<TestHook<'a>>,
        staged_source: Option<TestHook<'a>>,
        before_source_restore: Option<TestHook<'a>>,
    }

    impl TransferObserver for TestTransferObserver<'_> {
        fn before_source_restore(&mut self, source: &Path, _: &Path) -> std::io::Result<()> {
            self.before_source_restore
                .as_mut()
                .map_or(Ok(()), |hook| hook(source))
        }

        fn remove_source(&mut self, source: &Path) -> std::io::Result<()> {
            match &mut self.remove_source {
                Some(hook) => hook(source),
                None => remove_path(source),
            }
        }

        fn staged_source(&mut self, source: &Path) -> std::io::Result<()> {
            self.staged_source
                .as_mut()
                .map_or(Ok(()), |hook| hook(source))
        }
        fn after_source_quarantine(&mut self, source: &Path) -> std::io::Result<()> {
            self.after_source_quarantine
                .as_mut()
                .map_or(Ok(()), |hook| hook(source))
        }

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

    #[cfg(unix)]
    #[test]
    fn clipboard_recursive_copy_refuses_replaced_destination_descendant() {
        for replaced_relative in ["payload/nested", "payload", ""] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let destination = root.path().join("destination");
            let outside = root.path().join("outside");
            fs::create_dir_all(source.join("nested")).unwrap();
            fs::write(source.join("nested/report.txt"), b"original").unwrap();
            fs::create_dir(&destination).unwrap();
            fs::create_dir(&outside).unwrap();
            let target = outside.clone();
            let receiving = destination.clone();
            let mut replaced = false;
            let mut observer = TestTransferObserver {
                force_staging: true,
                before_copy_source: Some(Box::new(|path| {
                    if path.file_name() == Some(std::ffi::OsStr::new("report.txt")) {
                        let container = fs::read_dir(&receiving)?.next().unwrap()?.path();
                        let replaced_path = if replaced_relative.is_empty() {
                            container
                        } else {
                            container.join(replaced_relative)
                        };
                        fs::remove_dir_all(&replaced_path)?;
                        std::os::unix::fs::symlink(&target, &replaced_path)?;
                        replaced = true;
                    }
                    Ok(())
                })),
                ..Default::default()
            };
            let result = transfer_into_with(&source, &destination, false, &mut observer);
            drop(observer);
            assert!(
                replaced,
                "fault must exercise an existing destination descendant"
            );
            assert!(
                !outside.join("report.txt").exists(),
                "copy must never write through a replaced staging descendant"
            );
            assert!(
                result.is_err(),
                "replaced destination descendant must be refused"
            );
            assert_eq!(
                fs::read(source.join("nested/report.txt")).unwrap(),
                b"original"
            );
        }
    }

    #[test]
    fn clipboard_cut_lost_restoration_authority_retains_complete_recovery() {
        for refused_at in ["quarantine", "delete", "publish"] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("report.txt");
            let destination = root.path().join("destination");
            fs::write(&source, b"original").unwrap();
            fs::create_dir(&destination).unwrap();
            let staged = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
            let recorded = staged.clone();
            let mut observer = TestTransferObserver {
                force_staging: true,
                staged_source: Some(Box::new(move |path| {
                    *recorded.borrow_mut() = Some(path.to_path_buf());
                    Ok(())
                })),
                after_source_quarantine: Some(Box::new(|_| {
                    if refused_at == "quarantine" {
                        Err(std::io::Error::other("grant revoked"))
                    } else {
                        Ok(())
                    }
                })),
                before_source_remove: Some(Box::new(|_| {
                    if refused_at == "delete" {
                        Err(std::io::Error::other("grant revoked"))
                    } else {
                        Ok(())
                    }
                })),
                before_publish: Some(Box::new(|_| Err(std::io::Error::other("grant revoked")))),
                before_source_restore: Some(Box::new(|_| {
                    Err(std::io::Error::other("no current source grant"))
                })),
                ..Default::default()
            };
            let error = transfer_into_with(&source, &destination, true, &mut observer).unwrap_err();
            assert!(
                !source.exists(),
                "lost authority must not be bypassed by restoration"
            );
            assert!(error.contains("no current source grant"));
            let recovery = staged.borrow().clone().expect("actual staging receipt");
            assert_eq!(fs::read(recovery).unwrap(), b"original");
            let quarantines: Vec<_> = fs::read_dir(root.path())
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".tilo-transfer-source-")
                })
                .collect();
            assert_eq!(quarantines.len(), usize::from(refused_at != "publish"));
            for entry in quarantines {
                assert_eq!(fs::read(entry.path()).unwrap(), b"original");
            }
        }
    }

    #[test]
    fn clipboard_worker_unwind_after_cut_deletion_retains_complete_recovery() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("report.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let staged = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let recorded = staged.clone();
        let mut observer = TestTransferObserver {
            force_staging: true,
            staged_source: Some(Box::new(move |path| {
                *recorded.borrow_mut() = Some(path.to_path_buf());
                Ok(())
            })),
            before_publish: Some(Box::new(|_| {
                panic!("injected worker failure after deletion")
            })),
            ..TestTransferObserver::default()
        };
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                transfer_into_with(&source, &destination, true, &mut observer)
            }))
            .is_err()
        );
        assert!(
            !source.exists(),
            "the fault exercised the post-deletion boundary"
        );
        let recovery = staged
            .borrow()
            .clone()
            .expect("owner received staging location");
        assert_eq!(fs::read(&recovery).unwrap(), b"original");
        assert_no_source_quarantine(root.path());
    }

    #[test]
    fn clipboard_cancel_before_deletion_restores_exact_source_identity() {
        let root = tmp();
        let source = root.path().join("draft.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let mut observer = TestTransferObserver {
            force_staging: true,
            before_source_remove: Some(Box::new(|_| Err(std::io::Error::other("cancelled")))),
            ..Default::default()
        };
        let error = transfer_into_with(&source, &destination, true, &mut observer).unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let current = fs::metadata(&source).unwrap();
            assert_eq!(
                (current.dev(), current.ino(), current.mode()),
                (metadata.dev(), metadata.ino(), metadata.mode())
            );
        }
        #[cfg(not(unix))]
        let _ = metadata;
        assert_no_source_quarantine(root.path());
    }

    #[test]
    fn clipboard_receipt_refusal_keeps_cut_source_and_cleans_staging() {
        let root = tmp();
        let source = root.path().join("draft.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let mut observer = TestTransferObserver {
            force_staging: true,
            staged_source: Some(Box::new(|staged| {
                assert_eq!(fs::read(staged)?, b"original");
                Err(std::io::Error::other("receipt unavailable"))
            })),
            ..Default::default()
        };
        let error = transfer_into_with(&source, &destination, true, &mut observer).unwrap_err();
        assert!(error.contains("receipt unavailable"));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_source_quarantine(root.path());
    }

    #[test]
    fn clipboard_quarantine_refusal_restores_original_without_publication() {
        let root = tmp();
        let source = root.path().join("draft.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let mut observer = TestTransferObserver {
            force_staging: true,
            after_source_quarantine: Some(Box::new(|quarantined| {
                assert_eq!(fs::read(quarantined)?, b"original");
                Err(std::io::Error::other("source grant revoked"))
            })),
            ..Default::default()
        };
        let error = transfer_into_with(&source, &destination, true, &mut observer).unwrap_err();
        assert!(error.contains("source grant revoked"));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_source_quarantine(root.path());
    }

    #[test]
    fn clipboard_source_replacement_is_restored_without_deleting_either_file() {
        let root = tmp();
        let source = root.path().join("draft.txt");
        let saved = root.path().join("saved.txt");
        let destination = root.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        let replace = source.clone();
        let retain = saved.clone();
        let mut observer = TestTransferObserver {
            force_staging: true,
            staged_source: Some(Box::new(move |staged| {
                assert_eq!(fs::read(staged)?, b"original");
                fs::rename(&replace, &retain)?;
                fs::write(&replace, b"replacement")
            })),
            after_source_quarantine: Some(Box::new(|quarantined| {
                assert_eq!(fs::read(quarantined)?, b"replacement");
                Err(std::io::Error::other("selected source identity changed"))
            })),
            ..Default::default()
        };
        let error = transfer_into_with(&source, &destination, true, &mut observer).unwrap_err();
        assert!(error.contains("identity changed"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
                "refusing a replaced source must not privatize its permissions"
            );
        }
        assert_eq!(fs::read(&source).unwrap(), b"replacement");
        assert_eq!(fs::read(&saved).unwrap(), b"original");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_no_source_quarantine(root.path());
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
                remove_source: Some(Box::new(|quarantine| {
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
