//! The records a connection reads and changes through the session's API: the ones this session's
//! own services keep for the packages it runs (the shell), which answer on the loop's pass, and the
//! workspace's, which its broker answers.

use std::sync::Arc;

use tilo_action_broker::collab_app_broker::AppBrokerState;
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_lock::MutexRecover as _;

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
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        let app = address.app.clone();
        if let Some(record) = self.on_pass(&address.app, |done| ApiAsk::Record { app, done })? {
            return Ok(tilo_actions::remote::ResourceCopy::Document(
                record.document,
            ));
        }
        self.workspace_reads()?.read(caller, address)
    }

    /// A workspace's records change through the broker, which wakes its readers; the records of
    /// packages this session runs itself are read as they are.
    fn watch(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        self.workspace_reads()?.watch(caller, address, wake)
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
