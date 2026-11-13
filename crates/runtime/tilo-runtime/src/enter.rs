//! One way into one of the person's workspaces, for a key of theirs: the
//! person's own device, an agent's grant, a script's key. Their workspace
//! list names each workspace's host with the host's own signed address
//! (`tilo_relay_session::workspaces::WorkspaceHostAddress`), and the key
//! enters it there, over iroh, wherever it is hosted:
//!
//! - **A device of the person's** (their desktop, a paired box) hosts it: the
//!   key dials that device's agent door ([`crate::remote_door`]), which admits
//!   it as the person's relay does (`tilo_room_host::workspace_access`): a
//!   certificate the person's root signed, unrevoked, current, its scope
//!   reaching the workspace, and the person's "allow this agent?" where the
//!   device asks it. The device serves the agent's MCP itself, with its own
//!   presence and history, so this side carries the lines.
//! - **A dedicated host** (a relay) hosts it: the key opens a session with the
//!   host as one of the person's devices and enters the workspace as every
//!   device does ([`tilo_session_client::device::Device::enter_workspace`]): the
//!   host invites it in on that session and it redeems the invitation there.
//!   The agent surface ([`crate::mcp::McpServer`]) runs here over the host's
//!   actions.
//!
//! Either way the caller gets an [`Entered`], which answers MCP JSON-RPC.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use serde_json::Value;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_relay_session::workspaces::WorkspaceHostAddress;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_session_core::remote::SessionActions;
use tilo_user_identity::DeviceCert;

use crate::mcp::{McpServer, Surface};
use collab_types::WorkspaceId;

/// A key of the person's: its signing key and the certificate their root (or
/// a grant of theirs) signed for it.
#[derive(Clone)]
pub struct Key {
    pub key: SigningKey,
    pub cert: DeviceCert,
}

/// A workspace a key entered: where its MCP is answered.
pub enum Entered {
    /// A device of the person's serves it; its agent door carries the lines.
    Door(Box<DoorLines>),
    /// A dedicated host's actions, served by the agent surface here.
    Host {
        server: Box<McpServer<SessionActions>>,
        device: Box<Device>,
        workspace_id: WorkspaceId,
    },
}

impl Entered {
    /// The answer to the JSON-RPC message `request`; `None` for a
    /// notification.
    pub async fn handle(&self, request: Value) -> Option<Value> {
        match self {
            Self::Door(lines) => lines.call(request).await,
            Self::Host { server, .. } => server.handle(request).await,
        }
    }

    /// Whether the workspace's host serves this entry still.
    #[must_use]
    pub fn is_live(&self) -> bool {
        match self {
            Self::Door(lines) => lines.is_live(),
            Self::Host { device, .. } => device.is_live(),
        }
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        if let Self::Host { device, .. } = self {
            device.close();
        }
    }
}

/// Where `address` says a workspace is hosted, checked against the host's
/// certificate it carries.
///
/// # Errors
/// It is no signed workspace address.
pub fn host_of(address: &WorkspaceHostAddress) -> Result<IrohWorkspaceHostHintV1, String> {
    address.address()
}

/// Enter `workspace` at `address`, its host as the person's list names it,
/// as `key`, dialing from `endpoint` (whose secret key is `key`'s). `name` is
/// what the run is called where it acts.
///
/// # Errors
/// The address is not the workspace's, the host does not answer, or it does
/// not admit this key, with its reason.
pub async fn enter(
    endpoint: &Endpoint,
    key: &Key,
    workspace: &str,
    address: &WorkspaceHostAddress,
    name: &str,
) -> Result<Entered, String> {
    let host = host_of(address)?;
    if host.workspace_id.to_string() != workspace {
        return Err("that address is another workspace's".to_owned());
    }
    if *endpoint.id().as_bytes() != key.cert.device_public_key {
        return Err("the endpoint dials as another key than the one entering".to_owned());
    }
    match host.mode {
        WorkspaceHostMode::NativeInteractiveWorkspaceOwner => {
            let dialed = crate::remote_door::open(
                endpoint,
                host.endpoint_addr.clone(),
                &key.cert,
                Some(workspace),
                false,
            )
            .await
            .map_err(|reason| format!("{} did not let this key in: {reason}", address.name))?;
            Ok(Entered::Door(Box::new(DoorLines::new(dialed))))
        }
        WorkspaceHostMode::CertifiedHost => {
            let device = Device::over(
                PeerConfig {
                    host: tilo_relay_client::Host::at(host.endpoint_addr.clone()),
                    cert: key.cert.clone(),
                    host_cert: host.host_device_certificate.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                    entering: None,
                },
                Arc::new(key.key.clone()),
                &key.cert.device_label,
            );
            let entered = match device.connected().await {
                Ok(()) => {
                    device
                        .enter_workspace(workspace, &key.cert.device_label)
                        .await
                }
                Err(refusal) => Err(refusal),
            };
            match entered {
                Ok(workspace_id) => Ok(Entered::on_host(device, workspace_id, workspace, name)),
                Err(refusal) => {
                    device.close();
                    Err(format!(
                        "{} did not let this key in: {}",
                        address.name, refusal.message
                    ))
                }
            }
        }
    }
}

impl Entered {
    /// `device`'s entry into `workspace` (`workspace_id` on its session), which a dedicated host
    /// serves: the agent surface runs here over the host's actions, as the run called `name`.
    #[must_use]
    pub fn on_host(device: Device, workspace_id: WorkspaceId, workspace: &str, name: &str) -> Self {
        let person = collab_types::Principal::of_person(device.cert().user_public_key)
            .id()
            .to_owned();
        let server = McpServer::new(device.actions(workspace_id), workspace.to_owned(), person)
            .with_surface(Surface::Agent)
            .with_agent_run(uuid::Uuid::new_v4().simple().to_string(), name.to_owned());
        Self::Host {
            server: Box::new(server),
            device: Box::new(device),
            workspace_id,
        }
    }
}

/// The MCP lines of an agent door one device opened for this key: each
/// request a line out, its answer the line back with its id; what the door
/// says in between (a notification) is not an answer.
pub struct DoorLines {
    lines: tokio::sync::Mutex<crate::remote_door::Lines>,
    live: std::sync::atomic::AtomicBool,
}

impl DoorLines {
    fn new(dialed: crate::remote_door::Dialed) -> Self {
        Self {
            lines: tokio::sync::Mutex::new(dialed.into_lines()),
            live: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn call(&self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let mut lines = self.lines.lock().await;
        if let Err(error) = lines.send(&request).await {
            self.live.store(false, std::sync::atomic::Ordering::Relaxed);
            return id.map(|id| failed(&id, &format!("the device's door closed: {error}")));
        }
        let id = id?;
        loop {
            match lines.next().await {
                Ok(answer) if answer.get("id") == Some(&id) => return Some(answer),
                Ok(_) => {}
                Err(error) => {
                    self.live.store(false, std::sync::atomic::Ordering::Relaxed);
                    return Some(failed(&id, &format!("the device's door closed: {error}")));
                }
            }
        }
    }
}

fn failed(id: &Value, message: &str) -> Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
}
