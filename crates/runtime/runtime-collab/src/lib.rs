//! `runtime-collab` — Cap'n Proto client for the Tilo
//! collaboration broker.
//!
//! ## Usage shape
//!
//! ```ignore
//! let local = tokio::task::LocalSet::new();
//! local.run_until(async {
//!     let broker = runtime_collab::try_connect()
//!         .map_err(|e| { eprintln!("collab unavailable: {e}"); e })?;
//!     let channel = broker
//!         .join_channel("channel-id", identity_bytes)
//!         .await?;
//!     channel.send(op_bytes).await?;
//!     let mut sub = channel.subscribe().await?;
//!     while let Ok(next) = sub.read().await {
//!         handle(next);
//!     }
//! }).await;
//! ```
//!
//! Every call returns [`NotAvailable`] if the broker is missing or
//! the connection drops, never a panic.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
#![allow(
    clippy::future_not_send,
    reason = "every future here wraps an Rc-held capnp-rpc capability; capnp-rpc futures \
              are inherently !Send and every call is documented to require a \
              tokio::task::LocalSet-driven context, so this is not fixable without \
              redesigning around the RPC library"
)]

#[cfg(feature = "client")]
use std::rc::Rc;

#[cfg(feature = "client")]
use runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Checked-in bindings let app developers build `tilo-app` without installing
// the Cap'n Proto compiler. build.rs keeps the schema and generated source in
// lockstep with a checksum gate.
#[allow(
    unused,
    dead_code,
    clippy::all,
    clippy::nursery,
    clippy::pedantic,
    clippy::restriction,
    reason = "generated Cap'n Proto bindings do not follow repository lint policy"
)]
#[cfg(feature = "client")]
pub mod collab_capnp {
    include!("generated/collab_capnp.rs");
}

#[cfg(feature = "client")]
use capnp_rpc::rpc_twoparty_capnp;

/// Opaque capability naming MLS state held by the shell's vault provider.
/// Bytes are useful only while registered in that provider; this type carries
/// neither a key nor a filesystem path.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct MlsVaultHandle([u8; 32]);

impl MlsVaultHandle {
    /// Wrap a freshly random identifier issued by a trusted broker/vault.
    /// Possession alone is insufficient: the receiving provider must have the
    /// identifier in its capability registry.
    pub fn from_issued_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque handle for one vault-registered MLS key package.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct MlsKeyPackageHandle([u8; 32]);

impl MlsKeyPackageHandle {
    pub fn from_issued_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque broker receipt proving an encrypted snapshot reached durable
/// document history. It resolves to a digest only in the issuing broker.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct MlsSnapshotReceipt([u8; 32]);

impl MlsSnapshotReceipt {
    pub fn from_issued_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Broker-internal MLS membership command. This is not exposed
/// as a `CollabService` RPC: applications never submit governance commands.
/// The shell's authenticated workspace authority constructs it from live
/// membership state and vault-issued handles.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsAuthorityTransaction {
    pub idempotency_key: [u8; 32],
    pub channel: [u8; 16],
    pub workspace: String,
    pub expected_membership_revision: u64,
    pub vault: MlsVaultHandle,
    pub action: MlsAuthorityAction,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MlsAuthorityAction {
    Admit {
        recipient: [u8; 32],
        role: MlsWorkspaceRole,
        key_package: MlsKeyPackageHandle,
    },
    Remove {
        recipient: [u8; 32],
        leaf_index: u32,
    },
    EnableEncryption {
        snapshot: MlsSnapshotReceipt,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MlsWorkspaceRole {
    Admin,
    Editor,
    Commenter,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceControlRoute {
    Direct,
    Relayed { via: [u8; 32] },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceControlCloseReason {
    Denied(String),
    Revoked,
    TargetDisconnected,
    RelayDisconnected,
    LocalDisconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceControlEvent {
    WaitingForConsent {
        route: WorkspaceControlRoute,
    },
    StreamStarted {
        surface: u64,
        width: u32,
        height: u32,
    },
    Frame {
        surface: u64,
        encoded_h264: Vec<u8>,
        timestamp: u64,
    },
    Closed(WorkspaceControlCloseReason),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceControlInput {
    PointerMotion {
        x: f64,
        y: f64,
    },
    PointerButton {
        button: u32,
        pressed: bool,
    },
    Key {
        keycode: u32,
        pressed: bool,
        modifiers: u32,
    },
    KeyboardFocus {
        focused: bool,
    },
}

#[cfg(feature = "client")]
/// Client endpoint for an application-owned paged action directory.  Apps
/// implement the matching `action_directory::Server` and hand this capability
/// to [`ChannelHandle::publish_action_directory`]; the broker owns neither the
/// widget nor a complete copy of its rows.
#[cfg(feature = "client")]
pub use collab_capnp::action_directory::Client as ActionDirectoryClient;
#[cfg(feature = "client")]
pub use collab_capnp::action_stream::Client as ActionStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::blob_stream::Client as BlobStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::collab_service::Client as CollabServiceClient;
#[cfg(feature = "client")]
pub use collab_capnp::context_stream::Client as ContextStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::invocation_stream::Client as InvocationStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::presence_stream::Client as PresenceStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::realtime_stream::Client as RealtimeStreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::stream::Client as StreamClient;
#[cfg(feature = "client")]
pub use collab_capnp::task_run_stream::Client as TaskRunStreamClient;

/// Version tag distinguishing a certified join presentation from invalid
/// identity bytes.
pub const CERTIFIED_JOIN_PREFIX: &[u8] = b"tilo-collab-certified-join-v1\0";

/// Peer-verifiable proof that an enrolled device joined one workspace.
/// `device_certificate` is the JSON form supplied by `runtime-identity`; the
/// proof is an Ed25519 device-key signature over
/// `runtime_identity::collaboration_join_signing_bytes(workspace_id)`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifiedJoinIdentity {
    pub workspace_id: String,
    pub device_certificate: Vec<u8>,
    pub proof: Vec<u8>,
}

impl CertifiedJoinIdentity {
    /// Encode with the reserved version prefix.
    ///
    /// # Errors
    ///
    /// Returns postcard's serialization error if the presentation cannot be
    /// encoded.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        let encoded = postcard::to_stdvec(self)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CERTIFIED_JOIN_PREFIX);
        bytes.extend_from_slice(&encoded);
        Ok(bytes)
    }

    /// Decode a certified presentation. `Ok(None)` means the bytes do not
    /// carry the certified prefix and must be rejected by the broker.
    ///
    /// # Errors
    ///
    /// Returns postcard's deserialization error for malformed certified
    /// payloads.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, postcard::Error> {
        let Some(encoded) = bytes.strip_prefix(CERTIFIED_JOIN_PREFIX) else {
            return Ok(None);
        };
        postcard::from_bytes(encoded).map(Some)
    }
}

/// Try to dial the Tilo collab broker via the launcher-supplied service
/// fd. Returns [`NotAvailable`] when the env var is unset (likely: app
/// running as a Flatpak on stock Linux) or the fd is no longer open.
///
/// The returned client is the typed root capability; wrap it in a
/// [`CollabClient`] for the idiomatic Rust surface. Must be called from
/// within a `tokio::task::LocalSet`-driven context — capnp-rpc futures
/// are `!Send`.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if the launcher didn't hand
/// this process a collab service fd, or another [`NotAvailable`] variant
/// if the connection fails to establish.
#[cfg(feature = "client")]
pub fn try_connect() -> Result<CollabClient, NotAvailable> {
    let fd = service_fd("collab").ok_or(NotAvailable::ServiceFdMissing)?;
    let unix = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    let client: CollabServiceClient = unix.bootstrap();
    Ok(CollabClient::from_raw(client))
}

/// Idiomatic Rust wrapper over the generated [`CollabServiceClient`].
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct CollabClient {
    inner: Rc<CollabServiceClient>,
}

#[cfg(feature = "client")]
impl CollabClient {
    /// Wrap an existing generated client (mainly useful for tests
    /// that own their own RPC system).
    pub fn from_raw(client: CollabServiceClient) -> Self {
        Self {
            inner: Rc::new(client),
        }
    }

    /// Join (or implicitly create) a channel by id, presenting the
    /// given opaque identity blob. `identity` is the postcard
    /// encoding of a `collab-types` identity record — opaque
    /// from the broker's perspective.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn join_channel(
        &self,
        channel_id: &str,
        identity: &[u8],
    ) -> Result<ChannelHandle, NotAvailable> {
        self.join_channel_as(channel_id, identity, "").await
    }

    /// Join a channel while announcing the participant's display handle.
    /// Empty handles omit the optional display label; authenticated identity
    /// still comes from the certified join presentation.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn join_channel_as(
        &self,
        channel_id: &str,
        identity: &[u8],
        participant_handle: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        self.join_channel_as_kind(channel_id, identity, participant_handle, "person")
            .await
    }

    /// Join as an explicitly typed participant. The broker validates an
    /// agent request against the workspace; this is not a display-name hint.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if admission fails or the broker connection
    /// is unavailable.
    pub async fn join_channel_as_kind(
        &self,
        channel_id: &str,
        identity: &[u8],
        participant_handle: &str,
        participant_kind: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        let mut req = self.inner.join_channel_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            builder.set_identity(identity);
            builder.set_participant_handle(participant_handle);
            builder.set_participant_kind(participant_kind);
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(ChannelHandle {
            inner: Rc::new(handle),
        })
    }
}

/// A subscribed-to channel. Holds a typed capability backed by the
/// broker; dropping the handle closes the subscription. Cheap to
/// clone (the underlying capability is reference-counted).
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct ChannelHandle {
    inner: Rc<collab_capnp::channel_handle::Client>,
}

/// Maximum encoded size of one Nearby Send control command or reply. Transfer
/// bytes never use this lane; the shell's existing resumable object transport
/// owns payload delivery.
pub const MAX_NEARBY_TRANSFER_WIRE_BYTES: usize = 64 * 1024;
/// Maximum certified devices returned to one picker snapshot.
pub const MAX_NEARBY_TRANSFER_TARGETS: usize = 32;
pub const MAX_NEARBY_TRANSFER_DESTINATIONS: usize = 16;
/// Maximum active/recent transfers returned to one status snapshot.
pub const MAX_NEARBY_TRANSFER_STATUSES: usize = 128;
/// Maximum UTF-8 byte length of a device label or rejection reason.
pub const MAX_NEARBY_TRANSFER_LABEL_BYTES: usize = 256;
/// Maximum UTF-8 byte length of a portable transfer display name.
pub const MAX_NEARBY_TRANSFER_NAME_BYTES: usize = 512;
/// Maximum raw byte length of one exact Files source path.
pub const MAX_NEARBY_TRANSFER_PATH_BYTES: usize = 4_096;
/// Maximum encoded size of one local Settings Wi-Fi continuity request.
pub const MAX_WIFI_CONTINUITY_WIRE_BYTES: usize = 4 * 1024;
pub const MAX_WIFI_SSID_BYTES: usize = 256;
pub const MAX_WIFI_PASSWORD_BYTES: usize = 512;

/// Portable Wi-Fi authentication modes understood by the sealed personal
/// secret bridge. Enterprise identity/certificate state is intentionally not
/// representable here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortableWifiSecurity {
    Wpa2,
    Wpa3,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WifiContinuityCommand {
    Lookup {
        ssid: String,
        security: PortableWifiSecurity,
    },
    Share {
        ssid: String,
        security: PortableWifiSecurity,
        password: String,
    },
    ShareSavedProfile {
        /// Local `NetworkManager` capability; never serialized into fabric state.
        profile_uuid: String,
    },
}

impl std::fmt::Debug for WifiContinuityCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lookup { ssid, security } => formatter
                .debug_struct("Lookup")
                .field("ssid", ssid)
                .field("security", security)
                .finish(),
            Self::Share { ssid, security, .. } => formatter
                .debug_struct("Share")
                .field("ssid", ssid)
                .field("security", security)
                .field("password", &"<redacted>")
                .finish(),
            Self::ShareSavedProfile { profile_uuid } => formatter
                .debug_struct("ShareSavedProfile")
                .field("profile_uuid", profile_uuid)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WifiContinuityReply {
    Password { password: String },
    NotFound,
    Shared { ssid: String },
    Rejected { reason: String },
}

impl std::fmt::Debug for WifiContinuityReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { .. } => formatter
                .debug_struct("Password")
                .field("password", &"<redacted>")
                .finish(),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Shared { ssid } => formatter
                .debug_struct("Shared")
                .field("ssid", ssid)
                .finish(),
            Self::Rejected { reason } => formatter
                .debug_struct("Rejected")
                .field("reason", reason)
                .finish(),
        }
    }
}

impl WifiContinuityCommand {
    pub fn valid(&self) -> bool {
        let valid_ssid = |ssid: &str| !ssid.is_empty() && ssid.len() <= MAX_WIFI_SSID_BYTES;
        match self {
            Self::Lookup { ssid, .. } => valid_ssid(ssid),
            Self::Share { ssid, password, .. } => {
                valid_ssid(ssid)
                    && !password.is_empty()
                    && password.len() <= MAX_WIFI_PASSWORD_BYTES
            }
            Self::ShareSavedProfile { profile_uuid } => valid_uuid(profile_uuid),
        }
    }
}

impl WifiContinuityReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::Password { password } => {
                !password.is_empty() && password.len() <= MAX_WIFI_PASSWORD_BYTES
            }
            Self::NotFound => true,
            Self::Shared { ssid } => !ssid.is_empty() && ssid.len() <= MAX_WIFI_SSID_BYTES,
            Self::Rejected { reason } => !reason.is_empty() && reason.len() <= 512,
        }
    }
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// A short-lived shell capability naming one picker target. It
/// carries no cryptographic peer identity and has no authority outside the
/// authenticated app connection that received it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NearbyTargetHandle([u8; 16]);

