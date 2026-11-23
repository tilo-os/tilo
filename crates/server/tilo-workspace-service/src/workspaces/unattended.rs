//! Commands a key runs in a workspace's machine without the person being
//! asked each time ([`tilo_server_session::workspaces::RUN_UNATTENDED`]).
//!
//! An agent's key asks its person before each command it runs in a workspace's
//! machine. A person may let the keys that reach one workspace of their own,
//! and that workspace alone, run commands there unasked: they set a workspace
//! up once, let it run unattended, branch it per task and mint each branch a
//! key that works there on its own. A key reaching all their workspaces, their
//! own backend's included, is still asked, as is everything else that asks
//! today.
//!
//! The setting is the workspace's, kept by the host beside whose workspace it
//! is ([`super::own`]), not in any record of it: a key that writes the
//! workspace's records cannot turn it on for itself. A branch starts with the
//! source's setting, copied when the host makes it. Only the person turns it
//! on, by answering a question on their approving devices ([`AsksPeople`]);
//! any key of theirs reaching the workspace may ask, and turning it off asks
//! nobody.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_server_session::workspaces::AdminRefusal;
use tilo_user_identity::DeviceCert;

use super::Workspaces;

/// How long the person has to answer whether a workspace runs commands
/// unattended: as long as a question about one action waits.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

/// How long a request that may wait for the person is held before it is
/// answered accepted and followed instead: turning running commands
/// unattended off is answered within it, and the person takes longer.
const ANSWERED_WITHIN: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the host holds `request` for its answer when the caller named no
/// wait: [`ANSWERED_WITHIN`] for a turn of running commands unattended, which
/// may wait minutes for the person, so the caller follows it rather than
/// holding its answer open; `None` for anything else.
pub(super) fn asks_first(request: &InvokeRequest) -> Option<std::time::Duration> {
    (request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
        && request.target.address.action == tilo_server_session::workspaces::RUN_UNATTENDED)
        .then_some(ANSWERED_WITHIN)
}

/// The person's answer to a question, once they gave it or it lapsed.
pub type Answer = Pin<Box<dyn Future<Output = Result<ConfirmationAnswer, String>> + Send>>;

/// What puts a question to a person on their approving devices: the server's
/// confirmations, which only devices of that person's root answer.
pub trait AsksPeople: Send + Sync {
    /// Ask `person` `question` and wait for their answer; `Err` when nobody
    /// answered before it expired, or it could not be asked.
    fn ask(&self, person: [u8; 32], question: PendingConfirmation) -> Answer;
}

/// Unknown and forbidden are the same answer.
const fn unknown() -> ActionCallError {
    ActionCallError::UnknownAction
}

impl Workspaces {
    /// Put the questions about running commands unattended to people through
    /// `asker`. A host with none refuses to turn it on.
    pub fn ask_people(&self, asker: Arc<dyn AsksPeople>) {
        *self.inner.asks_people.write_recover() = Some(asker);
    }

    /// Whether `caller` runs a command in the machine of the workspace
    /// `workspace_id` carries without its person being asked: an agent's key
    /// whose certificate reaches that workspace alone, of the person whose own
    /// workspace it is, who let it run commands unattended. Anyone else is
    /// asked as before: a key reaching all their workspaces, a key for another
    /// workspace, an agent on a device of theirs, and any other person's key.
    pub async fn runs_unattended(&self, workspace_id: WorkspaceId, caller: &Caller) -> bool {
        // An agent's key, as the server names one: the agent is the key.
        let (Some(user), Some(device)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return false;
        };
        if caller
            .acting()
            .agent_id()
            .and_then(tilo_action_policy::key_of_agent)
            != Some(*device)
        {
            return false;
        }
        let Some(own) = self.own() else {
            return false;
        };
        let Some((workspace, certificate)) = self.member_certificate(workspace_id, device).await
        else {
            return false;
        };
        let one_workspace = certificate
            .capabilities
            .agent
            .and_then(|scope| scope.workspace_name())
            .is_some_and(|one| one == workspace);
        one_workspace
            && certificate.user_public_key == *user
            && own.owner_of(&workspace) == Some(*user)
            && own.runs_unattended(&workspace)
    }

