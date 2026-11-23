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
use tilo_server_protocol::workspaces::{
    CREATE_WORKSPACE, CreatedWorkspace, ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER,
    HOST_WORKSPACES_RESOURCE,
};
use tilo_user_identity::DeviceCert;

use crate::service::{WorkspaceService, local_caller};

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
            .serve_host(
                &local_caller(caller),
                caller,
                RemoteActionMessage::Invoke(invocation(target, arguments)),
            )
            .ok_or("this workspace service does not answer in process")?;
        answered(pending).await
    }

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
        // The person's Servers workspace has one founder: a device whose
        // person's list says another of theirs keeps it is handed it instead.
        if workspace == tilo_server_protocol::servers::person_workspace_id(&caller.user_public_key)
        {
            crate::workspaces::person_workspace::may_found(
                self.workspaces(),
                &caller.user_public_key,
            )
            .await?;
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

    /// Hold a workspace and return the invitation this device redeems in process.
    ///
    /// # Errors
    /// The host refused to hold or enter the workspace, or made another workspace.
    pub async fn place_own_workspace(
        &self,
        workspace: &str,
        name: &str,
        caller: &DeviceCert,
    ) -> Result<String, String> {
        self.hold_own_workspace(workspace, name, caller).await?;
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), workspace.into());
        let request = invocation(
            RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), ENTER_WORKSPACE),
            },
            arguments,
        );
        let entered: EnteredWorkspace = answer(
            Box::pin(self.workspaces().enter_here(caller, &request))
                .await
                .map_err(|error| error.to_string())?,
        )?;
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
fn invocation(target: RemoteActionTarget, arguments: Map<String, Value>) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
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

    /// An agent another of the person's devices handed its session to is refused, in this
    /// device's words and with what to turn on, while this device takes no such agent.
    #[tokio::test]
    async fn another_devices_agent_is_refused_while_this_device_takes_none() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let created = server.workspaces().create("Home").await.unwrap();
        server
            .host()
            .workspaces()
            .accept_agents_from_other_devices(false);
        let handed = local_caller(&cert)
            .in_workspace(created.workspace.as_str())
            .delegated_to("claude");
        let pin = invocation(
            RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: ActionAddress::new("notes.notebook", "pin"),
            },
            Map::new(),
        );
        let refused = server
            .host()
            .workspaces()
            .device_actions()
            .invoke(&handed, pin)
            .await
            .expect("the dispatch answers")
            .expect_err("this device takes no agent from another of the person's devices");
        assert!(
            refused
                .to_string()
                .contains("Accept agents from my other devices"),
            "{refused}"
        );
        server.shutdown().await;
    }

    /// A member device asks its workspace's governance on the device action service, as its
    /// holder's own device asks it in process: an invitation made there reaches it, with the
    /// invitation another device joins by.
    #[tokio::test]
    async fn a_member_asks_its_workspaces_governance_on_the_device_action_service() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let created = server.workspaces().create("Home").await.unwrap();
        let mut arguments = Map::new();
        arguments.insert("role".to_owned(), "editor".into());
        let request = invocation(
            RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: tilo_actions::remote::workspace_governance_address(
                    created.workspace.as_str(),
                    tilo_actions::remote::WORKSPACE_INVITE,
                ),
            },
            arguments,
        );
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
