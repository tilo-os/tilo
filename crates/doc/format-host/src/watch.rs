//! Inotify-driven hot-reload. When the registry file changes (write / rename /
//! create-then-replace), reparse it and re-apply onto the router. Errors are
//! logged but don't kill the daemon — a half-edited registry just keeps the
//! previous set live.

use std::path::PathBuf;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::router::Router;

/// Spawn an inotify watcher on the registry file. Reloads are debounced 100ms
/// to coalesce editors that write-truncate then write-data.
pub fn spawn_watcher(router: Router, registry_path: PathBuf) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<()>(8);
    // Watch the parent dir — notify on directly-named files misses
    // `rename-then-replace` patterns (vim/`mv`) where the inode changes.
    let watch_dir = registry_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("registry path has no parent dir"))?
        .to_path_buf();
    std::fs::create_dir_all(&watch_dir)?;
    let registry_name = registry_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("registry path has no file component"))?
        .to_os_string();

    let mut watcher: RecommendedWatcher = {
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && ev
                    .paths
                    .iter()
                    .any(|p| p.file_name() == Some(&registry_name))
            {
                let _ = tx.blocking_send(());
            }
        })?
    };
    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    let router2 = router;
    let registry_path2 = registry_path;
    tokio::spawn(async move {
        // Keep the watcher alive in this task.
        let _watcher = watcher;
        while rx.recv().await.is_some() {
            tokio::time::sleep(Duration::from_millis(100)).await;
            while rx.try_recv().is_ok() {}
            if let Err(e) = crate::router::load_into(&router2, &registry_path2) {
                tracing::warn!(error = %e, "registry reload failed; keeping previous set");
            } else {
                tracing::info!(path = %registry_path2.display(), "registry reloaded");
            }
        }
    });
    Ok(())
}
