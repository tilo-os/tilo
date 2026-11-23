//! An agent of this login working on another of the person's devices: a `--connect --device`
//! client is served MCP here, and every request it makes goes to that device over the one action
//! frame between devices, signed by the agent's own key this device made for it
//! ([`tilo_workspace_service::agents::agent_keys`]). The device that answers checks the signature
//! as that agent's, for the person, under the agent's grants there. The desktop session and the
//! device's daemon serve such clients alike.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use collab_types::signer::DeviceSigner;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::agents::agent_keys::{AgentKeys, AgentSigner};

use crate::i18n::{t, t_args};

/// How long one request of a forwarded client waits for the device's answer.
const REPLY_DEADLINE: Duration = Duration::from_secs(60);
/// How long a forwarded client's connection to the device may take to open.
const CONNECT_DEADLINE: Duration = Duration::from_secs(20);

/// What a host knows of where the person's other devices are.
pub trait Whereabouts: Send + Sync {
    /// The workspaces this device holds, whose rosters name the person's devices.
    fn workspaces(&self) -> Vec<String>;

    /// Where the device `id` is reached.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        EndpointAddr::new(id)
    }

    /// The certificates `workspace`'s roster keeps here: its members as this device holds them.
    fn devices(&self, workspace: &str) -> Vec<DeviceCert>;
}

/// The person's other devices, as this device asks them over the one action frame between
/// devices.
#[derive(Clone)]
pub struct OtherDevices(Arc<Inner>);

struct Inner {
    /// The endpoint this device asks from.
    endpoint: Endpoint,
    /// The runtime the connection to the other device opens on.
    runtime: tokio::runtime::Handle,
    /// The person this device is, and its own key.
    whose: ([u8; 32], [u8; 32]),
    /// This device's certificate and the key that signs as it: the connection the agent's
    /// requests travel on, which the other device admits as this device.
    cert: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    /// The person's revocations, as this device reads them.
    revocations: RevocationList,
    /// The keys this device makes for its person's agents.
    keys: Arc<dyn AgentKeys>,
    places: Arc<dyn Whereabouts>,
}

impl OtherDevices {
    /// Ask through `actions` from `endpoint` on `runtime`, as the device `this` of `person`,
    /// whose certificate `cert` and key `signer` are, reaching devices where `places` says.
    #[must_use]
    pub fn new(
        endpoint: Endpoint,
        runtime: tokio::runtime::Handle,
        (person, this): ([u8; 32], [u8; 32]),
        (cert, signer): (DeviceCert, Arc<dyn DeviceSigner>),
        revocations: RevocationList,
        keys: Arc<dyn AgentKeys>,
        places: Arc<dyn Whereabouts>,
    ) -> Self {
        Self(Arc::new(Inner {
            endpoint,
            runtime,
            whose: (person, this),
            cert,
            signer,
            revocations,
            keys,
            places,
        }))
    }

    /// Serve `stream`, a client of this login that asked to work on the device `forward` names
    /// as the person's agent: MCP here, every request it makes sent to that device as an ordinary
    /// action frame signed by the agent's own key this device made, and decided there. Refused,
    /// with why, where no device of the person's goes by that name. Blocks the calling thread,
    /// which must not be one of the runtime's.
    pub fn serve_forward(&self, mut stream: UnixStream, forward: &tilo_mcp::devices::Forward) {
        let (device, workspace) = match self.target(forward) {
            Ok(target) => target,
            Err(reason) => {
                tracing::info!(device = %forward.device, %reason, "MCP server: a client is not carried to another device");
                tilo_mcp::devices::refuse_forward(stream, reason);
                return;
            }
        };
        if let Err(error) = tilo_mcp::devices::open_forward(&mut stream) {
            tilo_failure::ignored!(
                "a client that left before it was carried needs no answer",
                tracing::debug!(%error, "MCP server: the client left before it was carried")
            );
            return;
        }
        let person = self.0.whose.0;
        let served = tilo_mcp::session_api::serve_as(stream, None, |hello, _| {
            let client = hello.client_name.clone();
            let agent = tilo_actions::remote::device_agent_id(&person, &client);
            let signer = AgentSigner::grant(
                Arc::clone(&self.0.keys),
                &workspace,
                (&agent, &client),
                &hex::encode(person),
            )
            .map_err(|reason| format!("this computer cannot make the agent a key: {reason}"))?;
            let (signer, _caller) = signer;
            let (agent_cert, agent_signer) = signer.signing()?;
            let session = self.0.runtime.block_on(self.open(
                device,
                &workspace,
                &client,
                agent_cert,
                agent_signer,
            ))?;
            let server = tilo_mcp::McpServer::for_agent(
                session.actions.clone(),
                workspace.clone(),
                &agent,
                &hex::encode(person),
            )
            .agent_named(&client);
            Ok(server)
        });
        if let Err(error) = served {
            tilo_failure::ignored!(
                "a client that disconnected needs no answer",
                tracing::debug!(%error, "MCP server: the carried client disconnected")
            );
        }
    }

