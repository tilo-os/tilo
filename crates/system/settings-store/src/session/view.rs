//! The host's last durably accepted settings, independent of storage locks.
//!
//! Core `SessionSettings` reads retain their synchronous external-copy contract.
//! The native event loop installs this view instead, and a read of it
//! touches no file: the compositor's frame loop reads settings, and it never
//! waits on the disk. Another process's write to the copy reaches the view
//! through the host's watch of the copy calling [`SessionSettings::refresh`],
//! or, for a host whose watch could not start, through the worker looking at
//! the copy on a timer.
//!
//! The worker sleeps with no timeout otherwise, and reads never wake it: an
//! idle desktop reads settings at each clock-minute repaint, and each of
//! those reads used to.

use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Map, Value};

use super::{Committed, SessionSettings};
use crate::{ChangeOrigin, ChangeSignal, ConditionalUpdate, SectionChange, SettingsSource};
use tilo_lock::MutexRecover as _;

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
            // Writes the host's loop accepted without waiting, then the
            // copy another process may have written.
            session.write_pending();
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
    /// Accept a write from a thread that must not wait for the disk
    /// ([`crate::defer_writes_on_this_thread`]): readers see it now and the
    /// worker makes it durable.
    fn accept_deferred(&self, name: &str, origin: &ChangeOrigin, change: &mut SectionChange<'_>) {
        self.session.accept_deferred(name, origin, change);
        if let Some(sender) = &self.refresh {
            let _ = sender.try_send(());
        }
    }

    fn snapshot(&self) -> Arc<Committed> {
        Arc::clone(&self.session.committed.lock_recover())
    }
}

impl SettingsSource for HostView {
    fn load(&self) -> Map<String, Value> {
        self.snapshot().document.clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        if crate::writes_deferred_on_this_thread() {
            let mut values = Some(values);
            self.accept_deferred(name, &ChangeOrigin::default(), &mut |_| {
                values.take().unwrap_or_default()
            });
            return Ok(());
        }
        // Storage workers retain the original durable result/error contract.
        self.session.update_section(name, values)
    }

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        if crate::writes_deferred_on_this_thread() {
            let mut values = Some(values);
            self.accept_deferred(name, origin, &mut |_| values.take().unwrap_or_default());
            return Ok(());
        }
        self.session.update_section_by(name, values, origin)
    }

    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        if crate::writes_deferred_on_this_thread() {
            self.accept_deferred(name, origin, change);
            return Ok(());
        }
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

    fn flush_pending(&self) {
        self.session.write_pending();
    }

    fn take_deferred_failures(&self) -> Vec<crate::DeferredFailure> {
        self.session.take_deferred_failures()
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
        // Whatever the host's loop accepted without waiting is made durable
        // before the view goes.
        self.session.write_pending();
    }
}
