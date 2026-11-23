//! What waits for a person's yes on this host.
//!
//! An action the caller's grants leave to a person is refused with what they would be shown
//! ([`ActionCallError::ConfirmationRequired`]) unless the call carries their approve
//! ([`InvokeRequest::approval`]). Whoever dispatches the call keeps it as its own asked
//! `tilo.invocations` record ([`AskedRecords`]); nothing here keeps a question, waits for an
//! answer or asks a device. [`asked::WAITING`]`#`[`asked::LIST`] lists the asked records a
//! person may answer, across the workspaces here, read from those records as they sync.

use std::sync::Arc;

use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::remote::asked::{
    self, AskedBy, AskedFacts, AskedList, AskedRecords, Named, RecordRef,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::Workspaces;
use super::person_settings::answer;

/// Who asks, as the system names `caller`: its agent by the name its key goes by, else the
/// person's own device.
#[must_use]
pub(crate) fn asked_by(caller: &Caller) -> AskedBy {
    match caller.acting().agent_id() {
        Some(agent) => AskedBy::Agent(Named {
            id: agent.to_owned(),
            name: caller.agent_called().unwrap_or(agent).to_owned(),
        }),
        None => AskedBy::default(),
    }
}

/// Go on where the person approved `request`; else it waits for their yes, showing them
/// `facts`.
///
/// # Errors
/// [`ActionCallError::ConfirmationRequired`] with `facts`, for a call nobody approved.
pub(crate) fn approved_or_asks(
    request: &InvokeRequest,
    facts: impl FnOnce() -> AskedFacts,
) -> Result<(), ActionCallError> {
    if request.approval().is_some() {
        Ok(())
    } else {
        Err(ActionCallError::asks(facts()))
    }
}

impl Workspaces {
    /// Keep every call that waits for a person's yes as its asked record through `records`:
    /// the workspaces' store, which the host hands this once.
    pub fn keep_asked_in(&self, records: Arc<dyn AskedRecords>) {
        let _ = self.inner.asked_records.set(records);
    }

    /// Where calls that wait for a person's yes are kept, once the host named it.
    pub(crate) fn asked_records(&self) -> Option<Arc<dyn AskedRecords>> {
        self.inner.asked_records.get().cloned()
    }

    /// Keep `request`, which `caller` made and which waits for its person's yes showing them
    /// `facts`, as its asked record: in the workspace the call acts in, else in the person's
    /// own workspace here. The person asked is the request's own, as its caller names them,
    /// never whoever admitted the connection it came on. `None` where no store here keeps
    /// asked records.
    pub(crate) async fn keep_asked(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Option<Result<RecordRef, String>> {
        let records = self.asked_records()?;
        let workspace = match workspace_id {
            Some(id) => self.workspace_name(id).await,
            None => caller
                .user_public_key()
                .and_then(|person| self.own_workspace_of(person)),
        };
        Some(match workspace {
            Some(workspace) => records.write_asked(&workspace, caller, request, facts),
            None => Err("no workspace here keeps it for its person".to_owned()),
        })
    }

    /// Whether a grant in `workspace`'s policy lets `agent` take `node`, an action that
    /// reaches outside, without its person's yes each time: a grant decides it, never a
    /// default.
    pub async fn granted(&self, workspace: &str, agent: &str, node: &TreePath) -> bool {
        let Some(policy) = self.policy_of(workspace).await else {
            return false;
        };
        let decision = decide(
            policy.grants.iter(),
            &[Grantee::agent(agent)],
            node,
            ActionKind::Outside,
            &Context::at(tilo_clock::unix_now_ms()),
        );
        decision.answer == Answer::Allow && decision.grant().is_some()
    }

    /// Answer a [`asked::PROVIDER`] request: every asked record the calling person may answer.
    pub(super) fn asking_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let address = &request.target.address;
        if (address.resource.as_str(), address.action.as_str()) != (asked::WAITING, asked::LIST) {
            return Err(ActionCallError::UnknownAction);
        }
        let questions = self
            .asked_records()
            .map(|records| records.asked_of(&person))
            .unwrap_or_default();
        answer(&Ok::<_, String>(AskedList { questions }))
    }
}
