//! The records a connection reads and changes through the session's door: the ones this session's
//! own services keep for the packages it runs (the shell), which answer on the loop's pass, and the
//! workspace's, which its broker answers.

use std::sync::Arc;

use tilo_action_broker::collab_app_broker::AppBrokerState;
use tilo_actions::remote::{ActionHostError, Caller};
use tilo_lock::MutexRecover as _;

use super::{DoorAsk, SessionDoors, WAIT, unanswered};

/// The records a connection reads and changes as any app's: those the session's services keep for
/// packages it runs itself (the shell), on the loop's pass, and the workspace's through the broker.
pub(crate) struct ServiceRecords {
    pub(crate) state: Option<std::sync::Weak<AppBrokerState>>,
    pub(crate) session: Arc<SessionDoors>,
    /// An admitted agent's run, whose steps its changes of the workspace's records are.
    pub(crate) run: Option<Arc<dyn tilo_actions::remote::RecordChanges>>,
}

impl ServiceRecords {
    /// The session's answer to `ask` about `app`'s record, on its pass: `None` when no service keeps
    /// it, and then without waiting for a pass. A session that does not answer may still act on
    /// the ask: answering from the workspace's copy instead would answer from somewhere else.
    fn on_pass<T>(
        &self,
        app: &str,
        ask: impl FnOnce(tilo_actions::remote::Completer<Option<T>>) -> DoorAsk,
    ) -> Result<Option<T>, ActionHostError> {
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

    fn broker(&self) -> Result<Arc<AppBrokerState>, ActionHostError> {
        self.state
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this desktop keeps no records of its own".to_owned(),
            })
    }

    fn workspace_reads(
        &self,
    ) -> Result<Arc<dyn tilo_actions::remote::RecordReads>, ActionHostError> {
        self.broker()?
            .record_reads()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: tilo_actions::remote::RECORD_READS_UNAVAILABLE.to_owned(),
            })
    }
}

impl tilo_actions::remote::RecordReads for ServiceRecords {
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::RecordReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::RecordAddress,
    ) -> Result<tilo_actions::remote::RecordCopy, ActionHostError> {
        let app = address.app.clone();
        if let Some(record) = self.on_pass(&address.app, |done| DoorAsk::Record { app, done })? {
            return Ok(tilo_actions::remote::RecordCopy::Document(record.document));
        }
        self.workspace_reads()?.read(caller, address)
    }

    /// A workspace's records change through the broker, which wakes its readers; the records of
    /// packages this session runs itself are read as they are.
    fn watch(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::RecordAddress,
        wake: &tilo_actions::remote::RecordWake,
    ) -> Result<(), ActionHostError> {
        self.workspace_reads()?.watch(caller, address, wake)
    }
}

impl tilo_actions::remote::RecordChanges for ServiceRecords {
    fn apply(
        &self,
        caller: &Caller,
        change: &tilo_actions::remote::RecordChange,
    ) -> Result<serde_json::Value, ActionHostError> {
        let asked = (caller.principal().clone(), change.clone());
        if let Some(answer) = self.on_pass(&change.app, |done| DoorAsk::Change {
            actor: asked.0,
            change: asked.1,
            done,
        })? {
            return answer;
        }
        if let Some(run) = &self.run {
            return run.apply(caller, change);
        }
        self.broker()?
            .record_changes()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this desktop applies no task's changes at a record's origin".to_owned(),
            })?
            .apply(caller, change)
    }
}
