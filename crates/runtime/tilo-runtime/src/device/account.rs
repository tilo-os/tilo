//! The person's account at the server their device enters through, asked as this device: the
//! server's own operations (`account/…`: the person's workspace list, and where each workspace
//! this device does not keep yet is entered), over the device action service (`tilo-actions/1`)
//! from the device's own endpoint. Each request is signed with the device's key and decided for
//! the certificate the roster of the workspace it is asked in holds for the device; no session
//! carries it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_peer_state::actions::RemoteDevice;
use tilo_peer_state::join::TransportHint;
use tilo_server_protocol::WorkspaceId;
use tilo_server_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_PERSONAL, PERSONAL_AFTER, PersonalList,
};

use super::workspaces::ServiceWorkspaces;

/// How long one call is followed before it is given up on: past the longest a read of the list
/// that follows it waits on the server.
const CALLED: Duration = Duration::from_secs(60);

/// The person's account at their server, as this device asks it.
pub(crate) struct Account {
    server: Arc<RemoteDevice>,
}

/// The person's workspace list, and the version a device following it names next.
pub(crate) struct Listed {
    pub(crate) list: PersonalList,
    pub(crate) version: String,
}

impl Account {
    /// The account at `server`, the device `server_device` (a server keys its endpoint apart
    /// from the device it signs as), asked in `workspace` through the action service of `own`,
    /// this device's workspace service.
    pub(crate) fn at(
        own: &ServiceWorkspaces,
        server: &iroh::EndpointAddr,
        server_device: [u8; 32],
        workspace: &str,
    ) -> Self {
        let actions = own.copies().action_service();
        actions.reach_device_at(server_device, *server.id.as_bytes());
        Self {
            server: Arc::new(actions.remote(&own.endpoint(), server, workspace)),
        }
    }

    /// Call the server's operation `action` with `arguments`: what it answered, or why not.
    ///
    /// # Errors
    /// The server refused, failed, or was not reached.
    pub(crate) async fn call(
        &self,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let target = RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, action),
        };
        self.ask(target, arguments).await
    }

    /// Invoke `target` at the server with `arguments`: what it answered, or why not.
    ///
    /// # Errors
    /// The server refused, failed, or was not reached.
    pub(crate) async fn ask(
        &self,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let request = InvokeRequest {
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
        };
        match Arc::clone(&self.server)
            .invoke_until_settled(request, CALLED)
            .await
        {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or_default())
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
            Some(Ok(_)) => Err("the server did not finish the call".to_owned()),
            Some(Err(error)) => Err(error.to_string()),
            None => Err(tilo_api::daemon::enter::UNREACHED.to_owned()),
        }
    }

    /// The person's workspace list: at once with no `after`, else once it is another than the
    /// version `after` names, or the server's wait for it went by (the same list again).
    ///
    /// # Errors
    /// As [`Self::call`], and when the answer is no list.
    pub(crate) async fn list(&self, after: Option<&str>) -> Result<Listed, String> {
        let mut arguments = Map::new();
        if let Some(after) = after {
            arguments.insert(PERSONAL_AFTER.to_owned(), Value::String(after.to_owned()));
        }
        let answer = self.call(LIST_PERSONAL, arguments).await?;
        let version = PersonalList::version(&answer);
        let list = serde_json::from_value(answer)
            .map_err(|error| format!("the server's workspace list does not read: {error}"))?;
        Ok(Listed { list, version })
    }
}

impl tilo_api::session::AccountServer for Account {
    fn call(
        &self,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> tilo_api::session::Asked<'_, Value> {
        Box::pin(self.ask(target, arguments))
    }
}

impl Account {
    /// Where `workspace` is entered: the signed address of the device that keeps it, as the
    /// server names it to a device of the person's that does not keep it yet.
    ///
    /// # Errors
    /// As [`Self::call`], and when the server names no address for it.
    pub(crate) async fn address(&self, workspace: WorkspaceId) -> Result<TransportHint, String> {
        let mut arguments = Map::new();
        arguments.insert("workspace".to_owned(), Value::String(workspace.to_string()));
        let answer = self.call(LIST_PERSONAL, arguments).await?;
        let list: PersonalList = serde_json::from_value(answer)
            .map_err(|error| format!("the server's workspace list does not read: {error}"))?;
        list.workspaces
            .into_iter()
            .find(|entry| entry.workspace == workspace.to_string())
            .ok_or("the server's list does not name it")?
            .entered_at()
    }
}
