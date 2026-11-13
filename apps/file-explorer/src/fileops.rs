//! The disk work behind the edit commands: names, Trash, copying, moving and
//! imports.

use std::path::{Component, Path, PathBuf};

/// Accept one portable filesystem leaf name, never a path expression: `.` and
/// `..` would let a supposedly local operation address an ancestor directory.
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

pub(crate) fn readable_error(reason: &str) -> String {
    let reason = reason.trim();
    let Some((text, code)) = os_error(reason) else {
        return reason.to_owned();
    };
    let key = match code {
        1 | 13 => Some("files-error-permission"),
        2 => Some("files-error-not-found"),
        16 => Some("files-error-busy"),
        17 => Some("files-error-exists"),
        28 | 122 => Some("files-error-no-space"),
        30 => Some("files-error-read-only"),
        39 => Some("files-error-not-empty"),
        _ => None,
    };
    key.map_or_else(|| ui::format::capitalize_first(text), crate::i18n::t)
}

fn os_error(reason: &str) -> Option<(&str, i32)> {
    let (text, code) = reason
        .trim()
        .strip_suffix(')')?
        .rsplit_once(" (os error ")?;
    Some((text, code.parse().ok()?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderFailure {
    Permission,
    Missing,
    Other,
}

impl FolderFailure {
    pub(crate) fn of(reason: &str) -> Self {
        match os_error(reason).map(|(_, code)| code) {
            Some(1 | 13) => Self::Permission,
            Some(2 | 20) => Self::Missing,
            _ => Self::Other,
        }
    }

    /// Whether Try again can help: not while the cause stays.
    pub(crate) fn may_pass(self) -> bool {
        self == Self::Other
    }
}

pub use host_fs::trash::{is_trash_dir, trash_dir};

pub fn ensure_trash_dir() -> Option<PathBuf> {
    host_fs::trash::ensure_trash_dir().ok()
}

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

pub fn empty_trash() -> Result<usize, String> {
    host_fs::trash::empty()
}

pub fn restore_entries(entries: &[PathBuf]) -> Result<usize, String> {
    host_fs::trash::restore(entries)
}

pub fn purge_entries(entries: &[PathBuf]) -> Result<usize, String> {
    host_fs::trash::purge(entries)
}

pub fn unique_destination(dir: &Path, name: &str) -> Result<PathBuf, String> {
    match host_fs::transfer::free_name(dir, std::ffi::OsStr::new(name)) {
        Ok(Some(candidate)) => Ok(candidate),
        Ok(None) => Err(crate::i18n::t_args(
            "files-destination-names-exhausted",
            &[
                crate::i18n::arg("name", name),
                crate::i18n::arg("path", dir.to_string_lossy().into_owned()),
            ],
        )),
        Err(failure) => Err(refusal(failure)),
    }
}

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

pub fn delete_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    host_fs::trash::delete(paths)
}

pub fn move_to_trash(path: &Path) -> Result<Option<PathBuf>, String> {
    host_fs::trash::trash(path)
}

pub fn copy_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, false, &mut NoopTransferObserver)
}

pub fn import_mounted_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, false, &mut MountedImportObserver)
}

pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    transfer_into_with(src, dest_dir, true, &mut NoopTransferObserver)
}

pub(crate) fn move_into_named(
    src: &Path,
    dest_dir: &Path,
    name: &std::ffi::OsStr,
) -> Result<PathBuf, String> {
    transfer_into_named_with(src, dest_dir, name, true, &mut NoopTransferObserver)
}

pub use host_fs::transfer::{SourceDirectory, SourceOpenOptions, TransferObserver};

struct NoopTransferObserver;
impl TransferObserver for NoopTransferObserver {}

struct MountedImportObserver;
impl TransferObserver for MountedImportObserver {
    fn destination_permissions(&mut self, source: std::fs::Permissions) -> std::fs::Permissions {
        tilo_cloud_files::owner_writable(source)
    }
}

fn refusal(failure: host_fs::transfer::Failure) -> String {
    use host_fs::transfer::Failure;
    match failure {
        Failure::InvalidName => crate::i18n::t("files-invalid-destination-name"),
        Failure::NoSourceName => crate::i18n::t("files-source-no-name"),
        Failure::Inspect { path, error } => destination_inspection_error(&path, &error),
        Failure::NamesExhausted { name, dir } => crate::i18n::t_args(
            "files-destination-names-exhausted",
            &[
                crate::i18n::arg("name", name.to_string_lossy().into_owned()),
                crate::i18n::arg("path", dir.to_string_lossy().into_owned()),
            ],
        ),
        other => other.to_string(),
    }
}

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

