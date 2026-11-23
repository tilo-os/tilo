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
//! on: any key of theirs reaching the workspace may ask, and the call waits
//! for their yes; turning it off asks nobody.

use serde_json::{Value, json};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_server_protocol::WorkspaceId;
use tilo_server_session::workspaces::Refusal;
use tilo_user_identity::DeviceCert;

use super::Workspaces;

/// Unknown and forbidden are the same answer.
const fn unknown() -> ActionCallError {
    ActionCallError::UnknownAction
}

impl Workspaces {
    /// Whether `caller` runs a command in the machine of the workspace
    /// `workspace_id` carries without its person being asked: an agent's key
    /// whose certificate reaches that workspace alone, of the person whose own
    /// workspace it is, who let it run commands unattended. Anyone else is
    /// asked as before: a key reaching all their workspaces, a key for another
    /// workspace, an agent on a device of theirs, and any other person's key.
    pub async fn runs_unattended(&self, workspace_id: WorkspaceId, caller: &Caller) -> bool {
        // An agent's key, as the server names one: its person's agent on a
        // session its own certificate opened.
        if !(caller.acting().is_agent() && caller.is_session_delegated()) {
            return false;
        }
        let (Some(user), Some(device)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return false;
        };
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
    /// owner approves the call, and stays off otherwise. Answers `{"workspace", "unattended"}`.
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
        // agent a device handed its session to asks through neither.
        let asks = caller.acting().is_person()
            || (certificate.capabilities.agent.is_some()
                && caller.is_session_delegated()
                && caller.acting().is_agent());
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
            .ok_or_else(|| ActionCallError::refused(Refusal::NotYourOwn.to_reason()))?;
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
            return Err(ActionCallError::refused(Refusal::NotYourOwn.to_reason()));
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
        super::asking::approved_or_asks(request, || AskedFacts {
            on: Some(name.clone()),
            facts: vec![Fact::new("Also", "the branches made from it")],
            verb: "let keys made for this workspace alone run commands in its Linux machine \
                   without asking you each time"
                .into(),
            ..AskedFacts::by(super::asking::asked_by(caller))
        })?;
        own.set_unattended(&workspace, true)
            .map_err(ActionCallError::refused)?;
        tracing::info!(%workspace, "a workspace runs commands unattended");
        Ok(answer(true))
    }
}
