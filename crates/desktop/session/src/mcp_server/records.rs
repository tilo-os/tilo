//! The records a connection reads and changes through the session's API: the ones this session's
//! own services keep for the packages it runs (the shell), which answer on the loop's pass, and the
//! workspace's, which its broker answers.

use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, Caller};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::collab_app_broker::AppBrokerState;

use super::{ApiAsk, SessionServices, WAIT, unanswered};

/// The records a connection reads and changes as any app's: those the session's services keep for
/// packages it runs itself (the shell), on the loop's pass, and the workspace's through the broker.
pub(crate) struct ServiceRecords {
    pub(crate) state: Option<std::sync::Weak<AppBrokerState>>,
    pub(crate) session: Arc<SessionServices>,
    /// The agent session an admitted connection is, when it is one: the records it changes are
    /// kept there, as what taking its work back undoes.
    pub(crate) agent_session: Option<(
        Arc<crate::agent_sessions::AgentSessions>,
        tilo_action_policy::AgentSessionId,
    )>,
}

impl ServiceRecords {
    /// The session's answer to `ask` about `app`'s record, on its pass: `None` when no service keeps
    /// it, and then without waiting for a pass. A session that does not answer may still act on
    /// the ask: answering from the workspace's copy instead would answer from somewhere else.
    fn on_pass<T>(
        &self,
        app: &str,
        ask: impl FnOnce(tilo_actions::remote::Completer<Option<T>>) -> ApiAsk,
    ) -> Result<Option<T>, ActionCallError> {
        if !self.session.kept.lock_recover().contains(app) {
            return Ok(None);
        }
        let (done, answered) = tilo_actions::remote::pending();
        self.session.ask(ask(done));
        answered
            .wait_timeout(WAIT)
            .map(Option::flatten)
            .map_err(|_| unanswered())
    }

    fn broker(&self) -> Result<Arc<AppBrokerState>, ActionCallError> {
        self.state
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this desktop keeps no records of its own".to_owned(),
            })
    }

    fn workspace_reads(
        &self,
    ) -> Result<Arc<dyn tilo_actions::remote::ResourceReads>, ActionCallError> {
        self.broker()?
            .resource_reads()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned(),
            })
    }
}

impl tilo_actions::remote::ResourceReads for ServiceRecords {
    /// Every app's resources are in the workspace's one store, whichever service publishes
    /// them: the broker answers.
    fn store_read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        self.workspace_reads()?.store_read(caller, address, read)
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    /// The resources the session's services publish themselves (the device's sound, its windows,
    /// its styles, Settings' sections) are answered as a reading on the loop's pass, where the
    /// services are; every other address is an app's record, answered by the workspace.
    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        if let Some(copy) = self.published(caller, address)? {
            return Ok(copy);
        }
        self.workspace_reads()?.read(caller, address)
    }

    /// A workspace's records change through the broker, which wakes its readers; what the session's
    /// services publish is woken by the pass that moves it ([`SessionServices::watch_reading`]).
    fn watch(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        if super::published_root(&address.app, &address.record) {
            self.session.watch_reading(wake);
            return Ok(());
        }
        self.workspace_reads()?.watch(caller, address, wake)
    }
}

impl ServiceRecords {
    /// The reading of `address` when it names a resource the session publishes itself, decided for
    /// `caller` as every read of it is; `None` when it names an app's record, which the workspace
    /// answers.
    fn published(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<Option<tilo_actions::remote::ResourceCopy>, ActionCallError> {
        if !super::published_root(&address.app, &address.record) {
            return Ok(None);
        }
        let Some(published) = self.session.reading(&address.app, &address.record)? else {
            return Ok(Some(tilo_actions::remote::ResourceCopy::Absent));
        };
        // A read is decided at the resource's node, which its published kind names: a resource no
        // service publishes a kind for has no node to decide it at, so it is read by nobody.
        let Some(kind) = &published.kind else {
            return Ok(Some(tilo_actions::remote::ResourceCopy::Absent));
        };
        let state = self.state.as_ref().and_then(std::sync::Weak::upgrade);
        super::may_read_published(
            state.as_deref(),
            caller,
            (super::SESSION_APP, kind, &address.record),
        )?;
        Ok(Some(tilo_actions::remote::ResourceCopy::reading(
            published.reading,
            None,
        )))
    }
}

impl tilo_actions::remote::ResourceChanges for ServiceRecords {
    fn apply(
        &self,
        caller: &Caller,
        change: &tilo_actions::remote::ResourceChange,
    ) -> Result<serde_json::Value, ActionCallError> {
        // The session hears of every record this connection changes, whichever end applies it:
        // one of the records taking its work back undoes.
        if let Some((sessions, session)) = &self.agent_session {
            sessions.changed(session, &change.app, &change.record);
        }
        let asked = (caller.acting().clone(), change.clone());
        if let Some(answer) = self.on_pass(&change.app, |done| ApiAsk::Change {
            actor: asked.0,
            change: asked.1,
            done,
        })? {
            return answer;
        }
        self.broker()?
            .resource_changes()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this desktop applies no task's changes at a record's origin".to_owned(),
            })?
            .apply(caller, change)
    }
}
