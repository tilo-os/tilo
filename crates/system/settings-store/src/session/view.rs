//! The host's last durably accepted settings, independent of storage locks.
//!
//! Core `SessionSettings` reads retain their synchronous external-copy contract.
//! The native event loop installs this view instead: a read whose `stat` of
//! the copy finds it changed (or too recent to tell) enqueues bounded refresh
//! work, while the existing file watcher also incorporates writes.
//!
//! The worker sleeps with no timeout, and reads of an unchanged copy do not
//! wake it: an idle desktop reads settings at each clock-minute repaint, and
//! each of those reads used to. Only a host whose copy watch could not start
//! asks it to look at the copy on a timer as well.

use std::sync::{Arc, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Map, Value};

use super::{Committed, SessionSettings};
use crate::{ChangeOrigin, ChangeSignal, ConditionalUpdate, SectionChange, SettingsSource};

struct HostView {
    session: Arc<SessionSettings>,
    refresh: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

/// Start the view. With `copy_poll`, the worker also takes in the copy that
/// often when no read asks it to: for a host that cannot watch the copy.
pub(super) fn start(
    session: &Arc<SessionSettings>,
    copy_poll: Option<Duration>,
) -> std::io::Result<Arc<dyn SettingsSource>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let session_weak = Arc::downgrade(session);
    let worker = tilo_rt::spawn_thread("tilo-settings-read", move || {
        loop {
            let next = match copy_poll {
                Some(interval) => match receiver.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => true,
                    Err(mpsc::RecvTimeoutError::Disconnected) => false,
                },
                None => receiver.recv().is_ok(),
            };
            if !next {
                break;
            }
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
        if let Some(sender) = &self.refresh
            && self.session.copy_may_have_changed()
        {
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

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        self.session.update_section_by(name, values, origin)
    }

    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        self.session.update_section_with(name, origin, change)
    }

    fn update_section_report(
        &self,
        name: &str,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.session.update_section_report(name, values)
    }

    fn compare_update_section(
        &self,
        name: &str,
        expected: &Map<String, Value>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.session.compare_update_section(name, expected, values)
    }

    fn compare_display_update(
        &self,
        update: crate::DisplayUpdate,
    ) -> std::io::Result<ConditionalUpdate> {
        self.session.compare_display_update(update)
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
