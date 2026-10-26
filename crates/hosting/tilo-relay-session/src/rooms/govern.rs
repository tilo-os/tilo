//! A room's governance actions from a device that is not a desktop: a
//! developer's machine pushing a release of an app or a bot into a running
//! room (`tilo-app push`), a script, an agent.
//!
//! The device is one of a member's devices, enrolled as any other. It holds
//! a session with the room's host, subscribes to the room's workspace, and
//! invokes the action on the room's governance resource, addressed to the
//! host's own device, exactly as a desktop does. The room decides who may.

use serde_json::{Map, Value};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};

use crate::RelayClient;
use crate::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};

/// A room as a device reaches it: its host's relay, the workspace that
/// carries it there, and its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomLink {
    /// The host's relay endpoint.
    pub relay: iroh_base::EndpointId,
    /// Where the relay listens, when it is reached directly (a relay on
    /// this machine or network); otherwise found through discovery.
    pub direct: Option<std::net::SocketAddr>,
    /// The workspace that carries the room on its relay.
    pub workspace_id: u64,
    /// The room's id.
    pub room: String,
}

impl RoomLink {
    /// The relay's address.
    #[must_use]
    pub fn relay_addr(&self) -> iroh_base::EndpointAddr {
        let addr = iroh_base::EndpointAddr::new(self.relay);
        match self.direct {
            Some(direct) => addr.with_ip_addr(direct),
            None => addr,
        }
    }
}

impl std::fmt::Display for RoomLink {
    /// `tilo-room:<relay endpoint>[@<ip:port>]/<workspace>/<room>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tilo-room:{}", self.relay)?;
        if let Some(direct) = self.direct {
            write!(f, "@{direct}")?;
        }
        write!(f, "/{}/{}", self.workspace_id, self.room)
    }
}

impl std::str::FromStr for RoomLink {
    type Err = String;

    fn from_str(link: &str) -> Result<Self, String> {
        let shape =
            || format!("{link}: a room link is tilo-room:<relay>[@<ip:port>]/<workspace>/<room>");
        let rest = link.strip_prefix("tilo-room:").ok_or_else(shape)?;
        let mut parts = rest.split('/');
        let (relay, workspace, room) =
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(relay), Some(workspace), Some(room), None) if !room.is_empty() => {
                    (relay, workspace, room)
                }
                _ => return Err(shape()),
            };
        let (relay, direct) = match relay.split_once('@') {
            Some((relay, direct)) => (
                relay,
                Some(
                    direct
                        .parse()
                        .map_err(|error| format!("{direct}: {error}"))?,
                ),
            ),
            None => (relay, None),
        };
        Ok(Self {
            relay: relay
                .parse()
                .map_err(|error| format!("{relay}: not a relay endpoint: {error}"))?,
            direct,
            workspace_id: workspace
                .parse()
                .map_err(|_| format!("{workspace}: not a workspace id"))?,
            room: room.to_owned(),
        })
    }
}

/// A session with `link`'s host as the device `cert` signs for with `key`:
/// directly when the link names where the relay listens, otherwise found
/// through discovery and relays.
///
/// # Errors
/// The endpoint could not bind, or the host could not be reached or did
/// not complete the handshake.
pub async fn connect(
    link: &RoomLink,
    cert: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
) -> Result<RelayClient, String> {
    let secret = iroh::SecretKey::from_bytes(&key.to_bytes());
    let endpoint = if link.direct.is_some() {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(secret)
            .bind()
            .await
    } else {
        iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            .bind()
            .await
    }
    .map_err(|error| format!("bind an endpoint: {error}"))?;
    RelayClient::connect_to_pinned_host(
        endpoint,
        link.relay_addr(),
        cert,
        key,
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .map_err(|error| format!("reach {link}: {error}"))
}

/// Invoke the governance action `action` of `room`, carried by
/// `workspace_id`, with `argument`, over `client`'s session with the room's
/// host: what the action produced.
///
/// # Errors
/// The room did not admit the device, the session closed, or the room
/// refused the action (one the device's person may not take is unknown to
/// them).
pub async fn invoke(
    client: &RelayClient,
    workspace_id: u64,
    room: &str,
    action: &str,
    argument: Map<String, Value>,
) -> Result<Value, String> {
    client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| "the session closed".to_owned())?;
    loop {
        match client.recv().await {
            Some(Envelope::Ready { .. }) => break,
            Some(Envelope::NotAuthorised { reason, .. }) => {
                return Err(format!("the room does not admit this device: {reason}"));
            }
            None => return Err("the session closed".into()),
            Some(_) => {}
        }
    }
    let request = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, action),
        argument: if argument.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(argument)
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    });
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: client.relay_peer.device_public_key,
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(request)
                .encode()
                .map_err(|error| error.to_string())?,
        })
        .map_err(|_| "the session closed".to_owned())?;
    loop {
        match client.recv().await {
            Some(Envelope::RemoteAction { frame, .. }) => {
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?
                    .message;
                let RemoteActionMessage::Receipt(receipt) = message else {
                    continue;
                };
                return match receipt.status {
                    ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
                    ReceiptStatus::Rejected { error } => Err(error.to_string()),
                    other => Err(format!("the room did not finish `{action}`: {other:?}")),
                };
            }
            Some(Envelope::RemoteActionUndeliverable { reason, .. }) => return Err(reason),
            None => return Err("the session closed".into()),
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RoomLink;

    #[test]
    fn a_room_link_names_its_relay_workspace_and_room() {
        let relay = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let link = RoomLink {
            relay,
            direct: Some("127.0.0.1:4433".parse().unwrap()),
            workspace_id: 42,
            room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        };
        let text = link.to_string();
        assert!(text.starts_with("tilo-room:"), "{text}");
        assert_eq!(text.parse::<RoomLink>().unwrap(), link);
        let found = RoomLink {
            direct: None,
            ..link
        };
        assert_eq!(found.to_string().parse::<RoomLink>().unwrap(), found);
        for wrong in [
            "https://tilo.coffee/lounge".to_owned(),
            "tilo-room:nope/1/room".to_owned(),
            format!("tilo-room:{relay}/not-a-number/room"),
            format!("tilo-room:{relay}/42"),
        ] {
            assert!(wrong.parse::<RoomLink>().is_err(), "{wrong}");
        }
    }
}
