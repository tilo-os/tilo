//! A device placing its own workspaces on the room host it runs: the host
//! creates the room and issues the device the invitation its replica
//! redeems, through the host's own `tilo.host` operations, asked in process
//! exactly as a relay is asked over a session. The desktop session and the
//! standalone runtime place their workspaces through this one path.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_relay_session::rooms::{
    CREATE_ROOM, CreatedRoom, ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
};
use tilo_user_identity::DeviceCert;

use crate::host::{RoomHost, local_caller};

/// How long the invitation a device redeems into a room placed on its own
/// host stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;

impl RoomHost {
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
        let invoke = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            // The provider is named: no catalogue lists it.
            target: RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: ActionAddress::new(HOST_ROOMS_RESOURCE.to_owned(), action),
            },
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
            .ok_or("this host does not answer its own operations")?;
        match pending.await {
            Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
                ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
                ReceiptStatus::Rejected { error } => Err(format!("the host refused: {error}")),
                ReceiptStatus::Failed { error } => Err(error.message),
                ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
                    Err("the host did not finish the request".into())
                }
            },
            Some(_) => Err("the host answered with something else".into()),
            None => Err("the host dropped the request".into()),
        }
    }

    /// Place `room`, a workspace called `name`, on this host for `caller`, a
    /// device of the host's own person: the host creates the room unless it
    /// holds it already (a placement cut short after the room was made goes
    /// on from there, the room it is, never another), and answers the
    /// invitation, as text, that `caller`'s replica redeems to hold it.
    ///
    /// # Errors
    /// The host refused to create or enter the room, or made another room.
    pub async fn place_own_room(
        &self,
        room: &str,
        name: &str,
        caller: &DeviceCert,
    ) -> Result<String, String> {
        if self.rooms().summary(room).await.is_err() {
            let mut arguments = Map::new();
            arguments.insert("name".into(), name.into());
            arguments.insert("room".into(), room.into());
            let created: CreatedRoom = answer(self.ask_own(caller, CREATE_ROOM, arguments).await?)?;
            if created.room.room != room {
                return Err("this device's host made another room than the workspace's".into());
            }
        }
        let mut arguments = Map::new();
        arguments.insert("room".into(), room.into());
        arguments.insert("expires_in_seconds".into(), OWN_INVITATION_SECONDS.into());
        let entered: EnteredRoom = answer(self.ask_own(caller, ENTER_ROOM, arguments).await?)?;
        Ok(tilo_peer_state::join::encode_invitation_text(
            &entered.invitation.decode()?.signed_invitation,
        ))
    }
}

fn answer<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("the host answered unexpectedly: {error}"))
}
