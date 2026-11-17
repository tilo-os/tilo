//! A workspace's governance actions from a device that is not a desktop: a
//! developer's machine pushing a release of an app or a bot into a running
//! workspace (`tilo-app push`), a script, an agent.
//!
//! The device is one of a member's devices, enrolled as any other. It holds
//! a session with the workspace's host, subscribes to the workspace, and
//! invokes the action on the workspace's governance resource, addressed to the
//! host's own device, exactly as a desktop does. The workspace decides who may.

use serde_json::{Map, Value};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_server_protocol::WorkspaceId;

use crate::ServerClient;
use crate::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};

/// A workspace as a device reaches it: its host's server, the shard that
/// carries it there, and its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceLink {
    /// The host's server endpoint.
    pub server: iroh_base::EndpointId,
    /// Where the server listens, when it is reached directly (a server on
    /// this machine or network); otherwise found through discovery.
    pub direct: Option<std::net::SocketAddr>,
    /// The server shard that carries the workspace: its transport topic.
    pub workspace_id: WorkspaceId,
    /// The workspace's id.
    pub workspace: String,
}

impl WorkspaceLink {
    /// The server's address.
    #[must_use]
    pub fn server_addr(&self) -> iroh_base::EndpointAddr {
        let addr = iroh_base::EndpointAddr::new(self.server);
        match self.direct {
            Some(direct) => addr.with_ip_addr(direct),
            None => addr,
        }
    }
}

impl std::fmt::Display for WorkspaceLink {
    /// `tilo-workspace:<server endpoint>[@<ip:port>]/<workspace id>/<workspace uuid>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tilo-workspace:{}", self.server)?;
        if let Some(direct) = self.direct {
            write!(f, "@{direct}")?;
        }
        write!(f, "/{}/{}", self.workspace_id, self.workspace)
    }
}

impl std::str::FromStr for WorkspaceLink {
    type Err = String;

    fn from_str(link: &str) -> Result<Self, String> {
        let shape = || {
            format!(
                "{link}: a workspace link is tilo-workspace:<server>[@<ip:port>]/<workspace id>/<workspace uuid>"
            )
        };
        let rest = link.strip_prefix("tilo-workspace:").ok_or_else(shape)?;
        let mut parts = rest.split('/');
        let (server, shard, workspace) =
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(server), Some(shard), Some(workspace), None) if !shard.is_empty() => {
                    (server, shard, workspace)
                }
                _ => return Err(shape()),
            };
        let (server, direct) = match server.split_once('@') {
            Some((server, direct)) => (
                server,
                Some(
                    direct
                        .parse()
                        .map_err(|error| format!("{direct}: {error}"))?,
                ),
            ),
            None => (server, None),
        };
        Ok(Self {
            server: server
                .parse()
                .map_err(|error| format!("{server}: not a server endpoint: {error}"))?,
            direct,
            workspace_id: shard
                .parse()
                .map_err(|_| format!("{shard}: not a workspace id"))?,
            workspace: workspace.to_owned(),
        })
    }
}

