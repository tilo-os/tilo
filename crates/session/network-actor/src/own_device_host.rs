//! Asking the room host of another of the person's devices for one of its
//! own operations (`tilo.host`): entering a room that device hosts, which
//! the person's workspace list names and this device does not hold yet.
//!
//! A device hosting rooms answers its person's other devices on a
//! person-scoped session (`RelayClient::connect_v2_to_own_device`), neither
//! side needing host capability. The session is opened for the one
//! request, on the person's continuity workspace, and closed with its
//! answer: nothing is kept dialling the device, and no timer runs for it.
use iroh::EndpointAddr;
use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
use tilo_relay_session::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::RevocationList;

use super::{ActorState, room_governance};

/// One host operation for another of the person's devices.
#[derive(Debug)]
pub struct Request {
    /// Where the device is reached.
    pub addr: EndpointAddr,
    /// The device, as the person's directory names it: the session pins it.
    pub device: [u8; 32],
    /// The invocation, on the workspace its request names, and where the
    /// receipt goes.
    pub governance: room_governance::Request,
}

impl ActorState {
    /// Ask `request` of the device it names, on a session opened for it.
    pub(super) fn ask_own_device_host(&self, request: Request) {
        let Some(identity) = self.host_identity.clone() else {
            let _ = request
                .governance
                .reply
                .send(Err("this device has no identity to ask with".into()));
            return;
        };
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            let Request {
                addr,
                device,
                governance: request,
            } = request;
            let room_governance::Request {
                ws, invoke, reply, ..
            } = request;
            let invocation = invoke.invocation;
            let answer = async {
                let (cert, entries) = identity.material_for(ws);
                let mut revocations = RevocationList::new();
                for entry in entries {
                    revocations
                        .insert(entry)
                        .map_err(|error| error.to_string())?;
                }
                let own = cert.device_public_key;
                let client = RelayClient::connect_v2_to_own_device(
                    endpoint,
                    addr,
                    cert.user_public_key,
                    device,
                    cert,
                    identity.device_signing_key.clone(),
                    revocations,
                )
                .await
                .map_err(|error| error.to_string())?;
                let session =
                    RelayCoeditSession::join(client, ws, own).map_err(|error| error.to_string())?;
                let answer = ask(&session, device, invocation, invoke).await;
                session.close().await;
                answer
            };
            let _ = reply.send(answer.await);
        });
    }
}

/// Send `invoke` to `device` once `session` is subscribed, and answer its
/// receipt.
async fn ask(
    session: &RelayCoeditSession,
    device: [u8; 32],
    invocation: tilo_actions::InvocationId,
    invoke: tilo_actions::remote::InvokeRequest,
) -> Result<ReceiptStatus, String> {
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(invoke))
        .encode()
        .map_err(|_| "the request could not be encoded".to_string())?;
    let mut sent = false;
    loop {
        match session.recv().await {
            Some(Inbound::Ready { .. }) if !sent => {
                session
                    .publish_remote_action(device, frame.clone())
                    .map_err(|error| error.to_string())?;
                sent = true;
            }
            Some(Inbound::RemoteAction { from, frame }) if sent && from == device => {
                let Ok(frame) = RemoteActionFrame::decode(&frame) else {
                    continue;
                };
                if let RemoteActionMessage::Receipt(receipt) = frame.message
                    && receipt.invocation == invocation
                {
                    return Ok(receipt.status);
                }
            }
            Some(_) => {}
            None => return Err("the device closed the session before answering".into()),
        }
    }
}
