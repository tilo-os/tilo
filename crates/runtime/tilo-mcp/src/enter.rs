//! One way into one of the person's workspaces, for a key of theirs: the
//! person's own device, an agent's grant, a script's key. Their workspace
//! list names each workspace's device with the device's own signed address
//! (`tilo_server_session::workspaces::KeepingDeviceAddress`), and the key
//! enters it there, over iroh, wherever it is hosted:
//!
//! - **A device of the person's** (their desktop, a paired box) hosts it: the
//!   key dials that device's MCP server ([`crate::devices`]), which admits
//!   it as the person's server does (`tilo_workspace_service::workspace_access`): a
//!   certificate the person's root signed, unrevoked, current, its scope
//!   reaching the workspace, and the person's "allow this agent?" where the
//!   device asks it. The device serves the agent's MCP itself, with its own
//!   presence and history, so this side carries the lines.
//! - **A dedicated server** hosts it: the key opens a session with the server as
//!   one of the person's devices and enters the workspace as every device does
//!   ([`tilo_session_client::device::Device::enter_workspace`]): the server
//!   invites it in on that session and it redeems the invitation there. The
//!   agent surface ([`crate::McpServer`]) runs here over the server's actions.
//!
//! Either way the caller gets an [`Entered`], which answers MCP JSON-RPC.

use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use serde_json::Value;
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_server_session::workspaces::KeepingDeviceAddress;
use tilo_session_client::device::Device;
use tilo_session_client::remote::SessionActions;
use tilo_user_identity::DeviceCert;

use crate::{McpServer, Surface};
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
    /// A device of the person's serves it; its MCP server carries the lines.
    Device(Box<DeviceLines>),
    /// A dedicated server's actions, served by the agent surface here.
    Server {
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
            Self::Device(lines) => lines.call(request).await,
            Self::Server { server, .. } => server.handle(request).await,
        }
    }

    /// Whether the workspace's device serves this entry still.
    #[must_use]
    pub fn is_live(&self) -> bool {
        match self {
            Self::Device(lines) => lines.is_live(),
            Self::Server { device, .. } => device.is_live(),
        }
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        if let Self::Server { device, .. } = self {
            device.close();
        }
    }
}

/// Where `address` says a workspace is kept, checked against the device's
/// certificate it carries.
///
/// # Errors
/// It is no signed workspace address.
pub fn keeping_device_of(address: &KeepingDeviceAddress) -> Result<WorkspaceDeviceHintV1, String> {
    address.address()
}

/// Enter `workspace` at `address`, its device as the person's list names it,
/// as `key`, dialing from `endpoint` (whose secret key is `key`'s). `name` is
/// what the run is called where it acts.
///
/// # Errors
/// The address is not the workspace's, the device does not answer, or it does
/// not admit this key, with its reason.
pub async fn enter(
    endpoint: &Endpoint,
    key: &Key,
    workspace: &str,
    address: &KeepingDeviceAddress,
) -> Result<Entered, String> {
    let device = keeping_device_of(address)?;
    if device.workspace_id.to_string() != workspace {
        return Err("that address is another workspace's".to_owned());
    }
    if *endpoint.id().as_bytes() != key.cert.device_public_key {
        return Err("the endpoint dials as another key than the one entering".to_owned());
    }
    let dialed = crate::devices::open(
        endpoint,
        device.endpoint_addr.clone(),
        &key.cert,
        Some(workspace),
        false,
    )
    .await
    .map_err(|why| match why {
        crate::devices::NotOpened::Refused(reason) => {
            format!("{} did not let this key in: {reason}", address.name)
        }
        crate::devices::NotOpened::Unreachable(reason) => {
            tracing::debug!(%reason, name = %address.name, "a key did not enter its workspace");
            "The device that keeps this workspace is offline. Ask the person to open Tilo on \
             it, then try again."
                .to_owned()
        }
    })?;
    Ok(Entered::Device(Box::new(DeviceLines::new(dialed))))
}

impl Entered {
    /// `device`'s entry into `workspace` (`workspace_id` on its session), which a dedicated
    /// server serves: the agent surface runs here over the server's actions, in a session of its
    /// own.
    #[must_use]
    pub fn on_host(device: Device, workspace_id: WorkspaceId, workspace: &str) -> Self {
        let cert = device.cert();
        let (user, key) = (cert.user_public_key, cert.device_public_key);
        let person = hex::encode(user);
        let actions = device.actions(workspace_id);
        // The apps the device lists for this workspace: what a task's programs compile against
        // there.
        let catalogue = crate::task_hosts::WorkspaceCatalogue::new(
            actions.clone(),
            tilo_actions::remote::Caller::authenticated(user, key),
        );
        let server = McpServer::new(actions, workspace.to_owned(), person.clone())
            .with_surface(Surface::Agent)
            .with_agent_session(uuid::Uuid::new_v4().simple().to_string())
            .with_task_hosts(Some(std::sync::Arc::new(
                crate::task_hosts::TaskHosts::new(Some(&person), std::sync::Arc::new(catalogue)),
            )));
        Self::Server {
            server: Box::new(server),
            device: Box::new(device),
            workspace_id,
        }
    }
}

/// The MCP lines of an MCP server one device opened for this key: each
/// request a line out, its answer the line back with its id; what the device
/// says in between (a notification) is not an answer.
pub struct DeviceLines {
    lines: tokio::sync::Mutex<crate::devices::Lines>,
    live: std::sync::atomic::AtomicBool,
}

impl DeviceLines {
    fn new(dialed: crate::devices::Dialed) -> Self {
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
            tracing::debug!(%error, "the workspace's computer closed the agent's connection");
            return id.map(|id| failed(&id, CLOSED));
        }
        let id = id?;
        loop {
            match lines.next().await {
                Ok(answer) if answer.get("id") == Some(&id) => return Some(answer),
                Ok(_) => {}
                Err(error) => {
                    self.live.store(false, std::sync::atomic::Ordering::Relaxed);
                    tracing::debug!(%error, "the workspace's computer closed the agent's connection");
                    return Some(failed(&id, CLOSED));
                }
            }
        }
    }
}

/// What a connection the workspace's computer ended says.
const CLOSED: &str = "The device that keeps this workspace closed the connection. Ask the \
     person to open Tilo on it, then try again.";

fn failed(id: &Value, message: &str) -> Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
}
