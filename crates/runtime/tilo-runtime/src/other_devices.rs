//! An agent of this login working on another of the person's devices: a `--connect --device`
//! client is served MCP here, and every request it makes goes to that device over the one action
//! frame between devices, naming the agent ([`HandedAgent`]). That device decides each request as
//! that agent's, for the person and never more, under the agent's grants there. The desktop
//! session and the device's daemon serve such clients alike.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use neighbour_wire::actions::HandedAgent;
use tilo_mcp::devices::{DeviceDirectory, Forward, Reach, device_name};
use tilo_peer_state::actions::{ActionService, RemoteDevice};
use tilo_user_identity::DeviceCert;

use crate::i18n::{t, t_args};

/// What a host knows of where the person's other devices are.
pub trait Whereabouts: Send + Sync {
    /// The workspaces this device holds, whose rosters name the person's devices.
    fn workspaces(&self) -> Vec<String>;

    /// Whether `device` of `workspace` is connected here now.
    fn reachable(&self, workspace: &str, device: [u8; 32]) -> bool;

    /// Where the device `id` is reached.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        EndpointAddr::new(id)
    }
}

/// The person's other devices, as this device asks them through its action service.
#[derive(Clone)]
pub struct OtherDevices(Arc<Inner>);

struct Inner {
    actions: ActionService,
    /// The endpoint the action service asks from, as this device.
    endpoint: Endpoint,
    /// The runtime the action service asks on.
    runtime: tokio::runtime::Handle,
    /// The person this device is, and its own key.
    whose: ([u8; 32], [u8; 32]),
    places: Arc<dyn Whereabouts>,
}

impl OtherDevices {
    /// Ask through `actions` from `endpoint`, on `runtime`, as the device `this` of `person`,
    /// reaching devices where `places` says they are.
    #[must_use]
    pub fn new(
        actions: ActionService,
        endpoint: Endpoint,
        runtime: tokio::runtime::Handle,
        (person, this): ([u8; 32], [u8; 32]),
        places: Arc<dyn Whereabouts>,
    ) -> Self {
        Self(Arc::new(Inner {
            actions,
            endpoint,
            runtime,
            whose: (person, this),
            places,
        }))
    }

    /// Serve `stream`, a client of this login that asked to work on the device `forward` names as
    /// the person's agent: MCP here, every request it makes sent to that device naming it, which
    /// decides each. Refused, with why, where no device of the person's goes by that name. Blocks
    /// the calling thread, which must not be one of the runtime's.
    pub fn serve_forward(&self, mut stream: UnixStream, forward: &Forward) {
        let (device, workspace) = match self.0.runtime.block_on(self.target(forward)) {
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
        let session = format!("device-agent-{}", uuid::Uuid::new_v4().simple());
        let person = hex::encode(self.0.whose.0);
        let directory = Arc::new(self.clone()) as Arc<dyn DeviceDirectory>;
        let served = tilo_mcp::session_api::serve_as(stream, None, |hello, _| {
            let agent = HandedAgent {
                client: hello.client_name.clone(),
                session,
            };
            let remote = self.remote_for_agent(device, &workspace, agent)?;
            let id = tilo_mcp::session_api::proposed_agent_id(&hello.client_name);
            Ok(
                tilo_mcp::McpServer::for_agent(remote, workspace.clone(), &id, &person)
                    .agent_named(&hello.client_name)
                    .with_devices(Some(directory)),
            )
        });
        if let Err(error) = served {
            tilo_failure::ignored!(
                "a client that disconnected needs no answer",
                tracing::debug!(%error, "MCP server: the carried client disconnected")
            );
        }
    }

    /// The device `key` asked as `agent`, which this device's person handed what it asks to, in
    /// `workspace`.
    fn remote_for_agent(
        &self,
        key: [u8; 32],
        workspace: &str,
        agent: HandedAgent,
    ) -> Result<RemoteDevice, String> {
        let id = EndpointId::from_bytes(&key).map_err(|_| "that is not a device key".to_owned())?;
        let _entered = self.0.runtime.enter();
        Ok(self.0.actions.remote_for_agent(
            &self.0.endpoint,
            &self.0.places.addr(id),
            workspace,
            agent,
        ))
    }

    /// The device `forward` names and the workspace the agent works in there: the one it names,
    /// else the one of this device's workspaces that device is in. A device is named by its name
    /// or its key.
    async fn target(&self, forward: &Forward) -> Result<([u8; 32], String), String> {
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
        let mut known = BTreeSet::new();
        for workspace in workspaces {
            for device in self.devices_in(&workspace, false).await {
                let name = device_name(&device);
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

    /// The person's devices in `workspace`'s roster as this device holds it, this one among them
    /// when `with_this`.
    async fn devices_in(&self, workspace: &str, with_this: bool) -> Vec<DeviceCert> {
        let (person, this) = self.0.whose;
        self.0
            .actions
            .certificates(workspace)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|device| {
                device.user_public_key == person && (with_this || device.device_public_key != this)
            })
            .collect()
    }
}

impl DeviceDirectory for OtherDevices {
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>> {
        Box::pin(async move {
            let this = self.0.whose.1;
            let mut reach: BTreeMap<[u8; 32], Reach> = BTreeMap::new();
            for workspace in self.0.places.workspaces() {
                for device in self.devices_in(&workspace, true).await {
                    let key = device.device_public_key;
                    let answer = if key == this || self.0.places.reachable(&workspace, key) {
                        Ok(())
                    } else {
                        Err(t("other-devices-not-connected"))
                    };
                    match reach.get_mut(&key) {
                        Some(known) if known.answer.is_err() => known.answer = answer,
                        Some(_) => {}
                        None => {
                            reach.insert(
                                key,
                                Reach {
                                    device,
                                    this: key == this,
                                    answer,
                                },
                            );
                        }
                    }
                }
            }
            reach.into_values().collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use tilo_mcp::devices::{agent_id, forward_request, listing};

    use super::*;

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

        phase an_agent_from_another_device_is_named_by_it;
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

        phase the_listing_says_which_device_answers;
        {
            let text = listing(&[
                Reach {
                    device: certificate("Here", 1),
                    this: true,
                    answer: Ok(()),
                },
                Reach {
                    device: certificate("Build box", 2),
                    this: false,
                    answer: Ok(()),
                },
                Reach {
                    device: certificate("Phone", 3),
                    this: false,
                    answer: Err("it does not answer".into()),
                },
            ]);
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3, "{text}");
            assert!(lines[0].starts_with("Here  ") && lines[0].ends_with("this device"));
            assert!(lines[1].contains(&hex::encode([2_u8; 32])) && lines[1].ends_with("reachable"));
            assert!(lines[2].ends_with("not reachable: it does not answer"));
            assert!(
                listing(&[Reach {
                    device: certificate("Here", 1),
                    this: true,
                    answer: Ok(()),
                }])
                .starts_with("No other device")
            );
        }
    }
}