pub fn transfer_into_named_with(
    src: &Path,
    dest_dir: &Path,
    name: &std::ffi::OsStr,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, String> {
    host_fs::transfer::transfer_named(src, dest_dir, name, remove_source, observer).map_err(refusal)
}

fn destination_inspection_error(path: &Path, error: &std::io::Error) -> String {
    crate::i18n::t_args(
        "files-destination-inspect-failed",
        &[
            crate::i18n::arg("path", path.to_string_lossy().into_owned()),
            crate::i18n::arg("error", error.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(windows)]
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[test]
    fn system_errors_read_as_copy() {
        let denied = std::io::Error::from_raw_os_error(13).to_string();
        assert_eq!(
            readable_error(&denied),
            crate::i18n::t("files-error-permission")
        );
        let full = std::io::Error::from_raw_os_error(28).to_string();
        assert_eq!(
            readable_error(&full),
            crate::i18n::t("files-error-no-space")
        );
        let odd = std::io::Error::from_raw_os_error(40).to_string();
        let shown = readable_error(&odd);
        assert!(!shown.contains("os error"), "{shown}");
        assert!(
            shown.chars().next().is_some_and(char::is_uppercase),
            "{shown}"
        );
        assert_eq!(
            readable_error("A file with that name already exists"),
            "A file with that name already exists"
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn legacy_delete_creates_owner_only_trash_metadata_and_payload_directories() {
        use std::os::unix::fs::MetadataExt as _;
        let home = tilo_test_env::TestHome::new().expect("private test home");
        let source = home.home().join("notes.txt");
        fs::write(&source, b"private notes").expect("source contents");
        delete_to_trash(std::slice::from_ref(&source)).expect("production legacy delete");
        assert!(!source.exists());
        let files = trash_dir().expect("test Trash path");
        let data = files
            .parent()
            .and_then(Path::parent)
            .expect("data directory");
        for directory in ["Trash", "Trash/files", "Trash/info"] {
            let path = data.join(directory);
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

    #[test]
    #[cfg(windows)]
    fn windows_file_operations_refuse_streams_and_junction_escapes() {
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
                .output_locked()
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

    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn basic_file_operations_dedup_and_never_overwrite() {
        {
            let d = tmp();
            let root = d.path();
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
            fs::create_dir(root.join("folder")).unwrap();
            assert_eq!(
                unique_destination(root, "folder").unwrap(),
                root.join("folder 2")
            );
        }

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

        {
            let d = tmp();
            let f = d.path().join("old.txt");
            fs::write(&f, b"x").unwrap();
            let renamed = rename(&f, "new.txt").unwrap();
            assert!(renamed.ends_with("new.txt"));
            assert!(!f.exists());
            assert!(renamed.exists());
            fs::write(d.path().join("taken.txt"), b"x").unwrap();
            assert!(rename(&renamed, "taken.txt").is_err());
            assert!(rename(&renamed, "..").is_err());
        }

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
            assert!(src.join("inner.txt").exists());
            let copied2 = copy_into(&src, &destdir).unwrap();
            assert_eq!(copied2.file_name().unwrap().to_string_lossy(), "dir 2");
        }

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

        {
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

    #[cfg(unix)]
    #[test]
    fn destination_inspection_failure_reads_as_files_copy() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        fs::write(&source, b"selected bytes").unwrap();
        symlink("destination", &destination).unwrap();
        let candidate = destination.join("report.txt");
        let expected = crate::i18n::t_args(
            "files-destination-inspect-failed",
            &[
                crate::i18n::arg("path", candidate.to_string_lossy().into_owned()),
                crate::i18n::arg(
                    "error",
                    fs::symlink_metadata(&candidate).unwrap_err().to_string(),
                ),
            ],
        );
        for remove_source in [false, true] {
            let result = transfer_into_with(
                &source,
                &destination,
                remove_source,
                &mut NoopTransferObserver,
            );
            assert_eq!(result.unwrap_err(), expected);
            assert_eq!(fs::read(&source).unwrap(), b"selected bytes");
            assert_eq!(
                fs::read_link(&destination).unwrap(),
                PathBuf::from("destination")
            );
        }
    }
}
