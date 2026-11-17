//! What changed in the records of a workspace this desktop holds: the feed a watch of the whole
//! workspace reads through the MCP server ([`tilo_actions::remote::ChangeFeed`]), from the
//! device's replica of the workspace, as a relay's workspace answers it from its own
//! ([`ui::history_source::feed`]).
//!
//! A caller follows the workspace it works in: an agent's run, admitted to one. Each workspace's
//! log is kept for as long as the session runs, whoever asks, so every connection reads the same
//! changes in the same order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_actions::remote::{
    ActionHostError, CHANGE_FEED_UNAVAILABLE, Caller, ChangeFeed, ChangePage, Pending,
};
use tilo_lock::MutexRecover as _;
use tilo_session_client::engine::Engine;
use ui::history_source::feed::{ChangeLog, DocumentKey};

/// The device's replica of its workspaces, once the session opened it, and each workspace's log.
#[derive(Default)]
pub(crate) struct Feeds {
    engine: OnceLock<Arc<Engine>>,
    logs: Mutex<HashMap<String, ChangeLog>>,
}

impl Feeds {
    /// Read the workspaces' records from `engine` from now on.
    pub(crate) fn keep(&self, engine: Arc<Engine>) {
        let _ = self.engine.set(engine);
    }

    /// What wakes a waiter when a write lands in one of this device's workspaces: the replica's
    /// own arrivals, which the session tells every landing from another device or app to.
    /// `None` before the session opened its replica, and a waiter then waits until its own
    /// deadline.
    pub(crate) fn watching(&self) -> Option<Pending<()>> {
        let engine = self.engine.get()?;
        let mut landed = engine.arrivals().watching();
        landed.mark_unchanged();
        let runtime = tilo_rt::try_app().ok()?;
        let (changed, change) = tilo_actions::remote::pending();
        drop(runtime.spawn(async move {
            tokio::select! {
                _ = landed.changed() => {}
                // Nobody reads the answer any more: nothing to wake.
                () = changed.abandoned() => return,
            }
            changed.complete(());
        }));
        Some(change)
    }
}

impl ChangeFeed for Feeds {
    fn since(
        &self,
        caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionHostError>> {
        Pending::ready(self.page(caller, after))
    }
}

impl Feeds {
    fn page(&self, caller: &Caller, after: Option<&str>) -> Result<ChangePage, ActionHostError> {
        let Some(workspace) = caller.workspace() else {
            return Err(ActionHostError::Unavailable {
                reason: "a watch of a workspace follows the workspace its caller works in, and \
                         this caller works in none"
                    .into(),
            });
        };
        let Some(engine) = self.engine.get() else {
            return Err(ActionHostError::Unavailable {
                reason: CHANGE_FEED_UNAVAILABLE.into(),
            });
        };
        let records: Vec<DocumentKey> =
            engine
                .records(workspace)
                .map_err(|reason| ActionHostError::Unavailable {
                    reason: format!("the workspace's records are not read: {reason}"),
                })?;
        let mut logs = self.logs.lock_recover();
        let log = logs.entry(workspace.to_owned()).or_default();
        log.take_in(
            &records,
            |(app, record)| {
                engine
                    .document_revision(workspace, app, record)
                    .ok()
                    .flatten()
            },
            |(app, record)| engine.record(workspace, app, record).ok().flatten(),
            &ui::history_source::ThreadWords,
        );
        let page = log.page(after);
        drop(logs);
        Ok(page)
    }
}