impl NearbyTargetHandle {
    /// Wrap random bytes freshly issued by the shell's authenticated broker.
    pub fn from_issued_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Return the opaque bytes for equality, serialization, and stable action ids.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether this is a structurally valid shell-issued handle.
    pub fn valid(self) -> bool {
        self.0 != [0; 16]
    }
}

/// A short-lived shell capability naming one transfer for one authenticated
/// Files connection. It is never the transport's global transfer identity.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NearbyTransferHandle([u8; 16]);

impl NearbyTransferHandle {
    /// Wrap random bytes freshly issued by the shell's authenticated broker.
    pub fn from_issued_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Return opaque bytes for equality and serialization.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether this is a structurally valid shell-issued handle.
    pub fn valid(self) -> bool {
        self.0 != [0; 16]
    }
}

/// A short-lived shell capability naming one receiver destination for one
/// authenticated Files connection. Applications never receive its host path.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NearbyDestinationHandle([u8; 16]);

impl NearbyDestinationHandle {
    pub fn from_issued_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn valid(self) -> bool {
        self.0 != [0; 16]
    }
}

/// Path-free receiver destination projected from this Files instance's
/// current writable grants.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyTransferDestination {
    pub handle: NearbyDestinationHandle,
    pub label: String,
    pub remembered_for_session: bool,
}

/// A certified personal-device target safe to show in an application picker.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyTransferTarget {
    pub handle: NearbyTargetHandle,
    pub label: String,
    pub online: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NearbyTransferDirection {
    Sending,
    Receiving,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NearbyTransferPhase {
    AwaitingConsent,
    AwaitingDestination,
    Queued,
    Transferring,
    Completed,
    Failed,
    Cancelled,
}

/// Bounded status projection. Paths and file contents are absent:
/// receiver UI needs a portable display name, exact peer and byte progress.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyTransferStatus {
    pub handle: NearbyTransferHandle,
    pub name: String,
    pub device_label: String,
    pub direction: NearbyTransferDirection,
    pub phase: NearbyTransferPhase,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyTransferSnapshot {
    pub targets: Vec<NearbyTransferTarget>,
    pub destinations: Vec<NearbyTransferDestination>,
    pub transfers: Vec<NearbyTransferStatus>,
}

/// Commands accepted by the shell-owned Nearby Send authority. `source_path`
/// is raw Unix path bytes so a valid file never becomes inaccessible merely
/// because its name is not UTF-8. The broker accepts `Send` only from the
/// certified Files application and revalidates the exact target and source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NearbyTransferCommand {
    Snapshot,
    Send {
        target: NearbyTargetHandle,
        source_path: Vec<u8>,
    },
    Decide {
        handle: NearbyTransferHandle,
        accept: bool,
        destination: Option<NearbyDestinationHandle>,
        remember_for_session: bool,
    },
    Cancel {
        handle: NearbyTransferHandle,
    },
    Retry {
        handle: NearbyTransferHandle,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NearbyTransferReply {
    Snapshot { snapshot: NearbyTransferSnapshot },
    Queued { handle: NearbyTransferHandle },
    Accepted,
    Rejected { reason: String },
}

impl NearbyTransferCommand {
    /// Structural validation shared by clients and the shell broker. This is
    /// independent of authorization: the broker must still bind
    /// the app, source path, transfer id and device to its live route.
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot => true,
            Self::Decide {
                handle,
                accept,
                destination,
                remember_for_session,
            } => {
                handle.valid()
                    && if *accept {
                        destination.is_some_and(NearbyDestinationHandle::valid)
                    } else {
                        destination.is_none() && !remember_for_session
                    }
            }
            Self::Cancel { handle } | Self::Retry { handle } => handle.valid(),
            Self::Send {
                target,
                source_path,
            } => {
                target.valid()
                    && !source_path.is_empty()
                    && source_path.len() <= MAX_NEARBY_TRANSFER_PATH_BYTES
                    && !source_path.contains(&0)
            }
        }
    }
}

impl NearbyTransferReply {
    /// Validate the bounded, path-free status projection returned by a broker.
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot { snapshot } => {
                snapshot.targets.len() <= MAX_NEARBY_TRANSFER_TARGETS
                    && snapshot.destinations.len() <= MAX_NEARBY_TRANSFER_DESTINATIONS
                    && snapshot.transfers.len() <= MAX_NEARBY_TRANSFER_STATUSES
                    && snapshot.targets.iter().all(|target| {
                        target.handle.valid()
                            && !target.label.is_empty()
                            && target.label.len() <= MAX_NEARBY_TRANSFER_LABEL_BYTES
                    })
                    && snapshot.destinations.iter().all(|destination| {
                        destination.handle.valid()
                            && !destination.label.is_empty()
                            && destination.label.len() <= MAX_NEARBY_TRANSFER_LABEL_BYTES
                    })
                    && snapshot.transfers.iter().all(|transfer| {
                        !transfer.name.is_empty()
                            && transfer.name.len() <= MAX_NEARBY_TRANSFER_NAME_BYTES
                            && !transfer.device_label.is_empty()
                            && transfer.device_label.len() <= MAX_NEARBY_TRANSFER_LABEL_BYTES
                            && transfer.transferred_bytes <= transfer.total_bytes
                    })
            }
            Self::Queued { handle } => handle.valid(),
            Self::Accepted => true,
            Self::Rejected { reason } => {
                !reason.is_empty() && reason.len() <= MAX_NEARBY_TRANSFER_LABEL_BYTES
            }
        }
    }
}

