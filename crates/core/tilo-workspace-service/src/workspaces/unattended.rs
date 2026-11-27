//! Commands a key runs in a workspace's machine without the person being
//! asked each time ([`tilo_server_protocol::workspaces::RUN_UNATTENDED`]).
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
use tilo_server_protocol::workspaces::AdminRefusal;
use tilo_user_identity::DeviceCert;

use super::Workspaces;

/// Unknown and forbidden are the same answer.
const fn unknown() -> ActionCallError {
    ActionCallError::UnknownAction
}

impl Workspaces {
    /// A branch `branch` of `source` holds what its source holds: running
    /// commands unattended too, when the source does.
    pub(super) async fn branch_unattended(&self, source: &str, branch: &str) -> Result<(), String> {
        if self
            .ownership(source)
            .await
            .is_some_and(|ownership| ownership.unattended)
        {
            self.set_unattended(branch, true).await
        } else {
            Ok(())
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
                    .and_then(tilo_member_types::key_of_agent)
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
            .ok_or_else(|| ActionCallError::refused(AdminRefusal::NotYourOwn.to_reason()))?;
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
            .as_ref()
            .is_none_or(|scope| scope.reaches(&workspace));
        if !reaches || own.owner_of(&workspace) != Some(person) {
            return Err(ActionCallError::refused(
                AdminRefusal::NotYourOwn.to_reason(),
            ));
        }
        let answer = |on: bool| json!({"workspace": workspace, "unattended": on});
        if !on
            || self
                .ownership(&workspace)
                .await
                .is_some_and(|ownership| ownership.unattended)
        {
            self.set_unattended(&workspace, on)
                .await
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
        self.set_unattended(&workspace, true)
            .await
            .map_err(ActionCallError::refused)?;
        tracing::info!(%workspace, "a workspace runs commands unattended");
        Ok(answer(true))
    }
}
