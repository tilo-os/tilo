//! Change notification for the settings and the directories beside them.
//!
//! The settings themselves ring their source's change signal ([`watch_settings`]). The
//! identity and dictionary directories are watched as directories: their writers replace
//! files by rename, so a watch on a file would follow the old inode.
//!
//! The directory watches are change-only ([`tilo_files::native::watch_changes`]): reading a file in a
//! watched directory does not wake them. The identity directory is read whenever the desktop
//! checks who is signed in, and an idle desktop woke a watcher for each of those reads.

use std::path::Path;

/// A live watch; dropping it stops notifications.
pub struct SettingsWatch {
    _directory: Option<tilo_files::native::ChangeWatch>,
    _listener: Option<std::sync::Arc<crate::Listener>>,
}

/// Call `on_change` every time this process's settings ([`crate::source`]) change. The
/// callback runs on the thread that made or noticed the change, so it should only hand the
/// news to whoever re-reads the document.
///
/// # Errors
///
/// Fails when there are no settings: no config directory and no installed source.
pub fn watch_settings(
    on_change: impl Fn() + Send + Sync + 'static,
) -> std::io::Result<SettingsWatch> {
    let Some(source) = crate::source() else {
        return Err(std::io::Error::other("no config directory available"));
    };
    let listener: std::sync::Arc<crate::Listener> = std::sync::Arc::new(on_change);
    source.changes().listen(&listener);
    Ok(SettingsWatch {
        _directory: None,
        _listener: Some(listener),
    })
}

/// Call `on_change` every time the file at `path` is created, rewritten, renamed into place
/// or removed: a runtime publication another process replaces by rename. The callback runs
/// on the watcher's own thread.
///
/// # Errors
///
/// Fails when the directory cannot be created or the platform watcher cannot be started.
pub fn watch_file_at(
    path: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    let Some(directory) = path.parent() else {
        return Err(std::io::Error::other("the file has no parent directory"));
    };
    std::fs::create_dir_all(directory)?;
    let Some(file_name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
        return Err(std::io::Error::other("the path has no file name"));
    };
    watch_directory(
        directory,
        move |changed| changed.file_name() == Some(file_name.as_os_str()),
        on_change,
    )
}

/// Watch the existing identity directory for selector and identity-record changes.
/// Re-read the verified active owner after each notification.
///
/// # Errors
///
/// Fails if the directory does not exist or the platform watcher cannot start.
pub fn watch_identity_at(
    directory: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    watch_directory(directory, identity_entry(directory.to_owned()), on_change)
}

/// Whether a changed path is the identity selector or an identity record
/// directly in `root`.
fn identity_entry(root: std::path::PathBuf) -> impl Fn(&Path) -> bool + Send + 'static {
    move |path| {
        path.parent() == Some(root.as_path())
            && (path.file_name().is_some_and(|name| name == "active")
                || path
                    .extension()
                    .is_some_and(|extension| extension == "json"))
    }
}

/// Watch atomic account-dictionary publications, excluding temporary files and
/// legacy ownership markers. The host creates the private directory first.
pub(crate) fn watch_dictionary_at(
    directory: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    let root = directory.to_owned();
    watch_directory(
        directory,
        move |path| {
            path.parent() == Some(root.as_path())
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                && path
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|stem| {
                        stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
        },
        on_change,
    )
}

/// Call `on_change` when an entry of `directory` that `names` accepts (given its full path)
/// changes, and when changes may have been missed.
fn watch_directory(
    directory: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    Ok(SettingsWatch {
        _directory: Some(tilo_files::native::watch_changes(
            directory, names, on_change,
        )?),
        _listener: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A watch whose every notification says what `look` saw when it ran.
    fn watch_seeing<T: Send + 'static>(
        start: impl FnOnce(Box<dyn Fn() + Send>) -> std::io::Result<SettingsWatch>,
        look: impl Fn() -> T + Send + 'static,
    ) -> (SettingsWatch, mpsc::Receiver<T>) {
        let (sender, seen) = mpsc::channel();
        let watch = start(Box::new(move || {
            let _ = sender.send(look());
        }))
        .unwrap();
        (watch, seen)
    }

    /// Waits for a notification that ran once `seen` holds.
    fn notified_after<T>(what: &str, seen: &mpsc::Receiver<T>, holds: impl Fn(&T) -> bool) {
        while !holds(&tilo_test_env::recv(what, seen)) {}
    }

    #[test]
    fn identity_switch_and_selected_record_removal_wake_consumers() {
        let root = crate::tests::tmpdir();
        let looking = root.clone();
        let (_watch, seen) = watch_seeing(
            |on_change| watch_identity_at(&root, on_change),
            move || {
                (
                    std::fs::read_to_string(looking.join("active")).ok(),
                    looking.join("work.json").exists(),
                )
            },
        );
        std::fs::write(root.join("active"), "work").unwrap();
        notified_after("the identity switch", &seen, |(active, _)| {
            active.as_deref() == Some("work")
        });
        std::fs::write(root.join("work.json"), "{}").unwrap();
        notified_after("the new record", &seen, |(_, record)| *record);
        std::fs::remove_file(root.join("work.json")).unwrap();
        notified_after("the record's removal", &seen, |(_, record)| !*record);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_selector_and_records_in_the_identity_directory_are_its_entries() {
        let root = std::path::PathBuf::from("/identities");
        let entry = identity_entry(root.clone());
        assert!(entry(&root.join("active")));
        assert!(entry(&root.join("work.json")));
        assert!(!entry(&root.join("unrelated.txt")));
        assert!(!entry(&root.join("nested").join("work.json")));
    }

    /// The watch threads this process runs, by thread id.
    #[cfg(target_os = "linux")]
    fn watch_threads() -> Vec<String> {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .map(|task| task.file_name().to_string_lossy().into_owned())
            .filter(|tid| {
                std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                    .is_ok_and(|name| name.trim() == tilo_files::native::CHANGE_WATCH_THREAD)
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reading_the_watched_directory_does_not_wake_the_watch() {
        use std::os::unix::fs::PermissionsExt as _;
        // The identity directory is read whenever the desktop checks who is
        // signed in, and its reader re-applies the directory's private mode.
        // Neither changes a file, so neither may wake the watch's thread: on
        // an idle desktop each read did, through the opens `notify` watches.
        // The directory is private before the watch starts, and the reader
        // only chmods one that is not: a chmod that changes nothing still
        // tells every watcher of the directory that its attributes changed.
        let root = crate::tests::tmpdir();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let record = root.join("work.json");
        std::fs::write(&record, "{}").unwrap();
        let before = watch_threads();
        let _watch = watch_identity_at(&root, || {}).unwrap();
        let mut tid = None;
        tilo_test_env::wait_until("the watch thread to block", || {
            tid = watch_threads()
                .into_iter()
                .find(|candidate| !before.contains(candidate));
            tid.as_deref()
                .is_some_and(|tid| crate::tests::blocked(tid).1)
        });
        let tid = tid.unwrap();
        let (switches, _) = crate::tests::blocked(&tid);
        for _ in 0..20 {
            std::fs::read(&record).unwrap();
            let _ = std::fs::read_dir(&root).unwrap().count();
            if std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777 != 0o700 {
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        assert_eq!(
            crate::tests::blocked_count(&tid),
            switches,
            "reading the watched directory woke the watch thread"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