#[cfg(feature = "client")]
impl ChannelHandle {
    /// Ask the broker to direct one current route holder to publish a complete
    /// document snapshot for this late joiner.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker cannot accept or complete the
    /// snapshot request.
    pub async fn request_document_snapshot(&self) -> Result<(), NotAvailable> {
        self.inner
            .request_document_snapshot_request()
            .send()
            .promise
            .await?;
        Ok(())
    }

    /// Submit one bounded realtime-session continuity command. The broker
    /// authenticates `app_id` against this channel and returns a versioned
    /// application-contract reply.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker rejects the command or the
    /// local capability disconnects.
    pub async fn realtime_command(
        &self,
        app_id: &str,
        command_json: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        let mut request = self.inner.realtime_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(command_json);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_reply_json()?.to_vec())
    }

    /// Submit one bounded Nearby Send control command. File contents remain on
    /// the existing shell-owned resumable transport; this request carries only
    /// picker/status/consent control data.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker disconnects, rejects malformed
    /// Cap'n Proto data, or returns an oversized/invalid typed reply.
    pub async fn nearby_transfer_command(
        &self,
        app_id: &str,
        command: &NearbyTransferCommand,
    ) -> Result<NearbyTransferReply, NotAvailable> {
        if !command.valid() {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "invalid nearby transfer command".into(),
            )));
        }
        let command_json = serde_json::to_vec(command).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "encode nearby transfer command: {error}"
            )))
        })?;
        if command_json.len() > MAX_NEARBY_TRANSFER_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "nearby transfer command exceeds wire bound".into(),
            )));
        }
        let mut request = self.inner.nearby_transfer_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply_json()?;
        if bytes.len() > MAX_NEARBY_TRANSFER_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "nearby transfer reply exceeds wire bound".into(),
            )));
        }
        let decoded: NearbyTransferReply = serde_json::from_slice(bytes).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "decode nearby transfer reply: {error}"
            )))
        })?;
        if !decoded.valid() {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "invalid nearby transfer reply".into(),
            )));
        }
        Ok(decoded)
    }

    /// Use the shell-owned sealed Wi-Fi secret bridge from the authenticated
    /// Settings route. Plaintext is confined to this connected local
    /// descriptor and the immediate `NetworkManager` call.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the command or reply violates its wire
    /// contract, serialization fails, or the local broker request fails.
    pub async fn wifi_continuity_command(
        &self,
        app_id: &str,
        command: &WifiContinuityCommand,
    ) -> Result<WifiContinuityReply, NotAvailable> {
        if !command.valid() {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "invalid Wi-Fi continuity command".into(),
            )));
        }
        let command_json = serde_json::to_vec(command).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "encode Wi-Fi continuity command: {error}"
            )))
        })?;
        if command_json.len() > MAX_WIFI_CONTINUITY_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "Wi-Fi continuity command exceeds wire bound".into(),
            )));
        }
        let mut request = self.inner.wifi_continuity_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply_json()?;
        if bytes.len() > MAX_WIFI_CONTINUITY_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "Wi-Fi continuity reply exceeds wire bound".into(),
            )));
        }
        let decoded: WifiContinuityReply = serde_json::from_slice(bytes).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "decode Wi-Fi continuity reply: {error}"
            )))
        })?;
        if !decoded.valid() {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "invalid Wi-Fi continuity reply".into(),
            )));
        }
        Ok(decoded)
    }

    /// Subscribe to lifecycle edges addressed to this exact application
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the local broker is unavailable.
    pub async fn subscribe_realtime(&self) -> Result<RealtimeStream, NotAvailable> {
        let reply = self
            .inner
            .subscribe_realtime_request()
            .send()
            .promise
            .await?;
        Ok(RealtimeStream {
            inner: Rc::new(reply.get()?.get_stream()?),
        })
    }

    /// Open the certified workspace-control capability for `target_device`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker request fails or its response
    /// does not contain a valid workspace-control capability.
    pub async fn open_workspace_control(
        &self,
        target_device: [u8; 32],
    ) -> Result<WorkspaceControlHandle, NotAvailable> {
        let mut request = self.inner.open_workspace_control_request();
        request.get().set_target_device(&target_device);
        let reply = request.send().promise.await?;
        Ok(WorkspaceControlHandle {
            inner: Rc::new(reply.get()?.get_handle()?),
        })
    }
    /// Report a bounded, inert continuation projection to the local shell.
    /// The broker binds `app_id` to this certified channel rather than trusting
    /// the caller's claim, and never relays this report to collaboration peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker request fails.
    pub async fn report_continuation(
        &self,
        app_id: &str,
        schema_version: u16,
        metadata: &[u8],
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.report_continuation_request();
        let mut builder = req.get();
        builder.set_app_id(app_id);
        builder.set_schema_version(schema_version);
        builder.set_metadata(metadata);
        req.send().promise.await?;
        Ok(())
    }

    /// Clear this application's previously reported continuation projection.
    /// The broker binds the request to the certified channel route exactly as
    /// it does for [`Self::report_continuation`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker request fails.
    pub async fn clear_continuation(&self, app_id: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.clear_continuation_request();
        req.get().set_app_id(app_id);
        req.send().promise.await?;
        Ok(())
    }

    /// Replace this process's private media handoff descriptors. The shell
    /// authenticates the channel route and never relays `reports_json` as room
    /// collaboration state.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker request fails.
    pub async fn report_playback(
        &self,
        app_id: &str,
        reports_json: &[u8],
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.report_playback_request();
        req.get().set_app_id(app_id);
        req.get().set_reports_json(reports_json);
        req.send().promise.await?;
        Ok(())
    }

    /// Submit a single signed op into the channel. The op blob is
    /// a postcard-encoded `collab-types::SignedOp`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_request();
        req.get().set_op(op);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of ops. The returned
    /// [`OpStream`] yields one op per `read` call; the consumer is
    /// expected to drive it in a loop. Stream lifetime is bounded
    /// by the parent channel handle.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable> {
        let req = self.inner.subscribe_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(OpStream {
            inner: Rc::new(stream),
        })
    }

    /// Open the channel's presence sub-handle.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn presence(&self) -> Result<PresenceHandle, NotAvailable> {
        let req = self.inner.presence_request();
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(PresenceHandle {
            inner: Rc::new(handle),
        })
    }

    /// Subscribe to roster + permission updates. The broker emits a
    /// fresh [`ChannelContext`] whenever its view changes; the
    /// stream is client-pulled like every other stream in this
    /// schema.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_context(&self) -> Result<ContextStream, NotAvailable> {
        let req = self.inner.subscribe_context_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(ContextStream {
            inner: Rc::new(stream),
        })
    }

    /// Hand the broker content this app has just stored.
    ///
    /// A document holds a reference to its binaries, never the bytes, so
    /// this is the only way the content behind a reference leaves the
    /// process that produced it — an app's blob store lives inside its
    /// sandbox, so the shell cannot go and read it directly.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn offer_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.offer_blob_request();
        {
            let mut b = req.get();
            b.set_hash(hash);
            b.set_bytes(bytes);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Ask for content a document here references and this app lacks.
    /// Returns as soon as the broker has the request, not when the content
    /// lands — the answer arrives on [`Self::subscribe_blobs`], so a caller
    /// doesn't block a frame on a transfer that may take several round trips.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn want_blob(&self, hash: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.want_blob_request();
        req.get().set_hash(hash);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of arrived content — answers to
    /// [`Self::want_blob`], and content a peer offered the room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_blobs(&self) -> Result<BlobStream, NotAvailable> {
        let req = self.inner.subscribe_blobs_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(BlobStream {
            inner: Rc::new(stream),
        })
    }

    /// Publish a typed action into the room, for whichever app owns the
    /// document it belongs to.
    ///
    /// Every other send verb carries Automerge bytes, which an app can only
    /// produce for a document it holds. This one carries an action, so an
    /// app can address a document it has no replica of and no schema for —
    /// it says what should happen, and the owning app decides whether it
    /// recognises the request. Fanned out at cluster scope; a receiver
    /// applies it only if its own registry knows `type_id`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn send_action(&self, type_id: &str, payload: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_action_request();
        {
            let mut b = req.get();
            b.set_type_id(type_id);
            b.set_payload(payload);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of actions other apps published into
    /// this room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_actions(&self) -> Result<ActionStream, NotAvailable> {
        let req = self.inner.subscribe_actions_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(ActionStream {
            inner: Rc::new(stream),
        })
    }

    /// Tell this session what this application currently has open and what
    /// may be done to it.
    ///
    /// Level-triggered: each call replaces this connection's whole published
    /// surface, so a closed resource stops being addressable rather than
    /// lingering. What crosses is inert — an [`ExportedAction`] carries ids
    /// and labels; the closure an `ActionSpec` holds never leaves the
    /// process that built it, so nothing published here can be fired by
    /// whoever reads it, only requested back through
    /// [`Self::subscribe_invocations`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call, or if `actions` is longer than a capnp list
    /// can address (`u32::MAX` entries).
    pub async fn publish_actions(
        &self,
        app_id: &str,
        actions: &[ExportedAction],
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_actions_request();
        {
            let mut b = req.get();
            b.set_app_id(app_id);
            let action_count = u32::try_from(actions.len())
                .map_err(|_| capnp::Error::failed("too many actions to publish".to_string()))?;
            let mut list = b.init_actions(action_count);
            for (i, a) in actions.iter().enumerate() {
                // `action_count` above already proved `actions.len()` fits in
                // u32, so every index here does too.
                let idx = u32::try_from(i).unwrap_or(u32::MAX);
                let mut row = list.reborrow().get(idx);
                row.set_resource(&a.resource);
                row.set_action(&a.action);
                row.set_resource_kind(&a.resource_kind);
                row.set_resource_label(&a.resource_label);
                row.set_action_label(&a.action_label);
                row.set_description(a.description.as_deref().unwrap_or(""));
                row.set_operation_kind(&a.operation_kind);
                row.set_params_json(&a.params_json);
                row.set_contract_json(&a.contract_json);
                if let Some(view) = &a.view {
                    let mut out = row.reborrow().init_view();
                    out.set_id(&view.id);
                    out.set_label(&view.label);
                    out.set_description(view.description.as_deref().unwrap_or(""));
                    out.set_open_action(&view.open_action);
                    out.set_primary(view.primary);
                    if let Some(mount) = &view.mount {
                        let mut out = out.init_mount();
                        out.set_action(&mount.action);
                        out.set_capability_param(&mount.capability_param);
                        out.set_view_param(&mount.view_param);
                    }
                }
                let mut input = row.init_input();
                match &a.input {
                    ExportedInput::None => input.set_none(()),
                    ExportedInput::Text {
                        prompt,
                        placeholder,
                        wants_content,
                    } => {
                        let mut t = input.init_text();
                        t.set_prompt(prompt);
                        t.set_placeholder(placeholder);
                        t.set_wants_content(*wants_content);
                    }
                }
            }
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Register an application-owned, paged action directory.
    ///
    /// This is additive to [`Self::publish_actions`]. It is intended for
    /// large or late-bound resource trees: only bounded query pages and exact
    /// resolutions cross the broker boundary, never widget ownership or a
    /// full resource corpus. Re-registering atomically replaces the previous
    /// directory for this connection.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during registration.
    pub async fn publish_action_directory(
        &self,
        app_id: &str,
        directory: ActionDirectoryClient,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_action_directory_request();
        {
            let mut builder = req.get();
            builder.set_app_id(app_id);
            builder.set_directory(directory);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of invocations the session has addressed
    /// to *this* application — distinct from [`Self::subscribe_actions`]
    /// (room-wide fan-out): an invocation names one connection, an
    /// instruction to one app to perform one operation it published.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_invocations(&self) -> Result<InvocationStream, NotAvailable> {
        let req = self.inner.subscribe_invocations_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(InvocationStream {
            inner: Rc::new(stream),
        })
    }

    /// Subscribe this admitted agent session to broker-addressed task runs.
    /// A person session and an offline agent receive no substitute queue.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_task_runs(&self) -> Result<TaskRunStream, NotAvailable> {
        let reply = self
            .inner
            .subscribe_task_runs_request()
            .send()
            .promise
            .await?;
        Ok(TaskRunStream {
            inner: Rc::new(reply.get()?.get_stream()?),
        })
    }

    /// Report an update for one exact broker-minted task lease.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when serialization or the broker call fails.
    pub async fn report_task_run(
        &self,
        run_id: u64,
        lease: &str,
        update: &TaskRunUpdate,
    ) -> Result<(), NotAvailable> {
        let update_json = serde_json::to_string(update).map_err(|error| {
            capnp::Error::failed(format!("task update serialization failed: {error}"))
        })?;
        let mut request = self.inner.report_task_run_request();
        let mut params = request.get();
        params.set_run_id(run_id);
        params.set_lease(lease);
        params.set_update_json(&update_json);
        request.send().promise.await?;
        Ok(())
    }

    /// Invoke a published application action under a leased task context.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker call fails or its update is not
    /// valid UTF-8.
    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments mirror the fixed Cap'n Proto task-action method"
    )]
    pub async fn invoke_task_action(
        &self,
        run_id: u64,
        lease: &str,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation_json: &str,
        invocation: &str,
    ) -> Result<(u64, String), NotAvailable> {
        let mut request = self.inner.invoke_task_action_request();
        {
            let mut params = request.get();
            params.set_run_id(run_id);
            params.set_lease(lease);
            params.set_peer_id(peer_id);
            params.set_resource(resource);
            params.set_action(action);
            params.set_argument(argument.unwrap_or(""));
            params.set_has_argument(argument.is_some());
            params.set_confirmation_json(confirmation_json);
            params.set_invocation(invocation);
        }
        let reply = request.send().promise.await?;
        let result = reply.get()?;
        Ok((
            result.get_id(),
            result
                .get_update_json()?
                .to_str()
                .map_err(|error| {
                    capnp::Error::failed(format!("invalid task invocation update: {error}"))
                })?
                .to_owned(),
        ))
    }

    /// Whether Task Center requested cooperative cancellation for this exact
    /// leased run. A `true` response is advisory to a running action, but an
    /// agent must not start another action after observing it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn task_run_cancelled(&self, run_id: u64, lease: &str) -> Result<bool, NotAvailable> {
        let mut request = self.inner.task_run_cancelled_request();
        let mut params = request.get();
        params.set_run_id(run_id);
        params.set_lease(lease);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_cancelled())
    }

    /// Report what became of an invocation. An empty `code` means it ran;
    /// anything else is a refusal, reported to whoever asked rather than
    /// swallowed — a caller that can't see the screen has no other way to
    /// tell a completed operation from one that quietly did nothing.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn complete_invocation(
        &self,
        id: u64,
        code: &str,
        message: &str,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.complete_invocation_request();
        {
            let mut b = req.get();
            b.set_id(id);
            b.set_code(code);
            b.set_message(message);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish an accepted/progress/terminal v2 lifecycle update. The broker
    /// validates ownership, invocation identity, ordering, and terminal-once.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn update_invocation(&self, id: u64, update_json: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.update_invocation_request();
        {
            let mut b = req.get();
            b.set_id(id);
            b.set_update_json(update_json);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Record a terminal local-UI effect in shared workspace activity.
    /// The broker validates that the address is a currently published Effect
    /// and reduces the update to a redacted receipt.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or refuses the receipt as unauthorised or
    /// malformed.
    pub async fn record_local_effect(
        &self,
        resource: &str,
        action: &str,
        update_json: &str,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.record_local_effect_request();
        {
            let mut builder = req.get();
            builder.set_resource(resource);
            builder.set_action(action);
            builder.set_update_json(update_json);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Whether the caller requested cooperative cancellation of this job.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn invocation_cancellation(&self, id: u64) -> Result<bool, NotAvailable> {
        let mut req = self.inner.invocation_cancellation_request();
        req.get().set_id(id);
        let reply = req.send().promise.await?;
        Ok(reply.get()?.get_cancelled())
    }

    /// Send one `tilo_actions::remote::RemoteActionFrame` request and
    /// return the answering frame. The broker derives the caller from this
    /// admitted channel; the frame never names it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection drops or the broker
    /// refuses the frame.
    pub async fn remote_action(&self, frame: &[u8]) -> Result<Vec<u8>, NotAvailable> {
        let mut request = self.inner.remote_action_request();
        request.get().set_frame(frame);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_frame()?.to_vec())
    }

    /// Ask this session to open the "say something about this" surface,
    /// addressed to the compositor on the far end of this socket, not the
    /// room. `reference` is a serialised `tilo_reference::ResourceRef`
    /// (JSON); `beside` is `(x, y, w, h)` **in this window's own
    /// coordinates** — the session converts, since only it knows where this
    /// window sits on a desktop. The panel can't be drawn here: it opens
    /// beside the app's context menu, and a Wayland client has no way to
    /// paint outside its own window.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "beside carries on-screen window coordinates, always well within f32's \
                  range; the wire schema is f32 and only sub-pixel precision is lost"
    )]
    pub async fn compose_remark(
        &self,
        reference: &str,
        beside: (f64, f64, f64, f64),
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.compose_remark_request();
        {
            let mut b = req.get();
            b.set_reference(reference);
            b.set_x(beside.0 as f32);
            b.set_y(beside.1 as f32);
            b.set_w(beside.2 as f32);
            b.set_h(beside.3 as f32);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Fetch a page of previously-validated signed ops on a specific
    /// artifact. `artifact_id` is the 16-byte little-endian encoding
    /// of the artifact uuid used by `collab-types`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn fetch_scrollback_for(
        &self,
        artifact_id: &[u8],
        before_seq: u64,
        max: u32,
    ) -> Result<ScrollbackBatch, NotAvailable> {
        let mut req = self.inner.fetch_scrollback_for_request();
        {
            let mut b = req.get();
            b.set_artifact_id(artifact_id);
            b.set_before_seq(before_seq);
            b.set_max(max);
        }
        let reply = req.send().promise.await?;
        let batch = reply.get()?.get_batch()?;
        let ops_reader = batch.get_ops()?;
        let mut ops = Vec::with_capacity(ops_reader.len() as usize);
        for i in 0..ops_reader.len() {
            ops.push(ops_reader.get(i)?.to_vec());
        }
        Ok(ScrollbackBatch {
            ops,
            earliest_seq: batch.get_earliest_seq(),
            latest_seq: batch.get_latest_seq(),
            done: batch.get_done(),
        })
    }
}

#[derive(Clone)]
#[cfg(feature = "client")]
pub struct WorkspaceControlHandle {
    inner: Rc<collab_capnp::workspace_control_handle::Client>,
}

#[cfg(feature = "client")]
impl WorkspaceControlHandle {
    /// Wait for the next workspace-control event.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker request fails or the returned
    /// event cannot be decoded.
    pub async fn read(&self) -> Result<WorkspaceControlEvent, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        let bytes = reply.get()?.get_event()?;
        postcard::from_bytes(bytes).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "invalid workspace-control event: {error}"
            )))
        })
    }

    /// Send one input event through this certified control capability.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the event cannot be encoded or the broker
    /// rejects or fails the request.
    pub async fn send_input(&self, event: &WorkspaceControlInput) -> Result<(), NotAvailable> {
        let bytes = postcard::to_allocvec(event).map_err(|error| {
            NotAvailable::Capnp(capnp::Error::failed(format!(
                "workspace-control input encode failed: {error}"
            )))
        })?;
        let mut request = self.inner.send_input_request();
        request.get().set_event(&bytes);
        request.send().promise.await?;
        Ok(())
    }

    /// Close this workspace-control capability.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker cannot complete the close
    /// request.
    pub async fn close(&self) -> Result<(), NotAvailable> {
        self.inner.close_request().send().promise.await?;
        Ok(())
    }
}

