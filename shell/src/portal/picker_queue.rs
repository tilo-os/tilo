//! The production [`FilePickerSpawner`]: a queue the compositor drains
//! on its own thread, every frame.
//!
//! ## Why a queue and not a callback
//!
//! [`FilePickerSpawner::spawn`] is called from the zbus dispatcher — a
//! tokio worker thread. The dialog it has to raise is a widget in the
//! compositor's tree, and that tree is `Rc`/`RefCell` all the way down:
//! it is owned by the frame thread and cannot be touched from anywhere
//! else. So the two sides meet at a mutex-guarded `Vec`, the same shape
//! [`super::PromptQueueHandle`] uses. `spawn` pushes and returns
//! immediately (the trait requires it not to block); the frame thread
//! drains, mounts the dialog, and fulfils the reply when the user
//! decides.
//!
//! ## Nothing is dropped silently
//!
//! Every queued request owns a `oneshot::Sender`. Dropping one resolves
//! the awaiting D-Bus method as "cancelled", which is the correct answer
//! for a dialog that can no longer be shown — but it must be a decision,
//! not an accident. [`FilePickerQueue::drain`] hands the senders on to
//! the frame thread; if the frame thread cannot render (no chrome yet),
//! it answers rather than holding them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::file_picker_spawner::{FilePickerRequest, FilePickerSpawner};

/// Shared buffer between the D-Bus dispatcher and the frame thread.
#[derive(Clone, Default)]
pub struct FilePickerQueue {
    pending: Arc<Mutex<Vec<FilePickerRequest>>>,
}

impl FilePickerQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take everything queued so far. Each entry still owns its reply
    /// channel — whoever drains is responsible for answering it.
    pub fn drain(&self) -> Vec<FilePickerRequest> {
        match self.pending.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            // A poisoned lock means a panic happened while holding it.
            // Report nothing rather than a partial list; the senders in
            // there drop with the mutex and the callers see "cancelled",
            // which is the safe answer.
            Err(_) => Vec::new(),
        }
    }

    /// How many requests are waiting. For tests and for the state dump.
    pub fn len(&self) -> usize {
        self.pending.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FilePickerSpawner for FilePickerQueue {
    fn spawn(&self, req: FilePickerRequest) {
        match self.pending.lock() {
            Ok(mut q) => q.push(req),
            // Can't queue it, so answer it. Holding a sender we will
            // never read is the one outcome that hangs the app.
            Err(_) => {
                let _ = req.reply.send(None);
            }
        }
    }
}

/// Where a picker should start browsing, and how far it may roam.
///
/// Both come from the compositor, never from the requesting app: an app
/// that could name its own starting directory could use the dialog to
/// probe the filesystem for what exists, and an app that could name its
/// own roots would be granting itself the thing the dialog is supposed
/// to be asking about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerScope {
    /// Directory the dialog opens on.
    pub initial_dir: PathBuf,
    /// Every directory the user may reach from the dialog. The widget
    /// refuses to navigate outside these.
    pub roots: Vec<PathBuf>,
}

impl PickerScope {
    /// The scope for a caller with no workspace-specific placesin: the
    /// user's own directories.
    ///
    /// This is the *user's* reach, not the app's. The app gets nothing
    /// from this — it gets whatever the user then picks, and only that.
    pub fn for_user() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(h) = home.as_ref() {
            roots.push(h.clone());
        }
        // The projected workspace tree, when it is mounted. A picker
        // that could not reach it would make every shared document
        // unopenable through the portal.
        let workspace = PathBuf::from("/var/workspace");
        if workspace.is_dir() {
            roots.push(workspace);
        }
        let initial_dir = home
            .filter(|h| h.is_dir())
            .or_else(|| roots.first().cloned())
            .unwrap_or_else(|| PathBuf::from("/"));
        if roots.is_empty() {
            roots.push(initial_dir.clone());
        }
        Self { initial_dir, roots }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::PortalCaller;
    use crate::shell::state::WorkspaceId;
    use tokio::sync::oneshot;

    fn req(title: &str) -> (FilePickerRequest, oneshot::Receiver<Option<Vec<PathBuf>>>) {
        let (tx, rx) = oneshot::channel();
        (
            FilePickerRequest {
                caller: PortalCaller::new(WorkspaceId(1), "app"),
                title: title.to_string(),
                save: false,
                multiple: false,
                accept_label: None,
                current_name: None,
                filters: Vec::new(),
                reply: tx,
            },
            rx,
        )
    }

    #[test]
    fn spawn_queues_rather_than_blocking() {
        let q = FilePickerQueue::new();
        let (r, _rx) = req("Open");
        q.spawn(r);
        assert_eq!(q.len(), 1, "spawn must buffer the request");
    }

    #[test]
    fn drain_takes_everything_and_leaves_the_queue_empty() {
        let q = FilePickerQueue::new();
        let (a, _ra) = req("a");
        let (b, _rb) = req("b");
        q.spawn(a);
        q.spawn(b);
        let taken = q.drain();
        assert_eq!(taken.len(), 2);
        assert!(q.is_empty(), "drain must leave nothing behind");
    }

    #[tokio::test]
    async fn a_dropped_request_answers_cancelled_rather_than_hanging() {
        let q = FilePickerQueue::new();
        let (r, rx) = req("Open");
        q.spawn(r);
        // Simulate the frame thread taking the request and then going
        // away without deciding — the app must not be left waiting.
        drop(q.drain());
        assert!(
            rx.await.is_err(),
            "a dropped sender must close the channel, which the \
             FileChooser backend maps to cancelled"
        );
    }

    #[test]
    fn the_user_scope_always_has_a_root_and_starts_inside_it() {
        let scope = PickerScope::for_user();
        assert!(!scope.roots.is_empty(), "a picker with no root can browse nothing");
        assert!(
            scope
                .roots
                .iter()
                .any(|r| scope.initial_dir.starts_with(r)),
            "the dialog must open somewhere it is allowed to be: {scope:?}"
        );
    }
}
