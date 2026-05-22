//! Abstraction for "spawn a file-picker overlay" — used by
//! [`super::file_chooser::FileChooserBackend`] to surface a real
//! dialog without coupling to the shell's chrome.
//!
//! Sandbox Phase 4.3 follow-up: the FileChooser backend was returning
//! a placeholder URI on `Allow`. This trait lets the backend hand off
//! the rendering + selection to whichever surface owns the caller's
//! workspace chrome.
//!
//! Two production implementors are imagined:
//!   - The hosted-mode chrome — pushes a [`quick_ui::widget::FilePicker`]
//!     into the workspace overlay layer.
//!   - The native compositor — opens a transient surface owned by the
//!     compositor's xdg-shell context.
//!
//! Tests use [`MockFilePickerSpawner`] which resolves the
//! `oneshot::Receiver` with a pre-baked path set, never rendering
//! anything. [`NullFilePickerSpawner`] always cancels (host fallback
//! for environments without a chrome).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::PortalCaller;

/// One end-user request the file-picker spawner must fulfil.
///
/// The host implementor renders a real dialog (mounted in the
/// caller's workspace chrome) and ultimately fulfils `reply` with
/// the user's selection: `Some(paths)` for "user picked these" or
/// `None` for "user cancelled / dialog closed without selection".
pub struct FilePickerRequest {
    /// Identity of the requesting app — host can show "{app_id} wants
    /// to open a file" in the dialog header.
    pub caller: PortalCaller,
    /// User-facing dialog title (provided by the calling app — usually
    /// "Open File" or similar).
    pub title: String,
    /// `true` for `SaveFile` / `SaveFiles` flows, `false` for `OpenFile`.
    /// Affects the action-button label ("Save" vs "Open").
    pub save: bool,
    /// One-shot reply channel. The spawner fulfils this with the
    /// user's selection.
    pub reply: oneshot::Sender<Option<Vec<PathBuf>>>,
}

/// Host-side contract: spawn a real file-picker for `req`.
///
/// Implementors typically push `req` into a per-frame queue the
/// compositor drains on the main thread. The reply channel resolves
/// the portal D-Bus call.
pub trait FilePickerSpawner: Send + Sync {
    /// Enqueue a request. Must not block — the caller is the D-Bus
    /// dispatcher and we don't want to stall every portal request on
    /// the main thread.
    fn spawn(&self, req: FilePickerRequest);
}

/// Cheap clone wrapper around `Arc<dyn FilePickerSpawner>` —
/// matches the [`super::PromptQueueHandle`] shape so each interface
/// impl can hold its own copy.
#[derive(Clone)]
pub struct FilePickerSpawnerHandle {
    inner: Arc<dyn FilePickerSpawner>,
}

impl FilePickerSpawnerHandle {
    pub fn new(inner: Arc<dyn FilePickerSpawner>) -> Self {
        Self { inner }
    }

    /// Helper — issue a request and return the receiver. Same
    /// shape `PromptQueueHandle::enqueue` uses.
    pub fn request(
        &self,
        caller: PortalCaller,
        title: impl Into<String>,
        save: bool,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let req = FilePickerRequest {
            caller,
            title: title.into(),
            save,
            reply: tx,
        };
        self.inner.spawn(req);
        rx
    }
}

/// Cancel-everything spawner — for hosted-mode environments without
/// a chrome (e.g. headless tests or the boot-time portal that's up
/// before the first workspace renders). Every request resolves to
/// `None` (cancelled).
pub struct NullFilePickerSpawner;

impl NullFilePickerSpawner {
    pub fn cancel() -> Self {
        Self
    }
}

impl FilePickerSpawner for NullFilePickerSpawner {
    fn spawn(&self, req: FilePickerRequest) {
        let _ = req.reply.send(None);
    }
}

/// Test-only spawner that returns a pre-baked path list on every
/// request. Tracks the requests it received so tests can assert on
/// the caller / title / save flag without rendering anything.
pub struct MockFilePickerSpawner {
    paths: Vec<PathBuf>,
    seen: Mutex<Vec<(PortalCaller, String, bool)>>,
}

impl MockFilePickerSpawner {
    /// Build a mock that resolves every request with `paths`.
    pub fn returning(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Build a mock that always resolves to "user cancelled".
    pub fn always_cancel() -> Self {
        Self {
            paths: Vec::new(),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Tests assert against this to verify the backend forwarded the
    /// right caller / title / save flag.
    pub fn requests(&self) -> Vec<(PortalCaller, String, bool)> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl FilePickerSpawner for MockFilePickerSpawner {
    fn spawn(&self, req: FilePickerRequest) {
        if let Ok(mut s) = self.seen.lock() {
            s.push((req.caller.clone(), req.title.clone(), req.save));
        }
        // Empty path list ≡ cancelled — mirrors the dialog's
        // "Open with no selection = cancel" treatment.
        let reply = if self.paths.is_empty() {
            None
        } else {
            Some(self.paths.clone())
        };
        let _ = req.reply.send(reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;
    use std::path::PathBuf;

    #[tokio::test]
    async fn null_spawner_resolves_to_none() {
        let h = FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel()));
        let caller = PortalCaller::new(WorkspaceId(1), "app");
        let rx = h.request(caller, "Open", false);
        let out = rx.await.expect("oneshot");
        assert!(out.is_none(), "null spawner must always cancel");
    }

    #[tokio::test]
    async fn mock_spawner_returns_baked_paths() {
        let h = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(
            vec![PathBuf::from("/tmp/a.txt")],
        )));
        let caller = PortalCaller::new(WorkspaceId(2), "app");
        let rx = h.request(caller, "Open", false);
        let out = rx.await.expect("oneshot");
        assert_eq!(out, Some(vec![PathBuf::from("/tmp/a.txt")]));
    }

    #[tokio::test]
    async fn mock_spawner_records_requests() {
        let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
            "/x",
        )]));
        let h = FilePickerSpawnerHandle::new(mock.clone());
        let caller = PortalCaller::new(WorkspaceId(3), "org.example");
        let _ = h.request(caller.clone(), "Open file", false).await;
        let _ = h.request(caller.clone(), "Save as", true).await;
        let seen = mock.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1, "Open file");
        assert!(!seen[0].2);
        assert_eq!(seen[1].1, "Save as");
        assert!(seen[1].2);
    }

    #[tokio::test]
    async fn mock_always_cancel_returns_none() {
        let h =
            FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
        let caller = PortalCaller::new(WorkspaceId(4), "app");
        let rx = h.request(caller, "Open", false);
        assert!(rx.await.expect("oneshot").is_none());
    }
}
