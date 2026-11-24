//! The records a connection reads and changes through the session's API: the ones this session's
//! own services keep for the packages it runs (the shell), which answer on the loop's pass, and the
//! workspace's, which the device's daemon answers.

use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, Caller};
use tilo_lock::MutexRecover as _;

use super::{ApiAsk, SessionServices, WAIT, unanswered};

/// The records a connection reads and changes as any app's: those the session's services keep for
/// packages it runs itself (the shell), on the loop's pass, and the workspace's through the device's
/// dispatch.
pub(crate) struct ServiceRecords {
    pub(crate) state: Option<std::sync::Weak<super::Dispatch>>,
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

    /// The device's reader of the workspaces' records, which decides each read as the caller.
    fn workspace_reads(
        &self,
    ) -> Result<Arc<dyn tilo_actions::remote::ResourceReads>, ActionCallError> {
        self.state
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map(|dispatch| dispatch as Arc<dyn tilo_actions::remote::ResourceReads>)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned(),
            })
    }
}

impl tilo_actions::remote::ResourceReads for ServiceRecords {
    /// Every app's resources are in the workspace's one store, whichever service publishes
    /// them: the broker answers.
    fn store_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
        read: serde_json::Value,
    ) -> tilo_actions::remote::ReadsAnswer<'a, serde_json::Value> {
        tilo_actions::remote::reads_answer(async move {
            self.workspace_reads()?
                .store_read(caller, address, read)
                .await
        })
    }

    /// The resources the session's services publish themselves (the device's sound, its windows,
    /// its styles, Settings' sections) are answered as a reading on the loop's pass, where the
    /// services are; every other address is an app's record, answered by the workspace.
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, tilo_actions::remote::ResourceCopy> {
        tilo_actions::remote::reads_answer(async move {
            if let Some(copy) = self.published(caller, address).await? {
                return Ok(copy);
            }
            self.workspace_reads()?.read(caller, address).await
        })
    }

    /// A workspace's records change through the broker, which wakes its readers; what the session's
    /// services publish is woken by the pass that moves it ([`SessionServices::watch_reading`]).
    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
        wake: &'a tilo_actions::remote::ResourceWake,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        tilo_actions::remote::reads_answer(async move {
            if super::published_root(&address.app, &address.record) {
                self.session.watch_reading(wake);
                return Ok(());
            }
            self.workspace_reads()?.watch(caller, address, wake).await
        })
    }
}

impl ServiceRecords {
    /// The reading of `address` when it names a resource the session publishes itself, read by the
    /// person at this device as their own; `None` when it names an app's record, or when the
    /// caller is not the person: the device's daemon answers both, deciding the read as the caller.
    async fn published(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<Option<tilo_actions::remote::ResourceCopy>, ActionCallError> {
        if !super::published_root(&address.app, &address.record) || !caller.acting().is_person() {
            return Ok(None);
        }
        let Some(published) = self
            .session
            .reading_awaited(&address.app, &address.record)
            .await?
        else {
            return Ok(Some(tilo_actions::remote::ResourceCopy::Absent));
        };
        // A resource no service publishes a kind for has no node to decide a read at, so it is
        // read by nobody.
        if published.kind.is_none() {
            return Ok(Some(tilo_actions::remote::ResourceCopy::Absent));
        }
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
        Err(ActionCallError::Unavailable {
            reason: "this desktop applies no task's changes at a record's origin".to_owned(),
        })
    }
}
