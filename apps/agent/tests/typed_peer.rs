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
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io;
use std::path::Path;
use std::time::Duration;

use ed25519_dalek::{Signer as _, SigningKey};
use qos_agent::wire::Link;
use ui::collaboration::{CertifiedEndpointIdentity, CollabEvent, CollabRequest, DocumentMutation};

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
    let mut certificate = qos_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: handle.to_owned(),
        capabilities: qos_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    qos_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs test device");
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
            handle,
        } = request
        else {
            anyhow::bail!("the first request must announce the peer route")
        };
        self.link = Some(Link::connect_endpoint(
            path,
            app_id,
            *cluster_id,
            record.clone(),
            handle.clone(),
            Some(endpoint_identity(handle.as_deref().unwrap_or(app_id))),
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

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout.unwrap_or(Duration::from_millis(200));
    }
}

/// # Errors
///
/// Returns an error when announcing the route fails (for a `Hello` request)
/// or when the peer's underlying send fails.
pub fn send(peer: &mut TypedPeer, request: &CollabRequest) -> io::Result<()> {
    let result = if matches!(request, CollabRequest::Hello { .. }) {
        let path = peer.path.clone();
        peer.announce(&path, request)
    } else {
        peer.send(request)
    };
    result.map_err(io::Error::other)
}

/// # Panics
///
/// Panics if the underlying typed link errors, which in these tests means
/// the collaboration broker has gone away.
pub fn recv(peer: &mut TypedPeer) -> Option<CollabEvent> {
    peer.recv()
        .expect("receive from typed collaboration service")
}

pub fn document_mutation(event: &CollabEvent) -> Option<DocumentMutation> {
    event.document_mutation()?.ok()
}

/// # Panics
///
/// Panics if the typed collaboration service cannot bind `path`.
pub fn spawn(path: &Path) -> shell::shell::collab_broker::BrokerHandle {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("person key creates a person principal")
    };
    let mut workspace = shell::shell::state::Workspace::test_default(
        shell::shell::state::WorkspaceId(1),
        "Agent integration room",
    );
    workspace.room = shell::shell::state::RoomId::parse(&TEST_ROOM.to_string()).unwrap();
    workspace.set_owner(principal);
    let mut tree = shell::shell::state::WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let trusted_devices = ["quinten", "Scribe", "files"]
        .map(|handle| device_key(handle).verifying_key().to_bytes())
        .into_iter()
        .collect();
    shell::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        tree.shared(),
        trusted_devices,
    )
    .expect("bind the typed collaboration service")
}
