//! Change notification for the shared settings file.
//!
//! Writers replace `settings.json` through a temp file and a rename, so a watch on the file
//! itself would follow the old inode and go quiet after the first save. The watch sits on the
//! containing directory instead and reports only events that name the settings file.
//!
//! The watches are change-only ([`tilo_files::native::watch_changes`]): reading a file in a
//! watched directory does not wake them. The identity directory is read whenever the desktop
//! checks who is signed in, and an idle desktop woke a watcher for each of those reads.

use std::path::Path;

/// A live watch; dropping it stops notifications.
pub struct SettingsWatch {
    _watch: tilo_files::native::ChangeWatch,
}

/// Call `on_change` every time the configured settings file is created, rewritten, renamed
/// into place or removed. The callback runs on the watcher's own thread, so it should only
/// hand the news to whoever re-reads the document.
///
/// # Errors
///
/// Fails when no config directory can be resolved, the directory cannot be created, or the
/// platform watcher cannot be started.
pub fn watch_settings(on_change: impl Fn() + Send + 'static) -> std::io::Result<SettingsWatch> {
    let Some(path) = crate::settings_path() else {
        return Err(std::io::Error::other("no config directory available"));
    };
    watch_settings_at(&path, on_change)
}

/// [`watch_settings`] for an explicit settings file.
///
/// # Errors
///
/// As [`watch_settings`].
pub fn watch_settings_at(
    path: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    let Some(directory) = path.parent() else {
        return Err(std::io::Error::other(
            "settings file has no parent directory",
        ));
    };
    std::fs::create_dir_all(directory)?;
    let Some(file_name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
        return Err(std::io::Error::other("settings path has no file name"));
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
    let root = directory.to_owned();
    watch_directory(
        directory,
        move |path| {
            path.parent() == Some(root.as_path())
                && (path.file_name().is_some_and(|name| name == "active")
                    || path
                        .extension()
                        .is_some_and(|extension| extension == "json"))
        },
        on_change,
    )
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
        _watch: tilo_files::native::watch_changes(directory, names, on_change)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn identity_switch_and_selected_record_removal_wake_consumers() {
        let root = crate::tests::tmpdir();
        let (sender, changes) = mpsc::channel();
        let _watch = watch_identity_at(&root, move || {
            let _ = sender.send(());
        })
        .unwrap();
        std::fs::write(root.join("unrelated.txt"), "unrelated").unwrap();
        assert!(changes.recv_timeout(Duration::from_millis(100)).is_err());
        std::fs::write(root.join("active"), "work").unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        while changes.recv_timeout(Duration::from_millis(100)).is_ok() {}
        std::fs::write(root.join("work.json"), "{}").unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        while changes.recv_timeout(Duration::from_millis(100)).is_ok() {}
        std::fs::remove_file(root.join("work.json")).unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
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

    /// How often the thread blocked, and whether it is blocked now.
    #[cfg(target_os = "linux")]
    fn blocked(tid: &str) -> (u64, bool) {
        let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).unwrap();
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(str::trim)
                .unwrap_or_default()
                .to_owned()
        };
        (
            field("voluntary_ctxt_switches:").parse().unwrap(),
            field("State:").starts_with('S'),
        )
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reading_the_watched_directory_does_not_wake_the_watch() {
        use std::os::unix::fs::PermissionsExt as _;
        // The identity directory is read whenever the desktop checks who is
        // signed in, and its reader re-applies the directory's private mode.
        // Neither changes a file, so neither may wake the watch's thread: on
        // an idle desktop each read did, through the opens `notify` watches.
        let root = crate::tests::tmpdir();
        let record = root.join("work.json");
        std::fs::write(&record, "{}").unwrap();
        let before = watch_threads();
        let _watch = watch_identity_at(&root, || {}).unwrap();
        let mut tid = None;
        tilo_test_env::wait_until("the watch thread to block", || {
            tid = watch_threads()
                .into_iter()
                .find(|candidate| !before.contains(candidate));
            tid.as_deref().is_some_and(|tid| blocked(tid).1)
        });
        let tid = tid.unwrap();
        let (switches, _) = blocked(&tid);
        for _ in 0..20 {
            std::fs::read(&record).unwrap();
            let _ = std::fs::read_dir(&root).unwrap().count();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            blocked(&tid).0,
            switches,
            "reading the watched directory woke the watch thread"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_atomic_rewrite_is_reported_and_sibling_files_are_not() {
        let root = crate::tests::tmpdir();
        let path = root.join("tilo").join("settings.json");
        let (sender, changes) = mpsc::channel();
        let _watch = watch_settings_at(&path, move || {
            let _ = sender.send(());
        })
        .unwrap();

        std::fs::write(root.join("tilo").join("unrelated.json"), "{}").unwrap();
        assert!(
            changes.recv_timeout(Duration::from_millis(300)).is_err(),
            "a sibling file is not the settings document"
        );

        let mut theme = serde_json::Map::new();
        theme.insert("theme".into(), serde_json::json!("Light"));
        crate::update_section_at(&path, "appearance", theme.clone()).unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        while changes.recv_timeout(Duration::from_millis(100)).is_ok() {}

        theme.insert("theme".into(), serde_json::json!("Dark"));
        crate::update_section_at(&path, "appearance", theme).unwrap();
        changes
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("a second atomic save still reaches the watch");
        std::fs::remove_dir_all(root).unwrap();
    }
}
