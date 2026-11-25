//! The device's own web UI as a setting: on until its person turns it off, off at once
//! when they do, and served again when they turn it back on.
//!
//! The listener itself is the embedder's ([`crate::daemon::Server::serve_web`], or the
//! server's own start); this keeps whether the device wants one, how to serve it again, and
//! what stops it. The host operation `web.serve` and the server's entry among its person's
//! servers both reach it here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};

use tilo_workspace_service::workspaces::WebUi;

use super::{WebHandle, WebParts, WebTasks, serve_parts};

/// This device's web UI.
pub(crate) struct Control {
    /// Whether the device serves a web UI: on until its person says otherwise.
    wanted: AtomicBool,
    /// What serves it again after it stopped, once the embedder named it.
    parts: SyncMutex<Option<WebParts>>,
    /// The listener this control started again, kept so that it keeps serving.
    serving: SyncMutex<Option<WebHandle>>,
    /// What stops the listener serving now, whichever side started it.
    tasks: SyncMutex<Option<Arc<WebTasks>>>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            wanted: AtomicBool::new(true),
            parts: SyncMutex::default(),
            serving: SyncMutex::default(),
            tasks: SyncMutex::default(),
        }
    }
}

impl Control {
    /// Whether the device's setting says it serves a web UI: on until its person says
    /// not.
    pub(crate) fn wanted(&self) -> bool {
        self.wanted.load(Ordering::SeqCst)
    }

    /// Remember whether the device serves a web UI, as its entry among its person's
    /// servers says: what its start reads before it binds a listener.
    pub(crate) fn set_wanted(&self, wanted: bool) {
        self.wanted.store(wanted, Ordering::SeqCst);
    }

    /// Remember how to serve the listener, without serving it: what a device whose
    /// person turned its web UI off keeps, so that turning it on serves it.
    pub(crate) fn ready(&self, parts: WebParts) {
        *self
            .parts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(parts);
    }

    /// The embedder served the listener `handle`: remember how to serve it again, and
    /// what stops it before the handle is dropped.
    pub(crate) fn started(&self, parts: WebParts, handle: &WebHandle) {
        self.ready(parts);
        let tasks = handle.tasks();
        let previous = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(tasks);
        if let Some(previous) = previous {
            previous.stop();
        }
    }

    /// Stop serving now, and remember that the device serves none.
    fn stop(&self) {
        self.stop_serving();
    }

    /// [`Self::stop`], answering what served, so a stop can wait until it let go: the tasks
    /// that served reach the endpoint, which the address this device is reached at binds.
    pub(crate) fn stop_serving(&self) -> Vec<tokio::task::JoinHandle<()>> {
        self.wanted.store(false, Ordering::SeqCst);
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // A handle this control started keeps its listener alive: dropping it ends what
        // is left of it.
        let _ = self
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        tasks.map(|tasks| tasks.stop()).unwrap_or_default()
    }

    /// Serve it again, binding a listener of its own: `Ok` when this process never served
    /// one here (it serves one from its next start), or when one is serving already.
    fn start(self: &Arc<Self>) -> Result<(), String> {
        self.wanted.store(true, Ordering::SeqCst);
        // A listener serves when this control holds one it started, or the tasks of the
        // one the embedder served.
        let serving = self
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
            || self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some();
        if serving {
            return Ok(());
        }
        let Some(parts) = self
            .parts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        else {
            return Ok(());
        };
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "this device runs no runtime to serve its web UI on".to_owned())?;
        let control = Arc::clone(self);
        runtime.spawn(async move {
            match serve_parts(parts).await {
                Ok(handle) => {
                    let previous = control
                        .tasks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .replace(handle.tasks());
                    if let Some(previous) = previous {
                        previous.stop();
                    }
                    *control
                        .serving
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
                }
                Err(error) => tracing::warn!(%error, "the web UI could not be served again"),
            }
        });
        Ok(())
    }
}

/// The control as the device's workspaces ask for it, under the host operation
/// `web.serve`.
pub(crate) struct Serving(pub(crate) Arc<Control>);

impl WebUi for Serving {
    fn serve(&self, serving: bool) -> Result<(), String> {
        if serving {
            self.0.start()
        } else {
            self.0.stop();
            Ok(())
        }
    }
}
