//! The production [`FilePickerSpawner`]: a queue the compositor drains
//! on its own thread, every frame.
//!
//! [`FilePickerSpawner::spawn`] is called from the zbus dispatcher — a
//! tokio worker thread — but the dialog is a widget in the compositor's
//! `Rc`/`RefCell` tree, owned by the frame thread and untouchable
//! elsewhere. So the two sides meet at a mutex-guarded `Vec`, the same
//! shape [`super::PromptQueueHandle`] uses: `spawn` pushes and returns
//! immediately (the trait requires it not to block); the frame thread
//! drains, mounts the dialog, and fulfils the reply.
//!
//! Every queued request owns a `oneshot::Sender`. Dropping one resolves
//! the awaiting D-Bus method as "cancelled" — correct for a dialog that
//! can no longer be shown, but it must be a decision, not an accident:
//! [`FilePickerQueue::drain`] hands the senders on to the frame thread,
//! which answers rather than holds them if it can't render (no chrome yet).

use std::path::{Path, PathBuf};
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

    /// Take everything queued so far; each entry still owns its reply channel — whoever drains is responsible for answering it.
    pub fn drain(&self) -> Vec<FilePickerRequest> {
        match self.pending.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            // A poisoned lock means a panic happened while holding it — report
            // nothing rather than a partial list; the senders drop with the mutex and callers see "cancelled", the safe answer.
            Err(_) => Vec::new(),
        }
    }

    /// How many requests are waiting. For tests and for the state dump.
    pub fn len(&self) -> usize {
        self.pending.lock().map_or(0, |q| q.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FilePickerSpawner for FilePickerQueue {
    fn spawn(&self, req: FilePickerRequest) {
        match self.pending.lock() {
            Ok(mut q) => q.push(req),
            // Can't queue it, so answer it — holding a sender we'll never read is the one outcome that hangs the app.
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
/// probe the filesystem, and an app that could name its own roots would
/// be granting itself the thing the dialog is supposed to be asking about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerScope {
    /// Directory the dialog opens on.
    pub initial_dir: PathBuf,
    /// Every directory the user may reach from the dialog; the widget refuses to navigate outside these.
    pub roots: Vec<PathBuf>,
    /// The person's home folder, when the dialog should offer it (and the
    /// usual folders under it) in its sidebar. Only what `roots` allows shows.
    pub home: Option<PathBuf>,
}

impl PickerScope {
    /// The baseline scope: the user's own directories.
    ///
    /// This is the *user's* reach, not the app's — the app gets whatever the
    /// user then picks, and only that. Scoping the dialog to what the app
    /// already holds would make the portal incapable of granting anything new, which is the only thing it's for.
    pub fn for_user() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(h) = home.as_ref().filter(|h| h.is_dir()) {
            roots.push(h.clone());
        }
        // The projected workspace tree, when mounted — a picker that couldn't
        // reach it would make every shared document unopenable through the portal.
        let workspace = PathBuf::from("/var/workspace");
        if workspace.is_dir() {
            roots.push(workspace);
        }
        let fallback = roots.first().cloned().unwrap_or_else(|| PathBuf::from("/"));
        if roots.is_empty() {
            roots.push(fallback.clone());
        }
        Self {
            initial_dir: documents_dir(home.as_deref()).unwrap_or(fallback),
            roots,
            home,
        }
    }

    /// Widen the scope with the filesystem paths this workspace has already
    /// granted the caller.
    ///
    /// A path the app can already reach is not a new decision, so showing it
    /// costs nothing — leaving it out would strand an app granted a project
    /// directory outside `$HOME`, unable to reach its own files through the dialog.
    ///
    /// Directories are added as roots; a grant on a single *file* adds its
    /// parent so the file is visible in a listing, but the grant itself still only covers the file.
    #[must_use]
    pub fn with_granted_paths<I: IntoIterator<Item = PathBuf>>(mut self, granted: I) -> Self {
        for path in granted {
            let root = if path.is_dir() {
                path
            } else {
                match path.parent() {
                    Some(p) if p.is_dir() => p.to_path_buf(),
                    _ => continue,
                }
            };
            if !self.roots.iter().any(|r| root.starts_with(r)) {
                self.roots.push(root);
            }
        }
        self
    }
}

/// The user's documents directory, if there is one.
///
/// Where the dialog opens is the difference between useful and useless:
/// `$HOME` on a developer's machine is eighteen rows of dotfiles and not
/// one document, so the user's first action is always to navigate away.
/// Documents is usually the answer on the first screen.
///
/// `$XDG_DOCUMENTS_DIR` first (the freedesktop variable), then the
/// `user-dirs.dirs` file the XDG spec actually stores it in, then the
/// conventional `~/Documents`. Nothing is created — a machine without one falls back to `$HOME`.
fn documents_dir(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let configured = tilo_paths::UserDir::Documents.resolve_for_home(home);
    if let Some(dir) = configured.filter(|d| is_useful_documents_dir(d, home)) {
        return Some(dir);
    }
    let conventional = home.join("Documents");
    conventional.is_dir().then_some(conventional)
}

/// Is a configured documents directory worth opening on? It has to exist
/// and be somewhere other than `$HOME`, so `~/Documents` gets a look before
/// falling back to `$HOME`.
fn is_useful_documents_dir(dir: &Path, home: &Path) -> bool {
    dir.is_dir() && dir != home
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
                parent_window: None,
                title: title.to_string(),
                save: false,
                directory: false,
                writable_directory: false,
                multiple: false,
                accept_label: None,
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply: tx,
            },
            rx,
        )
    }

    #[test]
    fn picker_queue_queues_without_blocking_and_drains_everything() {
        // Phase `spawn_queues_rather_than_blocking`.
        {
            let q = FilePickerQueue::new();
            let (r, _rx) = req("Open");
            q.spawn(r);
            assert_eq!(q.len(), 1, "spawn must buffer the request");
        }
        // Phase `drain_takes_everything_and_leaves_the_queue_empty`.
        {
            let q = FilePickerQueue::new();
            let (a, _ra) = req("a");
            let (b, _rb) = req("b");
            q.spawn(a);
            q.spawn(b);
            let taken = q.drain();
            assert_eq!(taken.len(), 2);
            assert!(q.is_empty(), "drain must leave nothing behind");
        }
    }

    #[tokio::test]
    async fn a_dropped_request_answers_cancelled_rather_than_hanging() {
        let q = FilePickerQueue::new();
        let (r, rx) = req("Open");
        q.spawn(r);
        // Simulate the frame thread taking the request and then going away without deciding — the app must not be left waiting.
        drop(q.drain());
        assert!(
            rx.await.is_err(),
            "a dropped sender must close the channel, which the \
             FileChooser backend maps to cancelled"
        );
    }

    #[test]
    fn user_scope_root_ignores_home_or_missing_documents_dirs() {
        // Phase `a_documents_dir_that_is_home_is_treated_as_unset`.
        {
            let home = Path::new("/home/someone");
            assert!(
                !is_useful_documents_dir(home, home),
                "a documents dir equal to home carries no information"
            );
        }
        // Phase `a_missing_documents_dir_is_not_used`.
        {
            let home = Path::new("/home/someone");
            assert!(!is_useful_documents_dir(
                Path::new("/definitely/not/here"),
                home
            ));
        }
        // Phase `the_user_scope_always_has_a_root_and_starts_inside_it`.
        {
            let scope = PickerScope::for_user();
            assert!(
                !scope.roots.is_empty(),
                "a picker with no root can browse nothing"
            );
            assert!(
                scope.roots.iter().any(|r| scope.initial_dir.starts_with(r)),
                "the dialog must open somewhere it is allowed to be: {scope:?}"
            );
        }
    }
}