/// A signed op as delivered by [`OpStream::read`]. `from_user_id` is the
/// broker's attribution string for the author—duplicated on the RPC so
/// non-Rust clients can route by author without decoding the envelope.
#[derive(Debug, Clone)]
pub struct OpEvent {
    pub op: Vec<u8>,
    pub from_user_id: String,
}

/// Client-pulled op stream. Each `.read()` is a single RPC.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct OpStream {
    inner: Rc<StreamClient>,
}

#[cfg(feature = "client")]
impl OpStream {
    /// Pull the next op along with the broker's attribution string
    /// for its author. Resolves when the broker has one available.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a `from_user_id` that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<OpEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let op = r.get_op()?.to_vec();
        let from_user_id = r
            .get_from_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 from_user_id: {e}")))?
            .to_owned();
        if !matches!(r.get_kind(), Ok(collab_capnp::OpKind::Signed)) {
            return Err(NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "collaboration stream carried a retired unsigned frame",
            )));
        }
        Ok(OpEvent { op, from_user_id })
    }
}

/// One piece of content off a [`BlobStream`].
#[derive(Debug, Clone)]
pub struct BlobEvent {
    /// blake3 hex the content is addressed by, in every store on every
    /// machine.
    pub hash: String,
    pub bytes: Vec<u8>,
}

