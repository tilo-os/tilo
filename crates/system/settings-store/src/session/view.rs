//! The host's last durably accepted settings, independent of storage locks.
//!
//! Core `SessionSettings` reads retain their synchronous external-copy contract.
//! The native event loop installs this view instead: its reads enqueue bounded
//! refresh work while the existing file watcher also incorporates writes.

use std::sync::{Arc, PoisonError, mpsc};
use std::thread::JoinHandle;

use serde_json::{Map, Value};

use super::{Committed, SessionSettings};
use crate::{ChangeSignal, SettingsSource};

struct HostView {
    session: Arc<SessionSettings>,
    refresh: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

pub(super) fn start(session: &Arc<SessionSettings>) -> std::io::Result<Arc<dyn SettingsSource>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let session_weak = Arc::downgrade(session);
    let worker = std::thread::Builder::new()
        .name("tilo-settings-read".into())
        .spawn(move || {
            while receiver.recv().is_ok() {
                let Some(session) = session_weak.upgrade() else {
                    break;
                };
                session.refresh();
            }
        })?;
    Ok(Arc::new(HostView {
        session: Arc::clone(session),
        refresh: Some(sender),
        worker: Some(worker),
    }))
}

impl HostView {
    fn snapshot(&self) -> Arc<Committed> {
        if let Some(sender) = &self.refresh {
            // A queued refresh covers every reader; a busy storage worker
            // never grows the queue or blocks the event loop.
            let _ = sender.try_send(());
        }
        Arc::clone(
            &self
                .session
                .committed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

impl SettingsSource for HostView {
    fn load(&self) -> Map<String, Value> {
        self.snapshot().document.clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        // Storage workers retain the original durable result/error contract.
        self.session.update_section(name, values)
    }

    fn revision(&self) -> u64 {
        self.snapshot().revision
    }

    fn changes(&self) -> &ChangeSignal {
        self.session.changes()
    }
}

impl Drop for HostView {
    fn drop(&mut self) {
        self.refresh.take();
        if let Some(worker) = self.worker.take()
            && worker.thread().id() != std::thread::current().id()
        {
            // Every normal owner of the private worker joins it. A callback
            // dropping its own view must not try to join the current thread.
            let _ = worker.join();
        }
    }
}
