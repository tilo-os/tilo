//! What changed in the records of a workspace this desktop holds: the feed a watch of the whole
//! workspace reads through the MCP server ([`tilo_actions::remote::ChangeFeed`]), the device's
//! own feed ([`tilo_workspace_service::change_feed`]) over the documents the session reads, and
//! what wakes a watch when a write lands.

use std::sync::{Arc, OnceLock};

use tilo_actions::remote::{
    ActionCallError, CHANGE_FEED_UNAVAILABLE, Caller, ChangeFeed, ChangePage, Pending,
};
use tilo_peer_state::workspace_documents::DeviceDocuments;
use tilo_workspace_service::change_feed::DocumentsFeed;

/// The feed of the workspaces this device's service holds, once the session reads them.
#[derive(Default)]
pub(crate) struct Feeds {
    feed: OnceLock<(Arc<dyn DeviceDocuments>, DocumentsFeed)>,
    changes: OnceLock<crate::platform::records::Changes>,
}

impl Feeds {
    /// Read the workspaces' documents from `reads` from now on, and wake a waiter on what
    /// `changes` notes.
    pub(crate) fn keep(
        &self,
        reads: Arc<dyn DeviceDocuments>,
        changes: crate::platform::records::Changes,
    ) {
        let _ = self.feed.set((
            reads,
            DocumentsFeed::new(Arc::new(ui::history_source::ThreadWords)),
        ));
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
        let Some((reads, feed)) = self.feed.get() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: CHANGE_FEED_UNAVAILABLE.into(),
            }));
        };
        Pending::ready(feed.page_of(reads.as_ref(), caller, after))
    }
}
