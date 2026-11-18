//! A device placing its own workspaces on the workspace service it runs: the service
//! creates the workspace and issues the device the invitation its replica
//! redeems, through the host's own `tilo.host` operations, asked in process
//! exactly as a server is asked over a session. The desktop session and the
//! standalone runtime place their workspaces through this one path.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
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

    /// Ask `workspace`'s governance action `action` with `arguments`, as
    /// `caller`, one of the workspace's member devices on this device, in
    /// process: what a member asks the workspace's holder over a session,
    /// decided by the same rules.
    ///
    /// # Errors
    /// The workspace refused or failed the action, or dropped it.
    pub async fn ask_workspace(
        &self,
        caller: &DeviceCert,
        workspace: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        self.ask(
            caller,
            RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: ActionAddress::new(
                    tilo_actions::remote::workspace_governance_resource(workspace),
                    action,
                ),
            },
            arguments,
        )
        .await
    }

    async fn ask(
        &self,
        caller: &DeviceCert,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let invoke = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target,
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let pending = self
            .actions()
            .serve_host(
                &local_caller(caller),
                caller,
                RemoteActionMessage::Invoke(invoke),
            )
            .ok_or("this workspace service does not answer in process")?;
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

    /// Place `workspace`, a workspace called `name`, on this host for `caller`, a
    /// device of the host's own person: the host creates the workspace unless it
    /// holds it already (a placement cut short after the workspace was made goes
    /// on from there, the workspace it is, never another), and answers the
    /// invitation, as text, that `caller`'s replica redeems to hold it.
    ///
    /// # Errors
    /// The host refused to create or enter the workspace, or made another workspace.
    pub async fn place_own_workspace(
        &self,
        workspace: &str,
        name: &str,
        caller: &DeviceCert,
    ) -> Result<String, String> {
        if self.workspaces().summary(workspace).await.is_err() {
            let mut arguments = Map::new();
            arguments.insert("name".into(), name.into());
            arguments.insert("workspace".into(), workspace.into());
            let created: CreatedWorkspace =
                answer(self.ask_own(caller, CREATE_WORKSPACE, arguments).await?)?;
            if created.workspace.workspace != workspace {
                return Err(
                    "this device's host made another workspace than the workspace's".into(),
                );
            }
        }
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

fn answer<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("the host answered unexpectedly: {error}"))
}