/// Client-pulled stream of arrived document content. Each `.read()` is a
/// single RPC, like every other stream in this schema.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct BlobStream {
    inner: Rc<BlobStreamClient>,
}

#[cfg(feature = "client")]
impl BlobStream {
    /// Pull the next piece of arrived content. The caller MUST verify the
    /// bytes against `hash` before storing them — the key is the content's
    /// identity, so bytes hashing to something else aren't what was asked for.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a `hash` that isn't
    /// valid UTF-8.
    pub async fn read(&self) -> Result<BlobEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let hash = r
            .get_hash()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 blob hash: {e}")))?
            .to_owned();
        let bytes = r.get_bytes()?.to_vec();
        Ok(BlobEvent { hash, bytes })
    }
}

/// One action off an [`ActionStream`].
#[derive(Debug, Clone)]
pub struct ActionEvent {
    /// The registry type id the action was encoded under. Names the app
    /// that owns the document it belongs to.
    pub type_id: String,
    /// The encoded action, exactly as `tilo_actions::EncodedAction`
    /// carries it.
    pub payload: Vec<u8>,
    /// The broker's attribution for whoever sent the frame. Who *sent*
    /// it, which is a different claim from who the action says authored
    /// it.
    pub from_user_id: String,
}

/// Client-pulled stream of actions published into this room.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct ActionStream {
    inner: Rc<ActionStreamClient>,
}

#[cfg(feature = "client")]
impl ActionStream {
    /// Pull the next action published into this room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<ActionEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let type_id = r
            .get_type_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 action type id: {e}")))?
            .to_owned();
        let payload = r.get_payload()?.to_vec();
        let from_user_id = r
            .get_from_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 attribution: {e}")))?
            .to_owned();
        Ok(ActionEvent {
            type_id,
            payload,
            from_user_id,
        })
    }
}

/// What one exported action's invocation must carry, as it crosses the
/// wire — the transport-side twin of `tilo_actions::door::InputSpec`.
/// Kept here rather than depending on `tilo-actions` so this crate stays
/// a client for one socket; an app wanting the richer type converts at
/// its own edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        /// Whether that text names content already in this machine's
        /// store rather than words somebody says.
        wants_content: bool,
    },
}

/// Explicit resource-view metadata attached to the action that opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedView {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub open_action: String,
    pub primary: bool,
    pub mount: Option<ExportedViewMount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedViewMount {
    pub action: String,
    pub capability_param: String,
    pub view_param: String,
}

/// One operation an application exports, as inert data.
///
/// Everything a caller needs to decide whether to perform it, and
/// nothing that could perform it — read-only is the shape of this
/// struct, not a promise about how it is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    /// `document`, `effect`, or `view`.
    pub operation_kind: String,
    /// JSON encoding of the canonical typed parameter descriptors.
    pub params_json: String,
    /// JSON encoding of the complete safety/lifecycle/result contract.
    pub contract_json: String,
    pub view: Option<ExportedView>,
    pub input: ExportedInput,
}

/// Bounded, revision-aware request to an application-owned action directory.
///
/// `cursor` is opaque to callers. A source must reject it as stale when its
/// revision no longer names the same ordering, rather than silently skipping
/// or duplicating rows. Every numeric field is an upper bound, never a hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDirectoryQuery {
    /// Broker-issued live connection id. Zero requests the broker's root
    /// directory; non-zero addresses one application directory.
    pub peer_id: u64,
    pub revision: String,
    pub cursor: Vec<u8>,
    pub needle: String,
    pub parent_resource: String,
    pub max_results: u32,
    pub max_bytes: u32,
    pub max_work: u32,
    pub max_depth: u16,
}

/// A hierarchy-preserving directory row. Resource rows have no action or
/// descriptor; action rows contain the canonical descriptor for that exact
/// resource/action address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDirectoryRow {
    pub parent_resource: String,
    pub resource: String,
    /// Stable kind and user-facing label for resource/container rows. Action
    /// rows repeat their descriptor values here so a shell can render either
    /// shape without parsing raw resource identifiers.
    pub resource_kind: String,
    pub resource_label: String,
    pub resource_presentation_json: String,
    pub action: Option<String>,
    pub descriptor: Option<ExportedAction>,
    pub has_children: bool,
    /// Permission-bearing action used only to disclose a container row.
    pub visibility_action: Option<String>,
    /// Contract paired with [`Self::visibility_action`].
    pub visibility_contract_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDirectoryPage {
    pub revision: String,
    pub rows: Vec<ActionDirectoryRow>,
    pub next_cursor: Vec<u8>,
    pub exhausted: bool,
    pub stale_cursor: bool,
    pub budget_exhausted: bool,
}

/// Exact, current-authority response before an invocation is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDirectoryResolution {
    Found(Box<ActionDirectoryRow>),
    NotFound,
    StaleRevision,
    Unavailable,
}

#[cfg(feature = "client")]
fn capnp_text(value: capnp::text::Reader<'_>) -> Result<String, capnp::Error> {
    value.to_str().map(str::to_owned).map_err(|error| {
        capnp::Error::failed(format!("invalid UTF-8 in action directory: {error}"))
    })
}