/// A session with `link`'s host as the device `cert` signs for with `key`:
/// directly when the link names where the server listens, otherwise found
/// through discovery and relays.
///
/// # Errors
/// The endpoint could not bind, or the host could not be reached or did
/// not complete the handshake.
#[cfg(not(target_arch = "wasm32"))]
pub async fn connect(
    link: &WorkspaceLink,
    cert: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
) -> Result<ServerClient, String> {
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
    ServerClient::connect_to_pinned_host(
        endpoint,
        link.server_addr(),
        cert,
        key,
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .map_err(|error| format!("reach {link}: {error}"))
}

/// Invoke the governance action `action` of `workspace`, carried by
/// `workspace_id`, with `argument`, over `client`'s session with the workspace's
/// host: what the action produced.
///
/// # Errors
/// The workspace did not admit the device, the session closed, or the workspace
/// refused the action (one the device's person may not take is unknown to
/// them).
pub async fn invoke(
    client: &ServerClient,
    workspace_id: WorkspaceId,
    workspace: &str,
    action: &str,
    argument: Map<String, Value>,
) -> Result<Value, String> {
    subscribe(client, workspace_id).await?;
    call(client, workspace_id, workspace, action, argument).await
}

/// The governance action that takes a piece of a package file.
pub const UPLOAD_APP_FILE: &str = "upload-app-file";

/// The bytes of a package file one `upload-app-file` piece carries.
const UPLOAD_PIECE: usize = 256 * 1024;

/// Hand `file`, a `.tapp`, to `workspace`'s host in pieces, for its members to
/// fetch from the workspace: the file's BLAKE3 hash, hex, which `install-app`
/// then names as the version's `file`.
///
/// # Errors
/// As [`invoke`], for any piece.
pub async fn upload(
    client: &ServerClient,
    workspace_id: WorkspaceId,
    workspace: &str,
    file: &[u8],
) -> Result<String, String> {
    let hash = blake3::hash(file).to_hex().to_string();
    subscribe(client, workspace_id).await?;
    let total = file.len() as u64;
    let mut offset = 0_usize;
    for piece in file.chunks(UPLOAD_PIECE) {
        let mut argument = Map::new();
        argument.insert("file".into(), Value::from(hash.clone()));
        argument.insert("offset".into(), Value::from(offset as u64));
        argument.insert("total".into(), Value::from(total));
        argument.insert("data".into(), Value::from(hex::encode(piece)));
        call(client, workspace_id, workspace, UPLOAD_APP_FILE, argument).await?;
        offset = offset.saturating_add(piece.len());
    }
    Ok(hash)
}

/// Subscribe `client` to the workspace carried by `workspace_id`.
async fn subscribe(client: &ServerClient, workspace_id: WorkspaceId) -> Result<(), String> {
    client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| "the session closed".to_owned())?;
    loop {
        match client.recv().await {
            Some(Envelope::Ready { .. }) => return Ok(()),
            Some(Envelope::NotAuthorised { refusal, .. }) => {
                return Err(format!(
                    "the workspace does not admit this device: {refusal}"
                ));
            }
            None => return Err("the session closed".into()),
            Some(_) => {}
        }
    }
}

/// Invoke `action` on a session [`subscribe`] opened.
async fn call(
    client: &ServerClient,
    workspace_id: WorkspaceId,
    workspace: &str,
    action: &str,
    argument: Map<String, Value>,
) -> Result<Value, String> {
    let request = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
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
            to_device_public_key: client.server_peer.device_public_key,
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
                    other => Err(format!(
                        "the workspace did not finish `{action}`: {other:?}"
                    )),
                };
            }
            Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => {
                return Err(refusal.to_string());
            }
            None => return Err("the session closed".into()),
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceLink;
    use tilo_server_protocol::WorkspaceId;

    #[test]
    fn a_link_names_its_server_workspace_and_workspace_id() {
        let server = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let link = WorkspaceLink {
            server,
            direct: Some("127.0.0.1:4433".parse().unwrap()),
            workspace_id: WorkspaceId::from_u128(42),
            workspace: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        };
        let text = link.to_string();
        assert!(text.starts_with("tilo-workspace:"), "{text}");
        assert_eq!(text.parse::<WorkspaceLink>().unwrap(), link);
        let found = WorkspaceLink {
            direct: None,
            ..link
        };
        assert_eq!(found.to_string().parse::<WorkspaceLink>().unwrap(), found);
        for wrong in [
            "https://tilo.sh/lounge".to_owned(),
            "tilo-workspace:nope/1/workspace".to_owned(),
            format!("tilo-workspace:{server}/not-a-number/workspace"),
            format!("tilo-workspace:{server}/42"),
        ] {
            assert!(wrong.parse::<WorkspaceLink>().is_err(), "{wrong}");
        }
    }
}
