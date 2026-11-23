//! What changed in the records of a workspace this desktop holds: the feed a watch of the whole
//! workspace reads through the MCP server ([`tilo_actions::remote::ChangeFeed`]), from the
//! device's workspace service, as a server's workspace answers it from its own
//! ([`ui::history_source::feed`]).
//!
//! A caller follows the workspace it works in: an agent's run, admitted to one. Each workspace's
//! log is kept for as long as the session runs, whoever asks, so every connection reads the same
//! changes in the same order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_actions::remote::{
    ActionCallError, CHANGE_FEED_UNAVAILABLE, Caller, ChangeFeed, ChangePage, Pending,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use ui::history_source::feed::{ChangeLog, DocumentKey};

/// The documents of the workspaces this device's service holds, and each workspace's log.
#[derive(Default)]
pub(crate) struct Feeds {
    reads: OnceLock<Arc<dyn DeviceDocuments>>,
    changes: OnceLock<crate::platform::records::Changes>,
    logs: Mutex<HashMap<String, ChangeLog>>,
}

impl Feeds {
    /// Read the workspaces' documents from `reads` from now on, and wake a waiter on what
    /// `changes` notes.
    pub(crate) fn keep(
        &self,
        reads: Arc<dyn DeviceDocuments>,
        changes: crate::platform::records::Changes,
    ) {
        let _ = self.reads.set(reads);
        let _ = self.changes.set(changes);
    }

    /// What wakes a waiter when a write lands in one of this device's workspaces: the stream of
    /// record changes, which the session tells every landing from another device or app to.
    /// `None` before the session opened its service, and a waiter then waits until its own
    /// deadline.
    #[cfg(target_os = "linux")]
    pub(crate) fn watching(&self) -> Option<Pending<()>> {
        let reader = self.changes.get()?.reader();
        let runtime = tilo_rt::try_app().ok()?;
        let (changed, change) = tilo_actions::remote::pending();
        let fired = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = std::sync::Arc::clone(&fired);
        drop(runtime.spawn_blocking(move || {
            // Ends when the stream does: the session is gone.
            let _ = reader.recv();
            signal.notify_one();
        }));
        drop(runtime.spawn(async move {
            tokio::select! {
                () = fired.notified() => changed.complete(()),
                // Nobody reads the answer any more: nothing to wake.
                () = changed.abandoned() => {},
            }
        }));
        Some(change)
    }

    /// A host with one thread wakes no waiter: it waits until its own deadline.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn watching(&self) -> Option<Pending<()>> {
        None
    }
}

impl ChangeFeed for Feeds {
    fn since(
        &self,
        caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionCallError>> {
        Pending::ready(self.page(caller, after))
    }
}

impl Feeds {
    fn page(&self, caller: &Caller, after: Option<&str>) -> Result<ChangePage, ActionCallError> {
        let Some(workspace) = caller.workspace() else {
            return Err(ActionCallError::Unavailable {
                reason: "a watch of a workspace follows the workspace its caller works in, and \
                         this caller works in none"
                    .into(),
            });
        };
        let Some(reads) = self.reads.get() else {
            return Err(ActionCallError::Unavailable {
                reason: CHANGE_FEED_UNAVAILABLE.into(),
            });
        };
        let records: Vec<DocumentKey> =
            reads
                .listing(workspace)
                .map_err(|reason| ActionCallError::Unavailable {
                    reason: format!("the workspace's records are not read: {reason}"),
                })?;
        let mut logs = self.logs.lock_recover();
        let log = logs.entry(workspace.to_owned()).or_default();
        // One read a record: the revision and the bytes it stands at are one answer.
        let held = std::cell::RefCell::new(None);
        log.take_in(
            &records,
            |(app, record)| match reads.read(workspace, app, record, None) {
                Ok(DocumentRead::Whole {
                    revision, bytes, ..
                }) => {
                    *held.borrow_mut() = Some(bytes);
                    Some(revision)
                }
                _ => None,
            },
            |_| held.borrow_mut().take(),
            &ui::history_source::ThreadWords,
        );
        let page = log.page(after, caller.acting().member_id());
        drop(logs);
        Ok(page)
    }
}
