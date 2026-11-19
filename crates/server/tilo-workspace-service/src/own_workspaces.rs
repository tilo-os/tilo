//! A device placing its own workspaces on the workspace service it runs: the service
//! creates the workspace and issues the device the invitation its replica
//! redeems, through the host's own `tilo.host` operations, asked in process
//! exactly as a server is asked over a session. The desktop session and the
//! standalone runtime place their workspaces through this one path.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_server_session::workspaces::{
    CREATE_WORKSPACE, CreatedWorkspace, ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER,
    HOST_WORKSPACES_RESOURCE,
};
use tilo_user_identity::DeviceCert;

use crate::service::{WorkspaceService, local_caller};

/// How long the invitation a device redeems into a workspace placed on its own
/// host stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;

impl WorkspaceService {
    /// Ask this host its own operation `action` with `arguments`, as
    /// `caller`, a device of the host's own person, in process: the result
    /// the operation answered.
    ///
    /// # Errors
    /// The host refused or failed the operation, or dropped it.
    pub async fn ask_own(
        &self,
        caller: &DeviceCert,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        self.ask(
            caller,
            // The provider is named: no catalogue lists it.
            RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
            },
            arguments,
        )
        .await
    }

    /// Ask `target` as `caller`, outside any workspace: the service's own operations are its own,
    /// and only its workspaces' governance is theirs.
    async fn ask(
        &self,
        caller: &DeviceCert,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let pending = self
            .actions()
            .serve_host(&local_caller(caller), caller, message(target, arguments))
            .ok_or("this workspace service does not answer in process")?;
        answered(pending).await
    }

    /// Place `workspace`, a workspace called `name`, on this host for `caller`, a
    /// device of the host's own person: the host creates the workspace unless it
    /// holds it already (a placement cut short after the workspace was made goes
    /// on from there, the workspace it is, never another), and answers the
    /// invitation, as text, that `caller`'s replica redeems to hold it.
    ///
    /// # Errors
    /// The host refused to create or enter the workspace, or made another workspace.
    /// Hold `workspace`, a workspace called `name`, on this host for `caller`,
    /// a device of the host's own person: the host creates the workspace
    /// unless it holds it already. The embedder writes into it through the
    /// host, never into a replica of its own
    /// ([`crate::workspaces::Workspaces::commit_own_document`]).
    ///
    /// # Errors
    /// The host refused to create the workspace, or made another workspace.
    pub async fn hold_own_workspace(
        &self,
        workspace: &str,
        name: &str,
        caller: &DeviceCert,
    ) -> Result<(), String> {
        if self.workspaces().summary(workspace).await.is_ok() {
            return Ok(());
        }
        let mut arguments = Map::new();
        arguments.insert("name".into(), name.into());
        arguments.insert("workspace".into(), workspace.into());
        let created: CreatedWorkspace =
            answer(self.ask_own(caller, CREATE_WORKSPACE, arguments).await?)?;
        if created.workspace.workspace != workspace {
            return Err("this device's host made another workspace than the workspace's".into());
        }
        Ok(())
    }

    pub async fn place_own_workspace(
        &self,
        workspace: &str,
        name: &str,
        caller: &DeviceCert,
    ) -> Result<String, String> {
        self.hold_own_workspace(workspace, name, caller).await?;
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), workspace.into());
        arguments.insert("expires_in_seconds".into(), OWN_INVITATION_SECONDS.into());
        let entered: EnteredWorkspace =
            answer(self.ask_own(caller, ENTER_WORKSPACE, arguments).await?)?;
        Ok(tilo_peer_state::join::encode_invitation_text(
            &entered.invitation.decode()?.signed_invitation,
        ))
    }
}

/// What the service answered `pending` with: the value the operation answered, or why it did not.
async fn answered(pending: Pending<RemoteActionMessage>) -> Result<Value, String> {
    match pending.await {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Rejected { error } => Err(format!("refused: {error}")),
            ReceiptStatus::Failed { error } => Err(error.message),
            ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
                Err("the request did not finish".into())
            }
        },
        Some(_) => Err("the workspace service answered with something else".into()),
        None => Err("the workspace service dropped the request".into()),
    }
}

/// The question `target` with `arguments` asks, as a connection asks the service.
fn message(target: RemoteActionTarget, arguments: Map<String, Value>) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

fn answer<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("the host answered unexpectedly: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::tests::{governor, test_host};
    use tilo_actions::remote::ActionProvider as _;

    /// A member device asks its workspace's governance on the device action service, as its
    /// holder's own device asks it in process: an invitation made there reaches it, with the
    /// invitation another device joins by.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the service the test binds is shut down at its end, not dropped early"
    )]
    async fn a_member_asks_its_workspaces_governance_on_the_device_action_service() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let created = server.workspaces().create("Home").await.unwrap();
        let mut arguments = Map::new();
        arguments.insert("role".to_owned(), "editor".into());
        let RemoteActionMessage::Invoke(request) = message(
            RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: ActionAddress::new(
                    tilo_actions::remote::workspace_governance_resource(created.workspace.as_str()),
                    tilo_action_policy::roles::governance::INVITE,
                ),
            },
            arguments,
        ) else {
            unreachable!("a question is an invocation");
        };
        let caller = local_caller(&cert).in_workspace(created.workspace.as_str());
        let receipt = server
            .host()
            .workspaces()
            .device_actions()
            .invoke(&caller, request)
            .await
            .expect("the governance answers")
            .expect("the workspace takes the request");
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the workspace issues the invitation: {:?}", receipt.status);
        };
        let invitation = output
            .value
            .as_ref()
            .and_then(|issued| issued.get("invitation"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !invitation.is_empty(),
            "the invitation another device joins by: {output:?}"
        );
        server.shutdown().await;
    }
}
