//! `tilo mcp`'s agent in its workspace: each record it makes or changes opens
//! as a window on the workspace's desk, as a window of the person's naming
//! the run, so everyone in the workspace sees what it did as they see a
//! colleague's work, and following the agent brings that window forward.
//! Reads open nothing, and a record already open keeps its one window.
//!
//! What the agent is doing is the host's to say, not the agent's: the host
//! that executes the session's run shows it present, named as the host knows
//! it, says what each call is at and narrates each action as it goes
//! (`tilo-agent-participant`'s run voice). A client says nothing of itself,
//! so the person sees one entry per run, never a second one the client
//! claims.

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost as _, Caller, InvokeArgument,
    InvokeRequest, ReceiptStatus, application_action_target, application_record_target,
    settle_with,
};
use tilo_relay_protocol::live::LiveRecord;
use tilo_session_core::remote::SessionActions;

use crate::mcp::ActedRecord;

/// Longest the agent waits for the host to open a record it changed as a
/// window before it goes on.
const OPENING: tilo_clock::Duration = tilo_clock::Duration::from_secs(5);

/// The windows of the records an agent makes or changes, opened on the
/// workspace's desk as it changes each.
pub struct AgentWindows {
    /// The host the agent acts through, which opens the windows.
    actions: SessionActions,
    /// Who asks it: the person the agent acts for.
    caller: Caller,
    /// The workspace, as its records' targets name it.
    cluster: String,
}

impl AgentWindows {
    /// The windows `person`'s agent opens in the workspace `cluster`, through
    /// the host `actions` reaches.
    #[must_use]
    pub fn new(actions: SessionActions, person: String, cluster: String) -> Self {
        Self {
            actions,
            caller: Caller::local_person(person),
            cluster,
        }
    }

    /// The agent's call made or changed `changed`: each opens as a window,
    /// said to be opened in `run`, the run the call was a step of
    /// ([`crate::mcp::McpServer::run_of`]), and is waited for while the host
    /// opens it. Whether it opened is no
    /// matter to the agent's work: a record the host cannot show is still
    /// what the agent changed.
    pub async fn changed(&self, changed: Vec<ActedRecord>, run: Option<tilo_actions::WorkGroup>) {
        for record in changed {
            self.open(
                &LiveRecord {
                    app: record.app,
                    record: record.record,
                },
                run.clone(),
            )
            .await;
        }
    }

    /// Open `focus` as a window on the workspace's desk, asked of the host
    /// the agent acts through ([`open_request`]); the host leaves a record a
    /// window already shows where the people put it.
    async fn open(&self, focus: &LiveRecord, run: Option<tilo_actions::WorkGroup>) {
        let request = open_request(&self.cluster, focus, run);
        let invocation = request.invocation;
        let Some(Ok(receipt)) = self.actions.invoke(&self.caller, request).await else {
            return;
        };
        if matches!(receipt.status, ReceiptStatus::Accepted) {
            let until = tilo_clock::Instant::now()
                .checked_add(OPENING)
                .unwrap_or_else(tilo_clock::Instant::now);
            let _ = settle_with(&self.actions, &self.caller, invocation, None, until).await;
        }
    }
}

/// What opens the record `focus` names in workspace `cluster`: its app's
/// `open` on that record, or on the app's own record when it names none, as a
/// step of `run`: the window says which run opened it.
fn open_request(
    cluster: &str,
    focus: &LiveRecord,
    run: Option<tilo_actions::WorkGroup>,
) -> InvokeRequest {
    let resource = format!("{APPLICATION_RESOURCE_PREFIX}{}", focus.app);
    let target = if focus.record.is_empty() {
        application_action_target(cluster, &focus.app, resource, APPLICATION_OPEN_ACTION)
    } else {
        application_record_target(
            cluster,
            &focus.app,
            &focus.record,
            resource,
            APPLICATION_OPEN_ACTION,
        )
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::None,
        context: run.map(|run| tilo_actions::ActionContext {
            group: Some(run),
            ..tilo_actions::ActionContext::default()
        }),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}