#[cfg(feature = "client")]
fn decode_exported_action(
    row: collab_capnp::exported_action::Reader<'_>,
) -> Result<ExportedAction, capnp::Error> {
    let description = capnp_text(row.get_description()?)?;
    let input = match row.get_input()?.which()? {
        collab_capnp::action_input::Which::None(()) => ExportedInput::None,
        collab_capnp::action_input::Which::Text(text) => ExportedInput::Text {
            prompt: capnp_text(text.get_prompt()?)?,
            placeholder: capnp_text(text.get_placeholder()?)?,
            wants_content: text.get_wants_content(),
        },
    };
    let view = row
        .has_view()
        .then(|| row.get_view())
        .transpose()?
        .map(|view| {
            let description = capnp_text(view.get_description()?)?;
            Ok::<_, capnp::Error>(ExportedView {
                id: capnp_text(view.get_id()?)?,
                label: capnp_text(view.get_label()?)?,
                description: (!description.is_empty()).then_some(description),
                open_action: capnp_text(view.get_open_action()?)?,
                primary: view.get_primary(),
                mount: view
                    .has_mount()
                    .then(|| view.get_mount())
                    .transpose()?
                    .map(|mount| {
                        Ok::<_, capnp::Error>(ExportedViewMount {
                            action: capnp_text(mount.get_action()?)?,
                            capability_param: capnp_text(mount.get_capability_param()?)?,
                            view_param: capnp_text(mount.get_view_param()?)?,
                        })
                    })
                    .transpose()?,
            })
        })
        .transpose()?;
    Ok(ExportedAction {
        resource: capnp_text(row.get_resource()?)?,
        action: capnp_text(row.get_action()?)?,
        resource_kind: capnp_text(row.get_resource_kind()?)?,
        resource_label: capnp_text(row.get_resource_label()?)?,
        action_label: capnp_text(row.get_action_label()?)?,
        description: (!description.is_empty()).then_some(description),
        operation_kind: capnp_text(row.get_operation_kind()?)?,
        params_json: capnp_text(row.get_params_json()?)?,
        contract_json: capnp_text(row.get_contract_json()?)?,
        view,
        input,
    })
}

#[cfg(feature = "client")]
fn decode_action_directory_row(
    row: collab_capnp::action_directory_row::Reader<'_>,
) -> Result<ActionDirectoryRow, capnp::Error> {
    let action = capnp_text(row.get_action()?)?;
    let visibility_action = capnp_text(row.get_visibility_action()?)?;
    let descriptor = row
        .has_descriptor()
        .then(|| row.get_descriptor())
        .transpose()?
        .map(decode_exported_action)
        .transpose()?;
    Ok(ActionDirectoryRow {
        parent_resource: capnp_text(row.get_parent_resource()?)?,
        resource: capnp_text(row.get_resource()?)?,
        resource_kind: capnp_text(row.get_resource_kind()?)?,
        resource_label: capnp_text(row.get_resource_label()?)?,
        resource_presentation_json: capnp_text(row.get_resource_presentation_json()?)?,
        action: (!action.is_empty()).then_some(action),
        descriptor,
        has_children: row.get_has_children(),
        visibility_action: (!visibility_action.is_empty()).then_some(visibility_action),
        visibility_contract_json: capnp_text(row.get_visibility_contract_json()?)?,
    })
}

/// Decode a broker-validated directory page for an in-process shell client.
/// Public so the shell can use the same typed bridge without reserialising a
/// Cap'n Proto page into an eager JSON catalogue.
/// Decode one bounded action-directory page from its Cap'n Proto wire form.
///
/// # Errors
///
/// Returns a Cap'n Proto error when a field is absent, malformed, or cannot
/// be decoded into the typed resource/action representation.
#[cfg(feature = "client")]
pub fn decode_action_directory_page(
    page: collab_capnp::action_directory_page::Reader<'_>,
) -> Result<ActionDirectoryPage, capnp::Error> {
    let rows = page
        .get_rows()?
        .iter()
        .map(decode_action_directory_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ActionDirectoryPage {
        revision: capnp_text(page.get_revision()?)?,
        rows,
        next_cursor: page.get_next_cursor()?.to_vec(),
        exhausted: page.get_exhausted(),
        stale_cursor: page.get_stale_cursor(),
        budget_exhausted: page.get_budget_exhausted(),
    })
}

/// Decode a broker-validated exact resolution for an in-process shell client.
/// Decode an exact action-directory resolution from its Cap'n Proto wire form.
///
/// # Errors
///
/// Returns a Cap'n Proto error when the union or a contained action row is
/// malformed.
#[cfg(feature = "client")]
pub fn decode_action_directory_resolution(
    resolution: collab_capnp::action_directory_resolution::Reader<'_>,
) -> Result<ActionDirectoryResolution, capnp::Error> {
    match resolution.which()? {
        collab_capnp::action_directory_resolution::Which::Found(row) => Ok(
            ActionDirectoryResolution::Found(Box::new(decode_action_directory_row(row?)?)),
        ),
        collab_capnp::action_directory_resolution::Which::NotFound(()) => {
            Ok(ActionDirectoryResolution::NotFound)
        }
        collab_capnp::action_directory_resolution::Which::StaleRevision(()) => {
            Ok(ActionDirectoryResolution::StaleRevision)
        }
        collab_capnp::action_directory_resolution::Which::Unavailable(()) => {
            Ok(ActionDirectoryResolution::Unavailable)
        }
    }
}

/// One invocation off an [`InvocationStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Echoed back on [`ChannelHandle::complete_invocation`], so the
    /// session can match an answer to the question it asked.
    pub id: u64,
    pub resource: String,
    pub action: String,
    /// `None` when the caller supplied none. Separate from the empty
    /// string so an action that legitimately takes `""` can still say it
    /// was given one.
    pub argument: Option<String>,
    /// Who asked, as the session resolved them. An attribution the
    /// resulting change commits under — not an authority; the session
    /// already decided this may happen.
    pub actor: String,
    /// Serialized broker-owned [`tilo_actions::ActionContext`] when this
    /// invocation originated from a leased workspace task. Empty for ordinary
    /// invocations so legacy application callers remain context-neutral.
    pub context_json: String,
}

/// Client-pulled stream of invocations addressed to this application.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct InvocationStream {
    inner: Rc<InvocationStreamClient>,
}

/// Client-pulled active-session lifecycle stream.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct RealtimeStream {
    inner: Rc<RealtimeStreamClient>,
}

#[cfg(feature = "client")]
impl RealtimeStream {
    /// Pull one bounded, versioned event payload.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker disconnects.
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        Ok(reply.get()?.get_event_json()?.to_vec())
    }
}

#[cfg(feature = "client")]
impl InvocationStream {
    /// Pull the next invocation addressed to this application.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<Invocation, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let text = |t: capnp::text::Reader<'_>, what: &str| {
            t.to_str()
                .map(std::borrow::ToOwned::to_owned)
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 {what}: {e}")))
        };
        let resource = text(r.get_resource()?, "invocation resource")?;
        let action = text(r.get_action()?, "invocation action")?;
        let actor = text(r.get_actor()?, "invocation actor")?;
        let context_json = text(r.get_context_json()?, "invocation context")?;
        let argument = if r.get_has_argument() {
            Some(text(r.get_argument()?, "invocation argument")?)
        } else {
            None
        };
        Ok(Invocation {
            id: r.get_id(),
            resource,
            action,
            argument,
            actor,
            context_json,
        })
    }
}

/// Broker-minted work for one admitted, currently connected agent.  `lease`
/// is an opaque capability: an agent must echo it for progress and for any
/// action invocation attributed to this task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunEnvelope {
    pub version: u16,
    pub task_id: String,
    pub title: String,
    pub run_id: u64,
    pub lease: String,
    pub planning: TaskRunPlanning,
    pub action_context: serde_json::Value,
}

/// How the task owner allowed its delegated agent to choose actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunPlanning {
    RulesOnly,
    Model,
}

/// Agent lifecycle information the broker may project into the shared Task
/// Center audit.  There is no free-form status mutation: terminal direction is
/// explicit and all messages are bounded by the broker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRunUpdate {
    Accepted,
    Progress { message: String },
    Succeeded { message: String },
    Failed { message: String },
    Cancelled { message: String },
}

/// Client-pulled stream of broker-addressed task leases.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct TaskRunStream {
    inner: Rc<TaskRunStreamClient>,
}

#[cfg(feature = "client")]
impl TaskRunStream {
    /// Pull the next task run, rejecting malformed broker data rather than
    /// presenting an agent with an ambiguous authority envelope.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the stream fails or the received envelope
    /// is malformed.
    pub async fn read(&self) -> Result<TaskRunEnvelope, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        let json = reply
            .get()?
            .get_task_json()?
            .to_str()
            .map_err(|error| capnp::Error::failed(format!("invalid task envelope: {error}")))?;
        serde_json::from_str(json).map_err(|error| {
            capnp::Error::failed(format!("invalid task envelope JSON: {error}")).into()
        })
    }
}

#[cfg(test)]
mod task_run_tests {
    use super::{TaskRunEnvelope, TaskRunPlanning, TaskRunUpdate};

    #[test]
    fn task_run_wire_is_typed_and_rejects_unknown_fields() {
        let envelope = TaskRunEnvelope {
            version: 1,
            task_id: "task:1".into(),
            title: "Review proposal".into(),
            run_id: 7,
            lease: "broker-lease".into(),
            planning: TaskRunPlanning::RulesOnly,
            action_context: serde_json::json!({"purpose": "Review proposal"}),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            serde_json::from_str::<TaskRunEnvelope>(&json).unwrap(),
            envelope
        );
        assert!(serde_json::from_str::<TaskRunEnvelope>(
            r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"rules_only","action_context":{},"path":"/tmp"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<TaskRunEnvelope>(
            r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"unattended","action_context":{}}"#
        )
        .is_err());
        assert_eq!(
            serde_json::from_str::<TaskRunUpdate>(r#"{"kind":"progress","message":"checking"}"#)
                .unwrap(),
            TaskRunUpdate::Progress {
                message: "checking".into()
            }
        );
    }
}

/// Per-channel presence handle.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct PresenceHandle {
    inner: Rc<collab_capnp::presence_handle::Client>,
}