    /// Open the connection to `device` and make it the run of the agent `client`, in
    /// `workspace`: the session is the agent's own device there, so every request it makes is
    /// signed by the agent's key this device made for it, and the device admits it as that
    /// agent's for the person, under the agent's grants there.
    async fn open(
        &self,
        device: [u8; 32],
        workspace: &str,
        client: &str,
        agent_cert: DeviceCert,
        agent_signer: Arc<dyn DeviceSigner>,
    ) -> Result<Arc<ForwardSession>, String> {
        let id =
            EndpointId::from_bytes(&device).map_err(|_| "that is not a device key".to_owned())?;
        let workspace_id =
            WorkspaceId::parse(workspace).ok_or_else(|| "that is not a workspace id".to_owned())?;
        let server_cert = self
            .others_in(workspace)
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
            .ok_or_else(|| "that device is not in the workspace here".to_owned())?;
        let config = tilo_session_client::PeerConfig {
            host: tilo_server_client::host::ServerAddress::at(self.0.places.addr(id)),
            cert: agent_cert,
            server_cert,
            revocations: self.0.revocations.clone(),
            entering: None,
        };
        let opened = tokio::time::timeout(CONNECT_DEADLINE, async {
            let session =
                tilo_session_client::device::Device::over(config, agent_signer, client);
            session
                .enter_workspace(workspace, client)
                .await
                .map_err(|refusal| refusal.to_string())?;
            tilo_session_client::device::session_as_agent(session.session(), client)
                .await
                .map_err(|error| format!("the device did not take the agent's run: {error}"))?;
            Ok::<_, String>(session)
        })
        .await
        .map_err(|_| "the device did not answer".to_owned())??;
        let actions = opened.actions(workspace_id);
        Ok(Arc::new(ForwardSession {
            _session: opened,
            actions,
        }))
    }

    /// The device `forward` names and the workspace the agent works in there: the one it names,
    /// else the one of this device's workspaces that device is in. A device is named by its name
    /// or its key.
    fn target(&self, forward: &tilo_mcp::devices::Forward) -> Result<([u8; 32], String), String> {
        let asked = forward.device.trim();
        let key = tilo_failure::ignored!(
            "a device named by its name is not a key",
            hex::decode(asked).ok()
        )
        .and_then(|key| <[u8; 32]>::try_from(key).ok());
        if let (Some(key), Some(workspace)) = (key, &forward.workspace) {
            return Ok((key, workspace.clone()));
        }
        let workspaces = match &forward.workspace {
            Some(workspace) => vec![workspace.clone()],
            None => self.0.places.workspaces(),
        };
        let mut found = Vec::new();
        let mut known = std::collections::BTreeSet::new();
        for workspace in workspaces {
            for device in self.others_in(&workspace) {
                let name = tilo_mcp::devices::device_name(&device);
                if key == Some(device.device_public_key) || name.eq_ignore_ascii_case(asked) {
                    found.push((device.device_public_key, workspace.clone()));
                }
                known.insert(name);
            }
        }
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] if known.is_empty() => Err(t("other-devices-none")),
            [] => Err(t_args(
                "other-devices-unknown",
                &[
                    ("device", asked.to_owned().into()),
                    (
                        "devices",
                        known.into_iter().collect::<Vec<_>>().join(", ").into(),
                    ),
                ],
            )),
            _ => Err(t_args(
                "other-devices-name-workspace",
                &[("device", asked.to_owned().into())],
            )),
        }
    }

    /// The person's other devices in `workspace`'s roster as this device holds it.
    fn others_in(&self, workspace: &str) -> Vec<DeviceCert> {
        let (person, this) = self.0.whose;
        self.0
            .places
            .devices(workspace)
            .into_iter()
            .filter(|device| device.user_public_key == person && device.device_public_key != this)
            .collect()
    }
}

/// One forwarded client's connection to the device that answers for it: the session keeps the
/// agent's run open, and its actions carry each request.
struct ForwardSession {
    /// Holding the session keeps its links and the agent's run open.
    _session: tilo_session_client::device::Device,
    /// The workspace's actions, asked of the device that answers for them.
    actions: tilo_session_actions::SessionActions,
}

impl std::fmt::Debug for ForwardSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardSession").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_mcp::devices::{Forward, agent_id, forward_request};

    fn certificate(label: &str, key: u8) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [key; 32],
            device_label: label.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_forward_names_its_agent_its_request_and_the_devices_that_answer;

        phase an_agent_is_named_by_the_device_it_came_from;
        {
            let laptop = certificate("Studio Laptop", 0x3f);
            assert_eq!(
                agent_id("claude-code", &laptop),
                "claude-code@studio-laptop-3f3f3f3f"
            );
            assert_eq!(
                agent_id("codex", &certificate("  ", 0xab)),
                "codex@abababab"
            );
            let id = agent_id(&"x".repeat(64), &certificate(&"Ü".repeat(200), 1));
            tilo_peer_state::workspace_membership::valid_agent_id(&id).unwrap();
        }

        phase only_a_forward_line_asks_for_a_forward;
        {
            assert_eq!(
                forward_request(br#"{"forward":{"device":"studio"}}"#),
                Some(Forward {
                    device: "studio".into(),
                    workspace: None
                })
            );
            assert_eq!(
                forward_request(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
                None
            );
            assert_eq!(forward_request(br#"{"op":"agent_place"}"#), None);
            assert_eq!(
                forward_request(br#"{"forward":{"device":"studio","as":"person"}}"#),
                None,
                "nothing beyond the device and the workspace is read"
            );
            assert!(!tilo_mcp::session_api::is_control_request(
                br#"{"forward":{"device":"studio"}}"#
            ));
        }
    }
}
