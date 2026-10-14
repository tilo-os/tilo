//! Change notification for the shared settings file.
//!
//! Writers replace `settings.json` through a temp file and a rename, so a watch on the file
//! itself would follow the old inode and go quiet after the first save. The watch sits on the
//! containing directory instead and reports only events that name the settings file.

use std::path::Path;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// A live watch; dropping it stops notifications.
pub struct SettingsWatch {
    _watcher: RecommendedWatcher,
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
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            // An overflowed or failed queue may have hidden a change; re-reading is cheap.
            on_change();
            return;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event
            .paths
            .iter()
            .any(|changed| changed.file_name() == Some(file_name.as_os_str()))
        {
            on_change();
        }
    })
    .map_err(std::io::Error::other)?;
    watcher
        .watch(directory, RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;
    Ok(SettingsWatch { _watcher: watcher })
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
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            on_change();
            return;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.iter().any(|path| {
            path.parent() == Some(root.as_path())
                && (path.file_name().is_some_and(|name| name == "active")
                    || path
                        .extension()
                        .is_some_and(|extension| extension == "json"))
        }) {
            on_change();
        }
    })
    .map_err(std::io::Error::other)?;
    watcher
        .watch(directory, RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;
    Ok(SettingsWatch { _watcher: watcher })
}

/// Watch atomic account-dictionary publications, excluding temporary files and
/// legacy ownership markers. The host creates the private directory first.
pub(crate) fn watch_dictionary_at(
    directory: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<SettingsWatch> {
    let root = directory.to_owned();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            on_change();
            return;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.iter().any(|path| {
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
        }) {
            on_change();
        }
    })
    .map_err(std::io::Error::other)?;
    watcher
        .watch(directory, RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;
    Ok(SettingsWatch { _watcher: watcher })
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
