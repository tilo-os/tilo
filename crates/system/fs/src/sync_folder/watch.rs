//! Passes on change: the host's file events for the folder (`FSEvents` on
//! macOS, `inotify` on Linux) and a periodic pass for the workspace.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use super::SyncFolder;

/// How long the folder must be quiet before a pass: an editor's save is
/// several events, and a copy of a large file many.
const SETTLE: Duration = Duration::from_millis(200);

/// A folder kept in step until dropped.
pub struct Watch {
    watcher: Option<notify::RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl SyncFolder {
    /// Runs a pass now, then on every change in the folder and at least
    /// every `interval`, on a thread of its own, until the [`Watch`] is
    /// dropped.
    ///
    /// # Errors
    ///
    /// The host's file events cannot be subscribed to.
    pub fn watch(mut self, interval: Duration) -> io::Result<Watch> {
        let (notify_change, changes) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            let _ = notify_change.send(());
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;
        let thread = std::thread::Builder::new()
            .name("tilo-sync-folder".into())
            .spawn(move || {
                loop {
                    if let Err(error) = self.reconcile() {
                        tracing::warn!(%error, folder = %self.root.display(), "sync pass failed");
                    }
                    let next = changes
                        .recv_timeout(interval)
                        .and_then(|()| settle(&changes));
                    if next == Err(RecvTimeoutError::Disconnected) {
                        break;
                    }
                }
            })?;
        Ok(Watch {
            watcher: Some(watcher),
            thread: Some(thread),
        })
    }
}

/// Waits until no change arrived for [`SETTLE`].
fn settle(changes: &mpsc::Receiver<()>) -> Result<(), RecvTimeoutError> {
    loop {
        match changes.recv_timeout(SETTLE) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(disconnected) => return Err(disconnected),
        }
    }
}

impl Drop for Watch {
    /// Stops after the pass in progress: dropping the subscription ends the
    /// event channel the thread waits on.
    fn drop(&mut self) {
        drop(self.watcher.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