    /// The UUID of the workspace `workspace_id` carries, and the certificate
    /// it holds for `device` among its members'.
    async fn member_certificate(
        &self,
        workspace_id: WorkspaceId,
        device: &[u8; 32],
    ) -> Option<(String, DeviceCert)> {
        let workspace = self.workspace_identity(workspace_id).await?;
        let governed = self.inner.workspaces.lock().await.get(&workspace)?.clone();
        let certificate = governed
            .lock()
            .await
            .peer()
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == *device)?;
        Some((workspace, certificate))
    }

    /// Whether the own workspace `workspace` (its UUID) runs commands
    /// unattended: what its listing says to its owner.
    pub(super) fn listed_unattended(&self, workspace: &str) -> bool {
        self.own().is_some_and(|own| own.runs_unattended(workspace))
    }

    /// A branch `branch` of `source` holds what its source holds: running
    /// commands unattended too, when the source does.
    pub(super) fn branch_unattended(&self, source: &str, branch: &str) -> Result<(), String> {
        match self.own() {
            Some(own) if own.runs_unattended(source) => own.set_unattended(branch, true),
            _ => Ok(()),
        }
    }

    /// Turn running commands unattended on or off in the workspace
    /// `workspace_id` carries, as `request` asks `{"on": bool}`, for `caller`
    /// on the device `certificate` certifies: the workspace's owner or a key
    /// of theirs reaching it. Off is set at once; on is set only once the
    /// owner allows it, asked on their approving devices, and stays off
    /// otherwise. Answers `{"workspace", "unattended"}`.
    pub(super) async fn unattended_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or_else(unknown)?;
        // The person on a device of theirs, or an agent's key of theirs; an
        // agent a device of theirs runs asks through neither.
        let asks = caller.acting().is_person()
            || (certificate.capabilities.agent.is_some()
                && caller
                    .acting()
                    .agent_id()
                    .and_then(tilo_action_policy::key_of_agent)
                    == Some(certificate.device_public_key));
        if !asks {
            return Err(unknown());
        }
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(ActionCallError::InvalidRequest {
                reason: "`on` is true or false".into(),
            });
        };
        let on = arguments
            .get("on")
            .and_then(Value::as_bool)
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: "`on` is true or false".into(),
            })?;
        let person = certificate.user_public_key;
        let own = self
            .own()
            .ok_or_else(|| refused(AdminRefusal::NotYourOwn.to_reason()))?;
        let workspace = self
            .workspace_identity(workspace_id)
            .await
            .ok_or_else(unknown)?;
        if request.target.address.resource
            != tilo_actions::remote::workspace_governance_resource(&workspace)
        {
            return Err(unknown());
        }
        let reaches = certificate
            .capabilities
            .agent
            .is_none_or(|scope| scope.reaches(&workspace));
        if !reaches || own.owner_of(&workspace) != Some(person) {
            return Err(refused(AdminRefusal::NotYourOwn.to_reason()));
        }
        let answer = |on: bool| json!({"workspace": workspace, "unattended": on});
        if !on || own.runs_unattended(&workspace) {
            own.set_unattended(&workspace, on)
                .map_err(ActionCallError::refused)?;
            return Ok(answer(on));
        }
        let name = self
            .workspace_name(workspace_id)
            .await
            .unwrap_or_else(|| workspace.clone());
        self.ask_person(person, question(caller, certificate, &name))
            .await?;
        own.set_unattended(&workspace, true)
            .map_err(ActionCallError::refused)?;
        tracing::info!(%workspace, "a workspace runs commands unattended");
        Ok(answer(true))
    }
}

/// What the person is asked: who asks, that the keys made for this workspace
/// alone would run commands in its machine without asking them each time, and
/// that it holds for the branches made from it too.
fn question(caller: &Caller, certificate: &DeviceCert, workspace: &str) -> PendingConfirmation {
    PendingConfirmation {
        id: super::actions::agents::question_id(),
        agent_id: caller.acting().agent_id().unwrap_or_default().to_owned(),
        agent_label: certificate.device_label.clone(),
        title: String::new(),
        action_label: "let keys made for this workspace alone run commands in its Linux machine \
                       without asking you each time"
            .into(),
        resource_label: format!("{workspace}, and the branches made from it"),
        arguments: Vec::new(),
        risk: "privileged".into(),
        kind: "outside".into(),
        undoable: false,
        workspace: workspace.to_owned(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
        page: None,
        ask: None,
        allow_always: false,
    }
}