#[cfg(feature = "client")]
impl PresenceHandle {
    /// Publish a cursor position. The broker fan-outs to peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_cursor(&self, x: f32, y: f32) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_cursor_request();
        {
            let mut b = req.get();
            b.set_x(x);
            b.set_y(y);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish the local pointer at `(x, y)` in the coordinate space
    /// `key_bytes` names — [`Self::publish_cursor`] with the space kept.
    /// `key_bytes` is a postcard-encoded `ui::PresenceKey`
    /// (`PresenceKey::Pointer` for a pointer); the broker relays those bytes
    /// untouched, so the receiver reads the space its publisher chose.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_pointer(
        &self,
        key_bytes: &[u8],
        x: f32,
        y: f32,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_pointer_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_x(x);
            b.set_y(y);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish the local caret position on the artifact identified
    /// by `key_bytes`. `key_bytes` is a postcard-encoded
    /// `ui::PresenceKey`; the broker treats it as an opaque routing
    /// tag.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_caret(&self, key_bytes: &[u8], pos: u64) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_caret_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_pos(pos);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish a selection range on the keyed artifact. `start ==
    /// end` MAY be used to represent a caret; either publish path
    /// is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_selection(
        &self,
        key_bytes: &[u8],
        start: u64,
        end: u64,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_selection_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_start(start);
            b.set_end(end);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish a transient range the local participant just inserted.
    /// Unlike a selection, receivers render this as a fading event.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_typing(
        &self,
        key_bytes: &[u8],
        start: u64,
        end: u64,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_typing_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_start(start);
            b.set_end(end);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish an invocation-scoped semantic interaction. The broker derives
    /// the actor from this authenticated handle; the JSON carries only the
    /// versioned action/target lifecycle from `tilo_actions`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_interaction(
        &self,
        key_bytes: &[u8],
        interaction_json: &str,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_interaction_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_interaction_json(interaction_json);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish or clear (`text == ""`) the local participant's short status.
    /// The broker authenticates authorship; `automatic` is presentation
    /// provenance, not authority.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker rejects the request or its
    /// connection fails during the call.
    pub async fn publish_status(
        &self,
        key_bytes: &[u8],
        text: &str,
        automatic: bool,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_status_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_text(text);
            b.set_automatic(automatic);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish "the local user has arrived at the keyed object".
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_arrived_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// Publish "the local user has left the keyed object".
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_left_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// Subscribe to every kind of remote presence update in one
    /// stream. Each event carries the full [`PresencePayload`]
    /// union; clients discriminate on payload variant.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable> {
        let req = self.inner.subscribe_all_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(PresenceStream {
            inner: Rc::new(stream),
        })
    }
}

/// Rust mirror of the capnp `PresencePayload` union, kept as a typed
/// enum so app code doesn't touch the generated Reader/Builder types.
#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    /// A pointer published without a coordinate space — the legacy
    /// `publish_cursor` shape. `key_bytes` says nothing about where this
    /// position was measured; a reader must not invent a space for it.
    Cursor {
        x: f32,
        y: f32,
    },
    /// A pointer published *with* its space, by
    /// [`PresenceHandle::publish_pointer`]. The event's `key_bytes`
    /// decode to the `ui::PresenceKey` the publisher chose and are
    /// authoritative.
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: u64,
    },
    Selection {
        start: u64,
        end: u64,
    },
    Typing {
        start: u64,
        end: u64,
    },
    /// The authenticated sender is visually following `target`; `None`
    /// releases that transient relationship. This is presence, never remote
    /// viewport control.
    Following {
        target: Option<String>,
    },
    /// Versioned `tilo_actions::InteractionUpdate` JSON. Actor identity is the
    /// enclosing broker-attributed [`PresenceEvent::user_id`], never this
    /// caller-controlled payload.
    InteractionJson(String),
    Status {
        text: String,
        automatic: bool,
    },
    Arrived,
    Left,
}

/// A single remote presence update: `user_id` is the broker's
/// attribution, `key_bytes` is the postcard of a `ui::PresenceKey`
/// (opaque to the SDK), `payload` discriminates the variant.
#[derive(Debug, Clone)]
pub struct PresenceEvent {
    pub user_id: String,
    pub key_bytes: Vec<u8>,
    pub payload: PresencePayload,
}

/// Client-pulled unified presence stream.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct PresenceStream {
    inner: Rc<PresenceStreamClient>,
}

#[cfg(feature = "client")]
impl PresenceStream {
    /// Pull the next presence event across all payload kinds.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, the reply carries a `user_id` that isn't
    /// valid UTF-8, or the payload union's discriminant is unrecognised.
    pub async fn read(&self) -> Result<PresenceEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let evt = r.get_event()?;
        let user_id = evt
            .get_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
            .to_owned();
        let key_bytes = evt.get_key_bytes()?.to_vec();
        let payload = decode_presence_payload(evt.get_payload()?)?;
        Ok(PresenceEvent {
            user_id,
            key_bytes,
            payload,
        })
    }
}

#[cfg(feature = "client")]
fn decode_presence_payload(
    reader: collab_capnp::presence_payload::Reader<'_>,
) -> Result<PresencePayload, capnp::Error> {
    use collab_capnp::presence_payload::Which as W;
    Ok(match reader.which()? {
        W::Cursor(r) => PresencePayload::Cursor {
            x: r.get_x(),
            y: r.get_y(),
        },
        W::Pointer(r) => PresencePayload::Pointer {
            x: r.get_x(),
            y: r.get_y(),
        },
        W::Caret(r) => PresencePayload::Caret { pos: r.get_pos() },
        W::Selection(r) => PresencePayload::Selection {
            start: r.get_start(),
            end: r.get_end(),
        },
        W::Typing(r) => PresencePayload::Typing {
            start: r.get_start(),
            end: r.get_end(),
        },
        W::Following(target) => PresencePayload::Following {
            target: target
                .ok()
                .and_then(|target| target.to_str().ok())
                .filter(|target| !target.is_empty())
                .map(str::to_owned),
        },
        W::InteractionJson(value) => PresencePayload::InteractionJson(
            value?
                .to_str()
                .map_err(|error| {
                    capnp::Error::failed(format!("invalid interaction utf8: {error}"))
                })?
                .to_owned(),
        ),
        W::Status(value) => PresencePayload::Status {
            text: value
                .get_text()?
                .to_str()
                .map_err(|error| capnp::Error::failed(format!("invalid status utf8: {error}")))?
                .to_owned(),
            automatic: value.get_automatic(),
        },
        W::Arrived(()) => PresencePayload::Arrived,
        W::Left(()) => PresencePayload::Left,
    })
}

/// A single collaborator in a [`ChannelContext`].
#[derive(Debug, Clone)]
pub struct Collaborator {
    pub user_id: String,
    pub display_name: String,
    /// Packed as `0x00RRGGBB`; the top byte is reserved and MUST be
    /// zero on the wire. Consumers unpack into their native colour
    /// space.
    pub color: u32,
    /// Whether this member is an agent rather than a person, as the room
    /// recorded when it admitted them. Carried so no consumer has to
    /// read the id and guess.
    pub is_agent: bool,
    /// Whether this row is this machine's own participant. `user_id` is the
    /// address other machines use for a participant, and a machine never
    /// announces its own participant to its own applications — without
    /// this, an app had no way to find itself in the room's own roster.
    pub is_self: bool,
}

/// Roster + local permissions snapshot delivered on
/// [`ContextStream`]. See the capnp docs for the exact semantics.
#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub collaborators: Vec<Collaborator>,
    pub permissions: Vec<String>,
    /// `Some` only for a session-local Follow control frame. The inner
    /// `None` means stop following; ordinary context snapshots keep this
    /// field at `None` and replace roster/permissions as before.
    pub follow_changed: Option<Option<String>>,
    /// True only on a directed broker request to publish current document
    /// state. It is not a roster update and must not replace local context.
    pub snapshot_requested: bool,
}

/// Client-pulled channel-context stream.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct ContextStream {
    inner: Rc<ContextStreamClient>,
}

#[cfg(feature = "client")]
impl ContextStream {
    /// Pull the next roster / permission snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<ChannelContext, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let ctx = r.get_ctx()?;
        let collabs_reader = ctx.get_collaborators()?;
        let mut collaborators = Vec::with_capacity(collabs_reader.len() as usize);
        for i in 0..collabs_reader.len() {
            let c = collabs_reader.get(i);
            let user_id = c
                .get_user_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
                .to_owned();
            let display_name = c
                .get_display_name()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 display_name: {e}")))?
                .to_owned();
            collaborators.push(Collaborator {
                user_id,
                display_name,
                color: c.get_color(),
                is_agent: c.get_is_agent(),
                is_self: c.get_is_self(),
            });
        }
        let perms_reader = ctx.get_permissions()?;
        let mut permissions = Vec::with_capacity(perms_reader.len() as usize);
        for i in 0..perms_reader.len() {
            permissions.push(
                perms_reader
                    .get(i)?
                    .to_str()
                    .map_err(|e| capnp::Error::failed(format!("invalid utf8 permission: {e}")))?
                    .to_owned(),
            );
        }
        let follow_changed = ctx.get_follow_changed().then(|| {
            ctx.get_follow_target()
                .ok()
                .and_then(|target| target.to_str().ok())
                .filter(|target| !target.is_empty())
                .map(str::to_owned)
        });
        Ok(ChannelContext {
            collaborators,
            permissions,
            follow_changed,
            snapshot_requested: ctx.get_snapshot_requested(),
        })
    }
}

