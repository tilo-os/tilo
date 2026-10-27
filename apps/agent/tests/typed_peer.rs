//! Small app-side peer used by the agent's integration tests.
//!
//! The assertions in these tests predate the native collaboration service,
//! so the adapter deliberately retains their request/event-shaped driving
//! code while putting every byte on the production Cap'n Proto endpoint.

#![allow(
    dead_code,
    reason = "each integration binary uses a different adapter subset"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;
use std::time::Duration;

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_agent::wire::Link;
use ui::collaboration::{CertifiedEndpointIdentity, CollabEvent, CollabRequest};

const TEST_ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x6f2a_5fce_f612_46ac_9228_f6a3_ace4_9a11);
const ROOT_KEY: [u8; 32] = [91; 32];

pub const fn room() -> uuid::Uuid {
    TEST_ROOM
}

fn device_key(handle: &str) -> SigningKey {
    let mut seed = [0_u8; 32];
    for (index, byte) in handle.bytes().enumerate() {
        seed[index % 32] ^= byte;
    }
    seed[31] ^= 0xA5;
    SigningKey::from_bytes(&seed)
}

/// # Panics
///
/// Panics if the deterministic test certificate or presentation cannot encode.
pub fn endpoint_identity(handle: &str) -> CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = device_key(handle);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: handle.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs test device");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: TEST_ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &TEST_ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    CertifiedEndpointIdentity::new(presentation, device)
}

pub struct TypedPeer {
    link: Option<Link>,
    path: std::path::PathBuf,
    timeout: Duration,
}

impl TypedPeer {
    pub fn connect(path: &Path) -> Self {
        Self {
            link: None,
            path: path.to_path_buf(),
            timeout: Duration::from_millis(200),
        }
    }

    /// # Errors
    ///
    /// Returns an error when `request` is not `CollabRequest::Hello`, or when
    /// the typed collaboration client fails to connect to `path`.
    pub fn announce(&mut self, path: &Path, request: &CollabRequest) -> anyhow::Result<()> {
        let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind: _,
            handle,
        } = request
        else {
            anyhow::bail!("the first request must announce the peer route")
        };
        self.link = Some(Link::connect_person_endpoint(
            path,
            app_id,
            *cluster_id,
            record.clone(),
            handle.clone(),
            endpoint_identity(handle.as_deref().unwrap_or(app_id)),
        )?);
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to send.
    pub fn send(&mut self, request: &CollabRequest) -> anyhow::Result<()> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .send(request)
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to
    /// receive.
    pub fn recv(&mut self) -> anyhow::Result<Option<CollabEvent>> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .recv(self.timeout)
    }
}

/// Spawn the production broker harness with precisely the action grants a
/// scenario reviews. Document-write authority is separate so a denied agent
/// can still write a human-readable refusal without discovering the denied
/// action.
///
/// # Panics
///
/// Panics if the deterministic test room cannot be configured or its native
/// collaboration broker cannot bind `path`.
pub fn spawn_with_agent_actions(
    path: &Path,
    actions: &[(&str, &str, &str)],
) -> shell::shell::collab_broker::BrokerHandle {
    spawn_with_agent_grants(path, actions, &[])
}

/// [`spawn_with_agent_actions`], with the agent refused the `denied`
/// actions outright: what its operator preset would otherwise let it do.
///
/// # Panics
///
/// As [`spawn_with_agent_actions`].
#[allow(
    clippy::too_many_lines,
    reason = "builds the whole agent test room in one place"
)]
pub fn spawn_with_agent_grants(
    path: &Path,
    actions: &[(&str, &str, &str)],
    denied: &[(&str, &str, &str)],
) -> shell::shell::collab_broker::BrokerHandle {
    let parent = path.parent().expect("typed broker socket has a parent");
    shell::shell::collab_broker_capnp::tests::secure_socket_parent(parent)
        .expect("secure typed broker fixture root");
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("person key creates a person principal")
    };
    let mut workspace = shell::shell::state::Workspace::in_room(
        shell::shell::state::WorkspaceId(1),
        shell::shell::state::RoomId::parse(&TEST_ROOM.to_string()).unwrap(),
        "Agent integration room",
    );
    workspace.set_owner(principal.clone());
    let owner = tilo_action_policy::Subject::person(principal);
    // Operators answer in the room's documents.
    workspace.admit_agent_preset(
        "scribe",
        tilo_action_policy::roles::AgentPreset::Operator,
        &owner,
    );
    let everything = |id: String, agent: &str, node: tilo_action_policy::TreePath| {
        tilo_action_policy::Grant::new(
            id,
            tilo_action_policy::Subject::agent(agent),
            node,
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            owner.clone(),
        )
    };
    // In an app it was given actions of, Scribe takes exactly those: every
    // other action there is refused, its documents stay its to answer in.
    for (app, resource, action) in actions {
        workspace.grant(tilo_action_policy::Grant::new(
            format!("reviewed:{app}:other-actions"),
            tilo_action_policy::Subject::agent("scribe"),
            workspace
                .node()
                .app(*app)
                .rest()
                .any(tilo_action_policy::Level::Action),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Deny,
            owner.clone(),
        ));
        let node = workspace
            .node()
            .app(*app)
            .rest()
            .resource(*resource)
            .rest()
            .action(*action);
        workspace.grant(everything(
            format!("reviewed:{app}:{resource}:{action}"),
            "scribe",
            node,
        ));
    }
    for (app, resource, action) in denied {
        let node = workspace
            .node()
            .app(*app)
            .rest()
            .resource(*resource)
            .rest()
            .action(*action);
        workspace.grant(tilo_action_policy::Grant::new(
            format!("refused:{app}:{resource}:{action}"),
            tilo_action_policy::Subject::agent("scribe"),
            node,
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Deny,
            owner.clone(),
        ));
    }
    // App fixtures use the same agent-only endpoint as Scribe. Device
    // enrollment, room membership, and exact app authority remain separate.
    for host in ["quinten", "photos", "files"] {
        workspace.admit_agent_preset(
            host,
            tilo_action_policy::roles::AgentPreset::Operator,
            &owner,
        );
    }
    // Integration fixtures exercise the canonical routes and the dev.tilo test
    // routes. Grant each observed route explicitly: the production capability
    // check deliberately does not alias app authority.
    for (host, app) in [
        ("quinten", "dev.tilo.word-processor"),
        ("quinten", "so.tilo.word-processor"),
        ("photos", "dev.tilo.photos"),
        ("photos", "so.tilo.photos"),
        ("files", "so.tilo.file-explorer"),
    ] {
        let node = workspace.node().app(app);
        workspace.grant(everything(format!("fixture:{host}:{app}"), host, node));
    }
    let mut tree = shell::shell::state::WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let trusted_devices = ["quinten", "Scribe", "files", "photos"]
        .map(|handle| device_key(handle).verifying_key().to_bytes())
        .into_iter()
        .collect();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        tree.shared(),
        trusted_devices,
    )
    .expect("bind the typed collaboration service");
    // The fixture apps read the catalogue to see their own publication land,
    // as an app declaring `[actions] query` may.
    broker
        .app_action_broker()
        .expect("the harness serves actions")
        .set_app_action_access(std::sync::Arc::new(|_: &str| {
            shell::shell::app_action_access::ActionAccess {
                query: true,
                invoke: false,
            }
        }));
    broker
}
