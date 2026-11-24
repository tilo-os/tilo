//! The person's account at the server their device enters through, asked as this device: the
//! server's own operations (`account/…`), their workspaces' list record read through the record
//! API, and where each workspace this device does not keep yet is entered, over the device action
//! service (`tilo-actions/1`) from the device's own endpoint. Each request is signed with the
//! device's key and decided for the certificate the roster of the workspace it is asked in holds
//! for the device; no session carries it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget, ResourceReadRequest,
};
use tilo_peer_state::actions::RemoteDevice;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    IDENTITY_LIST, MEMBERSHIP_APP, OWNER_ROLE, OWNERSHIP, PersonWorkspaces, WorkspaceLabels,
    member_record,
};

use super::workspaces::ServiceWorkspaces;

/// How long one call is followed before it is given up on: past the longest a read of the list
/// that follows it waits on the server.
const CALLED: Duration = Duration::from_secs(60);
/// How many reads of the person's list a device makes while the record is still arriving on the
/// server, each held until it moves.
const LIST_READS: u32 = 4;
/// How long one held read of the person's list waits for the record to arrive, ms.
const LIST_WAIT_MS: u32 = 10_000;

/// The person's account at their server, as this device asks it.
pub(crate) struct Account {
    server: Arc<RemoteDevice>,
}

impl Account {
    /// The account at `server`, asked in `workspace` through the action service of `own`, this
    /// device's workspace service: the server answers as the device its endpoint is keyed by.
    pub(crate) fn at(
        own: &ServiceWorkspaces,
        server: &iroh::EndpointAddr,
        workspace: &str,
    ) -> Self {
        let actions = own.copies().action_service();
        Self {
            server: Arc::new(actions.remote(&own.endpoint(), server, workspace)),
        }
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

    /// Read `read` through the server's record API: the readings as JSON
    /// (`{"revision", "records"}`).
    ///
    /// # Errors
    /// The server refused, failed, or was not reached.
    async fn read_records(&self, read: ResourceReadRequest) -> Result<Value, String> {
        let message = RemoteActionMessage::ResourceRead {
            correlation: 0,
            on_behalf_of: None,
            read,
        };
        match Arc::clone(&self.server)
            .ask_message(message)
            .await
            .flatten()
        {
            Some(RemoteActionMessage::ResourceReading { result, .. }) => {
                let text = result.map_err(|error| error.to_string())?;
                serde_json::from_str(&text).map_err(|error| error.to_string())
            }
            Some(_) => Err("the server answered no reading".to_owned()),
            None => Err(tilo_api::daemon::enter::UNREACHED.to_owned()),
        }
    }

    /// The person's workspaces as their own workspace's list record names them on their server,
    /// read through the record API and joined with each workspace's membership for its role and
    /// when it was made, and its owner's labels where they own it. With `labels`, only the
    /// workspaces labelled with every one of them, and with `workspace` only that one.
    ///
    /// # Errors
    /// The server did not answer or refused, or the list did not arrive on it in time.
    pub(crate) async fn listed(
        &self,
        person: &[u8; 32],
        labels: &WorkspaceLabels,
        workspace: Option<&str>,
    ) -> Result<PersonWorkspaces, String> {
        let own = person_workspace_id(person);
        let mut after: Option<String> = None;
        let mut found = None;
        for _ in 0..LIST_READS {
            let read = ResourceReadRequest {
                workspace: own.clone(),
                app: IDENTITY_LIST.0.to_owned(),
                record: IDENTITY_LIST.1.to_owned(),
                joined: false,
                after: after.clone(),
                wait_ms: after.as_ref().map(|_| LIST_WAIT_MS),
                answer: tilo_actions::remote::ReadAnswer::Document,
            };
            let readings = self.read_records(read).await?;
            if let Some(list) = PersonWorkspaces::of_readings(&readings)? {
                found = Some(list);
                break;
            }
            after = PersonWorkspaces::revision_of(&readings);
        }
        let Some(mut list) = found else {
            return Err("your list of workspaces has not reached the server yet".to_owned());
        };
        // Each workspace's host is a device of theirs: the answer names it as their devices do.
        let devices = self
            .read_records(ResourceReadRequest {
                workspace: own.clone(),
                app: tilo_server_protocol::devices::DEVICES.0.to_owned(),
                record: tilo_server_protocol::devices::DEVICES.1.to_owned(),
                joined: false,
                after: None,
                wait_ms: None,
                answer: tilo_actions::remote::ReadAnswer::Reading,
            })
            .await;
        let names = devices.as_ref().map_or_else(
            |_| std::collections::BTreeMap::new(),
            |value| tilo_server_protocol::devices::names(value),
        );
        list.with_hosts(&names, Some(&self.server.device()));
        let member = member_record(person);
        for index in 0..list.workspaces.len() {
            let named = list.workspaces[index].workspace.clone();
            let record = |app: &str, record: &str| ResourceReadRequest {
                workspace: named.clone(),
                app: app.to_owned(),
                record: record.to_owned(),
                joined: false,
                after: None,
                wait_ms: None,
                answer: tilo_actions::remote::ReadAnswer::Reading,
            };
            if let Ok(reading) = self.read_records(record(MEMBERSHIP_APP, &member)).await {
                list.with_membership(&named, &reading);
            }
            if list.workspaces[index].role.as_deref() == Some(OWNER_ROLE)
                && let Ok(reading) = self.read_records(record(OWNERSHIP.0, OWNERSHIP.1)).await
            {
                list.with_ownership(&named, &reading);
            }
        }
        list.workspaces.retain(|entry| {
            tilo_server_protocol::workspaces::labelled(&entry.labels, labels)
                && workspace.is_none_or(|only| entry.workspace == only)
        });
        Ok(list)
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

    fn listed(
        &self,
        person: &[u8; 32],
        labels: &WorkspaceLabels,
        workspace: Option<&str>,
    ) -> tilo_api::session::Asked<'_, PersonWorkspaces> {
        let (person, labels, workspace) = (*person, labels.clone(), workspace.map(str::to_owned));
        Box::pin(async move { self.listed(&person, &labels, workspace.as_deref()).await })
    }
}