/// A page of previously-validated signed ops returned by
/// [`ChannelHandle::fetch_scrollback_for`].
#[derive(Debug, Clone)]
pub struct ScrollbackBatch {
    /// Postcard-encoded `SignedOp` blobs in ascending `seq` order.
    pub ops: Vec<Vec<u8>>,
    /// Lowest `seq` in the page (inclusive). Zero when `ops` is
    /// empty.
    pub earliest_seq: u64,
    /// Highest `seq` in the page (inclusive). Zero when `ops` is
    /// empty.
    pub latest_seq: u64,
    /// True iff the broker has no further in-memory history in the
    /// requested range.
    pub done: bool,
}

/// Re-export the raw generated module for power users that need to
/// reach below the idiomatic wrappers (server impls, tests).
#[cfg(feature = "client")]
pub mod raw {
    pub use super::collab_capnp::*;
}

#[cfg(test)]
mod certified_join_tests {
    use super::*;

    #[test]
    fn certified_join_round_trips_and_prefixed_garbage_never_downgrades() {
        let identity = CertifiedJoinIdentity {
            workspace_id: "workspace-a".into(),
            device_certificate: vec![1, 2, 3],
            proof: vec![4; 64],
        };
        let encoded = identity.encode().expect("encode");
        assert_eq!(
            CertifiedJoinIdentity::decode(&encoded).expect("decode"),
            Some(identity)
        );
        assert_eq!(
            CertifiedJoinIdentity::decode(b"uncertified identity").expect("uncertified bytes"),
            None
        );
        let mut malformed = CERTIFIED_JOIN_PREFIX.to_vec();
        malformed.extend_from_slice(b"not postcard");
        assert!(CertifiedJoinIdentity::decode(&malformed).is_err());
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    use std::env;

    fn clear_tilo_env() -> tilo_test_env::ScopedEnv {
        let mut scoped = tilo_test_env::ScopedEnv::new();
        for (key, _) in env::vars() {
            if key.starts_with("TILO_") {
                scoped.remove(&key);
            }
        }
        scoped
    }

    #[test]
    fn try_connect_without_env_returns_service_fd_missing() {
        let _env = clear_tilo_env();
        // Not using `expect_err` because `CollabClient` intentionally
        // doesn't implement `Debug` (the underlying capnp client hook
        // isn't `Debug`).
        match try_connect() {
            Err(NotAvailable::ServiceFdMissing) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected ServiceFdMissing"),
        }
    }

    #[test]
    fn nearby_transfer_contract_is_bounded_and_path_bytes_are_opaque() {
        let command = NearbyTransferCommand::Send {
            target: NearbyTargetHandle::from_issued_bytes([7; 16]),
            source_path: vec![b'/', b't', b'm', b'p', b'/', 0xff],
        };
        assert!(command.valid());
        let encoded = serde_json::to_vec(&command).expect("encode nearby command");
        assert!(encoded.len() < MAX_NEARBY_TRANSFER_WIRE_BYTES);
        assert_eq!(
            serde_json::from_slice::<NearbyTransferCommand>(&encoded).expect("decode command"),
            command
        );

        assert!(
            !NearbyTransferCommand::Send {
                target: NearbyTargetHandle::from_issued_bytes([7; 16]),
                source_path: vec![b'a', 0, b'b'],
            }
            .valid()
        );
        assert!(
            NearbyTransferCommand::Decide {
                handle: NearbyTransferHandle::from_issued_bytes([8; 16]),
                accept: true,
                destination: Some(NearbyDestinationHandle::from_issued_bytes([9; 16])),
                remember_for_session: true,
            }
            .valid()
        );
        assert!(
            !NearbyTransferCommand::Decide {
                handle: NearbyTransferHandle::from_issued_bytes([8; 16]),
                accept: true,
                destination: None,
                remember_for_session: false,
            }
            .valid()
        );
        assert!(
            !NearbyTransferCommand::Send {
                target: NearbyTargetHandle::from_issued_bytes([7; 16]),
                source_path: vec![b'a'; MAX_NEARBY_TRANSFER_PATH_BYTES + 1],
            }
            .valid()
        );
    }

    #[test]
    fn nearby_transfer_snapshot_rejects_unbounded_or_impossible_progress() {
        let target = NearbyTransferTarget {
            handle: NearbyTargetHandle::from_issued_bytes([3; 16]),
            label: "Travel laptop".into(),
            online: true,
        };
        let transfer = NearbyTransferStatus {
            handle: NearbyTransferHandle::from_issued_bytes([4; 16]),
            name: "Photos".into(),
            device_label: target.label.clone(),
            direction: NearbyTransferDirection::Sending,
            phase: NearbyTransferPhase::Transferring,
            transferred_bytes: 20,
            total_bytes: 100,
        };
        let reply = NearbyTransferReply::Snapshot {
            snapshot: NearbyTransferSnapshot {
                targets: vec![target],
                destinations: vec![NearbyTransferDestination {
                    handle: NearbyDestinationHandle::from_issued_bytes([5; 16]),
                    label: "Workspace files".into(),
                    remembered_for_session: false,
                }],
                transfers: vec![transfer.clone()],
            },
        };
        assert!(reply.valid());

        let mut impossible = transfer;
        impossible.transferred_bytes = 101;
        assert!(
            !NearbyTransferReply::Snapshot {
                snapshot: NearbyTransferSnapshot {
                    targets: Vec::new(),
                    destinations: Vec::new(),
                    transfers: vec![impossible],
                },
            }
            .valid()
        );
    }

    #[test]
    fn wifi_continuity_contract_is_bounded_and_redacts_secrets() {
        let command = WifiContinuityCommand::Share {
            ssid: "Studio".into(),
            security: PortableWifiSecurity::Wpa3,
            password: "correct horse battery staple".into(),
        };
        assert!(command.valid());
        assert!(!format!("{command:?}").contains("correct horse"));
        let encoded = serde_json::to_vec(&command).expect("encode Wi-Fi command");
        assert!(encoded.len() < MAX_WIFI_CONTINUITY_WIRE_BYTES);
        assert_eq!(
            serde_json::from_slice::<WifiContinuityCommand>(&encoded)
                .expect("decode Wi-Fi command"),
            command
        );

        assert!(
            !WifiContinuityCommand::Share {
                ssid: "Studio".into(),
                security: PortableWifiSecurity::Wpa2,
                password: "x".repeat(MAX_WIFI_PASSWORD_BYTES + 1),
            }
            .valid()
        );
        assert!(
            !WifiContinuityReply::Password {
                password: "x".repeat(MAX_WIFI_PASSWORD_BYTES + 1),
            }
            .valid()
        );
    }

    #[test]
    fn saved_wifi_profile_reference_requires_an_exact_local_uuid() {
        assert!(
            WifiContinuityCommand::ShareSavedProfile {
                profile_uuid: "12345678-1234-1234-1234-123456789abc".into(),
            }
            .valid()
        );
        for invalid in [
            "profile-name",
            "../../secret",
            "12345678-1234-1234-1234-123456789abz",
        ] {
            assert!(
                !WifiContinuityCommand::ShareSavedProfile {
                    profile_uuid: invalid.into(),
                }
                .valid()
            );
        }
    }

    #[test]
    fn action_directory_resource_presentation_round_trips() {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut page = message.init_root::<collab_capnp::action_directory_page::Builder<'_>>();
            page.set_revision("r1");
            page.set_exhausted(true);
            let mut rows = page.init_rows(1);
            let mut row = rows.reborrow().get(0);
            row.set_resource("totp:mail");
            row.set_resource_kind("totp");
            row.set_resource_label("Mail code");
            row.set_resource_presentation_json(r#"{"detail":"expires in 18 s","badge":"438 291"}"#);
        }
        let reader = message
            .get_root_as_reader::<collab_capnp::action_directory_page::Reader<'_>>()
            .expect("page reader");
        let decoded = decode_action_directory_page(reader).expect("page decodes");
        assert_eq!(
            decoded.rows[0].resource_presentation_json,
            r#"{"detail":"expires in 18 s","badge":"438 291"}"#
        );
    }

    #[test]
    fn exported_view_metadata_round_trips() {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut action = message.init_root::<collab_capnp::exported_action::Builder<'_>>();
            action.set_resource("notes:brief");
            action.set_action("show-editor");
            let mut view = action.init_view();
            view.set_id("editor");
            view.set_label("Editor");
            view.set_description("Edit this note");
            view.set_open_action("show-editor");
            view.set_primary(true);
            let mut mount = view.init_mount();
            mount.set_action("mount-editor");
            mount.set_capability_param("slot_capability");
            mount.set_view_param("slot_view");
        }
        let reader = message
            .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
            .expect("action reader");
        let decoded = decode_exported_action(reader).expect("action decodes");
        assert_eq!(
            decoded.view,
            Some(ExportedView {
                id: "editor".into(),
                label: "Editor".into(),
                description: Some("Edit this note".into()),
                open_action: "show-editor".into(),
                primary: true,
                mount: Some(ExportedViewMount {
                    action: "mount-editor".into(),
                    capability_param: "slot_capability".into(),
                    view_param: "slot_view".into(),
                }),
            })
        );
    }
}
