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
        clippy::string_slice,
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

pub mod private_draft;
pub mod task_control;
pub use task_control::{TaskDirection, TaskRunControl, TaskRunStatus, native_task_revision};
use tilo_sort::CompactSort as _;

#[cfg(feature = "client")]
use std::rc::Rc;

pub mod browser_service_sessions;
pub mod file_clipboard;
pub mod open_selected;
pub use browser_service_sessions::*;
pub mod retained_person;
pub mod retained_retirement;

#[cfg(feature = "client")]
use runtime_portal::NotAvailable;

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

pub mod approved_checks;
pub mod external_moderation;
pub mod write_attribution;
/// The host distinguishes local UI intent from continuation of a published action.
/// A published id is resolved only against broker-retained original custody.
#[derive(Clone, Copy, Debug)]
pub enum DisplayOrigin {
    LocalHost,
    Published([u8; 16]),
}

mod action_directory;
pub use action_directory::*;
mod surface_delta;
pub use surface_delta::*;
pub mod audio_output;
pub mod committed_document;
pub mod event;
#[cfg(feature = "client")]
pub mod live_activity;
pub mod native_media;
pub mod route;
#[cfg(feature = "client")]
pub mod settings;
#[cfg(feature = "client")]
pub use live_activity::{LiveActivities, LiveActivityHandle};
#[cfg(feature = "client")]
pub use settings::{AppSettingsHandle, MAX_SETTINGS_WIRE_BYTES, SettingsView};

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
        /// The unit's place in the target's encoded sequence. A gap means a
        /// unit was lost on the way: the deltas after it cannot be decoded
        /// until a keyframe, which [`WorkspaceControlInput::RequestKeyframe`]
        /// asks for.
        seq: u64,
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
    /// Unit `seq` of the stream could not be decoded (one before it went
    /// missing, or the decoder rejected one): ask the target for a keyframe.
    /// Allowed for a session with a live stream, read-only or not.
    RequestKeyframe {
        seq: u64,
    },
}

#[cfg(feature = "client")]
pub use collab_capnp::action_catalogue_stream::Client as ActionCatalogueStreamClient;
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
    let client: CollabServiceClient = runtime_portal::connect_service("collab")?.bootstrap();
    let client = CollabClient::from_raw(client);
    #[cfg(unix)]
    let client = client.with_descriptors(runtime_portal::service_descriptors("collab")?);
    Ok(client)
}

/// Where a connection's calls hand the broker descriptors, if it can.
#[cfg(all(feature = "client", unix))]
type Descriptors = Option<runtime_portal::descriptor_lane::DescriptorSender>;
#[cfg(all(feature = "client", not(unix)))]
type Descriptors = Option<std::convert::Infallible>;

/// Idiomatic Rust wrapper over the generated [`CollabServiceClient`].
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct CollabClient {
    inner: Rc<CollabServiceClient>,
    descriptors: Descriptors,
}

#[cfg(feature = "client")]
impl CollabClient {
    /// Wrap an existing generated client (mainly useful for tests
    /// that own their own RPC system).
    pub fn from_raw(client: CollabServiceClient) -> Self {
        Self {
            inner: Rc::new(client),
            descriptors: None,
        }
    }

    /// Hand the broker descriptors over `lane`, the descriptor lane of the
    /// very connection `self` calls over; see
    /// [`ChannelHandle::local_file_clipboard_command`].
    #[cfg(unix)]
    #[must_use]
    pub fn with_descriptors(
        mut self,
        lane: runtime_portal::descriptor_lane::DescriptorSender,
    ) -> Self {
        self.descriptors = Some(lane);
        self
    }

    /// [`Self::with_descriptors`] for a caller that only knows the lane
    /// [`runtime_portal::DescriptorStreamSource`] handed it: on a host
    /// without descriptors the lane is empty and the client stays as it was.
    #[must_use]
    pub fn with_optional_descriptors(mut self, lane: runtime_portal::DescriptorLane) -> Self {
        #[cfg(unix)]
        {
            self.descriptors = lane;
        }
        #[cfg(not(unix))]
        {
            let _ = lane;
        }
        self
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
        self.join_channel_as_kind(channel_id, identity, participant_handle, "person", "")
            .await
    }

    /// Join as an explicitly typed participant. The broker validates an
    /// agent request against the workspace; this is not a display-name hint.
    /// `record_kind` names the resource kind of the record the channel
    /// carries, or is empty.
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
        record_kind: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        let mut req = self.inner.join_channel_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            builder.set_identity(identity);
            builder.set_participant_handle(participant_handle);
            builder.set_participant_kind(participant_kind);
            builder.set_record_kind(record_kind);
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(ChannelHandle {
            inner: Rc::new(handle),
            descriptors: self.descriptors.clone(),
        })
    }
}

/// One published application action a leased task asks to run: the app
/// peer that publishes it, its resource and action, and the argument it
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskAction<'a> {
    pub peer_id: u64,
    /// A retained or application provider; empty means the live peer above.
    pub provider: &'a str,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
}

/// How a native task's re-ask of its own child may be held by the broker:
/// a running child until it moves past `seen` or settles, the run is
/// cancelled, or `wait_ms` (capped by the broker) passes; a child that waits
/// for its person until it is decided (answered, or a grant that decides it
/// changed), the run stops or the connection closes, whatever `wait_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskChildWait {
    pub wait_ms: u32,
    pub seen: tilo_actions::remote::UpdateMark,
}

/// A subscribed-to channel. Holds a typed capability backed by the
/// broker; dropping the handle closes the subscription. Cheap to
/// clone (the underlying capability is reference-counted).
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct ChannelHandle {
    inner: Rc<collab_capnp::channel_handle::Client>,
    descriptors: Descriptors,
}

mod speakers;
pub use speakers::*;

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
    /// Local wake configuration, never shared as a credential or fleet state.
    NetworkWake {
        profile_uuid: String,
        enabled: Option<bool>,
    },
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
            Self::NetworkWake {
                profile_uuid,
                enabled,
            } => formatter
                .debug_struct("NetworkWake")
                .field("profile_uuid", profile_uuid)
                .field("enabled", enabled)
                .finish(),
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
    NetworkWake { status: NetworkWakeStatus },
    Password { password: String },
    NotFound,
    Shared { ssid: String },
    Rejected { reason: String },
}

impl std::fmt::Debug for WifiContinuityReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkWake { status } => formatter
                .debug_struct("NetworkWake")
                .field("status", status)
                .finish(),
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
            Self::NetworkWake { profile_uuid, .. } | Self::ShareSavedProfile { profile_uuid } => {
                valid_uuid(profile_uuid)
            }
        }
    }
}

impl WifiContinuityReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::NetworkWake { status } => status.valid(),
            Self::Password { password } => {
                !password.is_empty() && password.len() <= MAX_WIFI_PASSWORD_BYTES
            }
            Self::NotFound => true,
            Self::Shared { ssid } => !ssid.is_empty() && ssid.len() <= MAX_WIFI_SSID_BYTES,
            Self::Rejected { reason } => !reason.is_empty() && reason.len() <= 512,
        }
    }
}

/// Bounded local observations; no field claims verified physical recovery.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkWakeStatus {
    pub profile: String,
    pub interface: Option<String>,
    pub configured: bool,
    pub supported: Option<bool>,
    pub applied: Option<bool>,
    pub problem: Option<String>,
}

impl NetworkWakeStatus {
    fn valid(&self) -> bool {
        valid_uuid(&self.profile)
            && self.interface.as_ref().is_none_or(|name| {
                !name.is_empty()
                    && name.len() <= 15
                    && !name.starts_with('-')
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            && self
                .problem
                .as_ref()
                .is_none_or(|problem| !problem.is_empty() && problem.len() <= 512)
    }
}

/// A hyphenated UUID, the only 36-character form `uuid` parses.
pub(crate) fn valid_uuid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::try_parse(value).is_ok()
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
    /// Local JSON broker extension, not a personal-fabric peer protocol.
    AirDropAvailability,
    /// A manually selected lab receiver. Certificate trust is collected only
    /// by the compositor; applications cannot submit or reuse a trusted pin.
    SendAirDrop {
        source_path: Vec<u8>,
        address: String,
        port: u16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NearbyTransferReply {
    Snapshot { snapshot: NearbyTransferSnapshot },
    Queued { handle: NearbyTransferHandle },
    Accepted,
    Rejected { reason: String },
    AirDropAvailability { available: bool },
}

impl NearbyTransferCommand {
    /// Structural validation shared by clients and the shell broker. This is
    /// independent of authorization: the broker must still bind
    /// the app, source path, transfer id and device to its live route.
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot | Self::AirDropAvailability => true,
            Self::SendAirDrop {
                source_path,
                address,
                port,
            } => {
                !source_path.is_empty()
                    && source_path.len() <= MAX_NEARBY_TRANSFER_PATH_BYTES
                    && !source_path.contains(&0)
                    && *port != 0
                    && address.len() <= 64
                    && address
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| !ip.is_unspecified() && !ip.is_multicast())
            }
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
            Self::Accepted | Self::AirDropAvailability { .. } => true,
            Self::Rejected { reason } => {
                !reason.is_empty() && reason.len() <= MAX_NEARBY_TRANSFER_LABEL_BYTES
            }
        }
    }
}

#[cfg(feature = "client")]
fn wire_failure(message: String) -> NotAvailable {
    NotAvailable::Capnp(capnp::Error::failed(message))
}

/// Encode one `what` control command, refusing it when it is invalid or
/// larger than `max_bytes`.
#[cfg(feature = "client")]
fn encode_bounded_command<C: serde::Serialize>(
    what: &str,
    command: &C,
    valid: bool,
    max_bytes: usize,
) -> Result<Vec<u8>, NotAvailable> {
    if !valid {
        return Err(wire_failure(format!("invalid {what} command")));
    }
    let command_json = serde_json::to_vec(command)
        .map_err(|error| wire_failure(format!("encode {what} command: {error}")))?;
    if command_json.len() > max_bytes {
        return Err(wire_failure(format!("{what} command exceeds wire bound")));
    }
    Ok(command_json)
}

/// Decode one `what` control reply, refusing it when it is larger than
/// `max_bytes` or fails its own validity check.
#[cfg(feature = "client")]
fn decode_bounded_reply<R: serde::de::DeserializeOwned>(
    what: &str,
    bytes: &[u8],
    max_bytes: usize,
    valid: impl FnOnce(&R) -> bool,
) -> Result<R, NotAvailable> {
    if bytes.len() > max_bytes {
        return Err(wire_failure(format!("{what} reply exceeds wire bound")));
    }
    let decoded: R = serde_json::from_slice(bytes)
        .map_err(|error| wire_failure(format!("decode {what} reply: {error}")))?;
    if !valid(&decoded) {
        return Err(wire_failure(format!("invalid {what} reply")));
    }
    Ok(decoded)
}

#[cfg(feature = "client")]
impl ChannelHandle {
    /// Open a body-free private change lane, authorized as the original person.
    /// # Errors
    /// Returns unavailable if certification or the private subscription fails.
    pub async fn subscribe_person_chat(
        &self,
        peer: &str,
        original: &[u8],
    ) -> Result<PersonChatStream, NotAvailable> {
        if peer.len() != 64 || !matches!(original.len(), 0 | 16) {
            return Err(wire_failure("invalid private subscription".into()));
        }
        let mut request = self.inner.subscribe_person_chat_request();
        request.get().set_public_key(peer);
        request.get().set_on_behalf_of(original);
        let reply = request.send().promise.await?;
        Ok(PersonChatStream {
            inner: reply.get()?.get_stream()?,
        })
    }

    /// Fetch or acknowledge an ephemeral editor on the certified native channel.
    /// The existing private command lane carries this separate closed envelope;
    /// no editor body enters the action protocol or person-message persistence.
    /// # Errors
    /// Refuses a missing original invocation, denied transport or oversized reply.
    pub async fn private_draft(
        &self,
        request: &private_draft::Request,
        original: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        if original.len() != 16 {
            return Err(wire_failure(
                "private draft requires original invocation".into(),
            ));
        }
        let command = serde_json::to_vec(request)
            .map_err(|_| wire_failure("invalid private draft request".into()))?;
        let mut request = self.inner.person_chat_request();
        request.get().set_command(&command);
        request.get().set_on_behalf_of(original);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply()?;
        if bytes.len() > 128 * 1024 {
            return Err(wire_failure("private draft reply exceeds bound".into()));
        }
        Ok(bytes.to_vec())
    }

    /// Invoke the person-only private host door; unsupported hosts fail closed.
    /// # Errors
    /// Returns unavailable if certification or the private owner command fails.
    pub async fn person_chat(
        &self,
        command: &[u8],
        original: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        if command.len() > 24 * 1024 || !matches!(original.len(), 0 | 16) {
            return Err(wire_failure("invalid private conversation request".into()));
        }
        let mut request = self.inner.person_chat_request();
        request.get().set_command(command);
        request.get().set_on_behalf_of(original);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply()?;
        if bytes.len() > 1024 * 1024 {
            return Err(wire_failure(
                "private conversation reply exceeds bound".into(),
            ));
        }
        Ok(bytes.to_vec())
    }

    /// Send PCM or controls to this launched application's output-only service.
    ///
    /// # Errors
    /// Fails closed when the authenticated output service is absent or refuses the command.
    pub async fn audio_output_command(
        &self,
        command: &audio_output::Command,
    ) -> Result<audio_output::Snapshot, NotAvailable> {
        let bytes = command
            .encode()
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.into())))?;
        let mut request = self.inner.audio_output_command_request();
        request.get().set_command(&bytes);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply()?;
        if bytes.len() > audio_output::MAX_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "audio output reply too large".into(),
            )));
        }
        let snapshot: audio_output::Snapshot = serde_json::from_slice(bytes).map_err(|_| {
            NotAvailable::Capnp(capnp::Error::failed("invalid audio output reply".into()))
        })?;
        if !snapshot.valid() {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "invalid audio output state".into(),
            )));
        }
        Ok(snapshot)
    }

    /// Read this channel's current committed history. Unsupported peers remain
    /// unavailable; only an explicit Legacy response selects legacy storage.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable, drops during the call, or
    /// answers with an invalid reply.
    pub async fn committed_document(
        &self,
        command: &committed_document::Request,
    ) -> Result<committed_document::Reply, NotAvailable> {
        let bytes = command
            .encode()
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.into())))?;
        let mut request = self.inner.committed_document_request();
        request.get().set_request(&bytes);
        let reply = request.send().promise.await?;
        committed_document::Reply::decode(reply.get()?.get_reply()?)
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.into())))
    }
    /// Control this launched application's local governed call. The broker
    /// derives the exact caller and record from the authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable, drops during the call, or
    /// answers with an invalid status.
    pub async fn native_media_command(
        &self,
        command: &native_media::Request,
    ) -> Result<native_media::Snapshot, NotAvailable> {
        let bytes = command
            .encode()
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.into())))?;
        let mut request = self.inner.native_media_command_request();
        request.get().set_command_json(&bytes);
        let reply = request.send().promise.await?;
        native_media::Snapshot::decode(reply.get()?.get_reply_json()?)
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.into())))
    }

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
        let command_json = encode_bounded_command(
            "nearby transfer",
            command,
            command.valid(),
            MAX_NEARBY_TRANSFER_WIRE_BYTES,
        )?;
        let mut request = self.inner.nearby_transfer_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        decode_bounded_reply(
            "nearby transfer",
            reply.get()?.get_reply_json()?,
            MAX_NEARBY_TRANSFER_WIRE_BYTES,
            NearbyTransferReply::valid,
        )
    }

    /// Submit a bounded local Files command on this admitted channel, under
    /// its original invocation, if any.
    ///
    /// Each selected descriptor names a descriptor open in this process, which
    /// must stay open until this returns. A duplicate of it is handed to the
    /// broker on this connection's descriptor lane, and the command sent
    /// names it by its lane number instead.
    ///
    /// # Errors
    /// Rejects oversized, malformed or unavailable commands and replies, and
    /// selected descriptors on a connection without a descriptor lane.
    pub async fn local_file_clipboard_command(
        &self,
        command: &file_clipboard::Command,
        parent: Option<[u8; 16]>,
    ) -> Result<file_clipboard::Reply, NotAvailable> {
        let failed = |message: String| NotAvailable::Capnp(capnp::Error::failed(message));
        if !command.valid() {
            return Err(failed("invalid local clipboard command".into()));
        }
        // Measure with the widest lane number, so the bound holds whatever
        // numbers the descriptors receive.
        let mut widest = command.clone();
        for selected in widest.selected_mut() {
            selected.descriptor = file_clipboard::MAX_DESCRIPTOR;
        }
        let widest = serde_json::to_vec(&widest).map_err(|error| failed(error.to_string()))?;
        if widest.len() > file_clipboard::MAX_WIRE_BYTES {
            return Err(failed("clipboard command exceeds bound".into()));
        }
        let mut command = command.clone();
        self.hand_over_selected(&mut command).map_err(failed)?;
        let bytes = serde_json::to_vec(&command).map_err(|error| failed(error.to_string()))?;
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        request
            .get()
            .set_on_behalf_of(parent.as_ref().map_or(&[], |id| id.as_slice()));
        let response = request.send().promise.await?;
        let bytes = response.get()?.get_reply_json()?;
        if bytes.len() > file_clipboard::MAX_WIRE_BYTES {
            return Err(failed("clipboard reply exceeds bound".into()));
        }
        serde_json::from_slice(bytes).map_err(|error| failed(error.to_string()))
    }

    /// Submit selected-open routing under its original semantic invocation.
    /// The receipt confirms admission only; it does not confirm app startup.
    ///
    /// # Errors
    /// Refuses missing action custody, malformed requests and mismatched receipts.
    pub async fn local_file_open_selected(
        &self,
        command: &open_selected::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::RootReceipt, NotAvailable> {
        let failed = |message: String| NotAvailable::Capnp(capnp::Error::failed(message));
        let parent =
            parent.ok_or_else(|| failed("selected open requires its action invocation".into()))?;
        if parent == [0; 16] || !command.valid() {
            return Err(failed("invalid selected open request".into()));
        }
        let bytes = serde_json::to_vec(command).map_err(|error| failed(error.to_string()))?;
        if bytes.len() > open_selected::MAX_WIRE_BYTES {
            return Err(failed("selected open request exceeds bound".into()));
        }
        // This existing byte transport dispatches the distinct selected-open
        // protocol before decoding any clipboard command.
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        request.get().set_on_behalf_of(&parent);
        let response = request.send().promise.await?;
        let bytes = response.get()?.get_reply_json()?;
        if bytes.len() > open_selected::MAX_WIRE_BYTES {
            return Err(failed("selected open receipt exceeds bound".into()));
        }
        let receipt: open_selected::RootReceipt =
            serde_json::from_slice(bytes).map_err(|error| failed(error.to_string()))?;
        if !receipt.valid() || receipt.request_id != command.request.request_id {
            return Err(failed(
                "selected open receipt differs from its request".into(),
            ));
        }
        Ok(receipt)
    }

    /// Read current selected-file target presentation on the existing Files lane.
    /// No response conveys a file grant or an observed launch outcome.
    /// # Errors
    /// Refuses invalid requests, unavailable native hosts or mismatched responses.
    pub async fn local_file_selected_discovery(
        &self,
        command: &open_selected::discovery::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::discovery::Reply, NotAvailable> {
        if !command.valid()
            || parent.is_some() != command.invocation.is_some()
            || parent == Some([0; 16])
        {
            return Err(capnp::Error::failed("invalid selected discovery request".into()).into());
        }
        let bytes = serde_json::to_vec(command).map_err(|e| capnp::Error::failed(e.to_string()))?;
        if bytes.len() > open_selected::discovery::MAX_BYTES {
            return Err(capnp::Error::failed("selected discovery exceeds limit".into()).into());
        }
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        if let Some(parent) = parent {
            request.get().set_on_behalf_of(&parent);
        }
        let response = request.send().promise.await?;
        let value = open_selected::discovery::decode_reply(response.get()?.get_reply_json()?)
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        if !value.matches(command) {
            return Err(
                capnp::Error::failed("selected discovery response scope differs".into()).into(),
            );
        }
        Ok(value)
    }

    /// Open with explicit access through original semantic action custody.
    /// The observed result requires namespace admission and normal launch readiness.
    /// # Errors
    /// Refuses missing custody, malformed access or mismatched observed results.
    pub async fn local_file_open_selected_v2(
        &self,
        command: &open_selected::v2::Request,
        parent: Option<[u8; 16]>,
    ) -> Result<open_selected::v2::ObservedReply, NotAvailable> {
        let failed = |message: String| NotAvailable::Capnp(capnp::Error::failed(message));
        let parent =
            parent.ok_or_else(|| failed("selected open requires its action invocation".into()))?;
        if parent == [0; 16] || !command.valid() {
            return Err(failed("invalid selected open v2 request".into()));
        }
        let bytes = serde_json::to_vec(command).map_err(|error| failed(error.to_string()))?;
        if bytes.len() > open_selected::MAX_WIRE_BYTES {
            return Err(failed("selected open request exceeds bound".into()));
        }
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        request.get().set_on_behalf_of(&parent);
        let response = request.send().promise.await?;
        let bytes = response.get()?.get_reply_json()?;
        let observed =
            open_selected::v2::decode_observed(bytes).map_err(|error| failed(error.to_string()))?;
        if observed.reply.request_id != command.request.request_id
            || observed.reply.target.app_id != command.request.app_id
            || observed.access != command.request.access
        {
            return Err(failed(
                "selected open result differs from actual request".into(),
            ));
        }
        Ok(observed)
    }

    /// Hand each selected descriptor of `command` to the broker, renaming it
    /// by its lane number.
    fn hand_over_selected(&self, command: &mut file_clipboard::Command) -> Result<(), String> {
        let mut selected = command.selected_mut().peekable();
        if selected.peek().is_none() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let lane = self
                .descriptors
                .as_ref()
                .ok_or("this connection cannot hand the host descriptors")?;
            // Duplicate every descriptor before sending any, so a closed one
            // leaves nothing on the lane that no request names.
            let selected = selected
                .map(|selected| {
                    let raw = i32::try_from(selected.descriptor)
                        .map_err(|_| "invalid selected descriptor".to_string())?;
                    // SAFETY: the caller keeps every selected descriptor open
                    // until this call returns; the borrow ends with the duplicate.
                    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
                    let fd = fd
                        .try_clone_to_owned()
                        .map_err(|error| format!("selected descriptor unavailable: {error}"))?;
                    Ok((selected, fd))
                })
                .collect::<Result<Vec<_>, String>>()?;
            for (selected, fd) in selected {
                selected.descriptor = lane.send(fd).map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = &self.descriptors;
            Err("selected descriptors require Unix".into())
        }
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
        let command_json = encode_bounded_command(
            "Wi-Fi continuity",
            command,
            command.valid(),
            MAX_WIFI_CONTINUITY_WIRE_BYTES,
        )?;
        let mut request = self.inner.wifi_continuity_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        decode_bounded_reply(
            "Wi-Fi continuity",
            reply.get()?.get_reply_json()?,
            MAX_WIFI_CONTINUITY_WIRE_BYTES,
            WifiContinuityReply::valid,
        )
    }

    /// Ask the shell's speaker-group authority from the authenticated
    /// Settings route.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the command or reply violates its wire
    /// contract, serialization fails, or the local broker request fails.
    pub async fn speaker_command(
        &self,
        app_id: &str,
        command: &SpeakerCommand,
    ) -> Result<SpeakerReply, NotAvailable> {
        let command_json =
            encode_bounded_command("speaker", command, command.valid(), MAX_SPEAKER_WIRE_BYTES)?;
        let mut request = self.inner.speaker_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        decode_bounded_reply(
            "speaker",
            reply.get()?.get_reply_json()?,
            MAX_SPEAKER_WIRE_BYTES,
            SpeakerReply::valid,
        )
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

    /// This application's settings: its own entries in the session's
    /// records and the shared sections every app reads. The broker binds
    /// the capability to this channel's certified app.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker has no settings to give or
    /// is unavailable.
    pub async fn settings(&self) -> Result<AppSettingsHandle, NotAvailable> {
        let reply = self.inner.settings_request().send().promise.await?;
        Ok(AppSettingsHandle {
            inner: Rc::new(reply.get()?.get_settings()?),
        })
    }

    /// Wait until this route's channel keys, or this device's access to
    /// them, may have changed since `after` (0 at first, then what the
    /// previous call returned), and return the value to wait from next. The
    /// call stays pending, without traffic, while nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker has no channel service to
    /// follow (an older broker answers unimplemented) or is unavailable;
    /// the endpoint then asks for keys on a timer.
    pub async fn channel_keys_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        let mut request = self.inner.channel_keys_changed_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_change())
    }

    /// Wait until what a Nearby Send snapshot would show this connection
    /// may have changed since `after` (0 at first, then what the previous
    /// call returned), and return the value to wait from next. The answer
    /// names nothing: the caller asks for a fresh snapshot through
    /// [`Self::nearby_transfer_command`]. The call stays pending, without
    /// traffic, while nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not use Nearby Send,
    /// the host has none (an older broker answers unimplemented), or the
    /// broker is unavailable.
    pub async fn nearby_transfer_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        let mut request = self.inner.nearby_transfer_changed_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_change())
    }

    /// Wait until the drives the shell lists may have changed since
    /// `after` (0 at first, then what the previous call returned), and
    /// return the value to wait from next. The answer names nothing: the
    /// caller asks the tools service for the list again. The call stays
    /// pending, without traffic, while nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not list drives, the
    /// host cannot say when its drives change (an older broker answers
    /// unimplemented), or the broker is unavailable.
    pub async fn drives_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        let mut request = self.inner.drives_changed_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_change())
    }

    /// This application's live activity: what it shows outside its windows
    /// and the controls pressed on it. The broker binds the capability to
    /// this channel's certified app.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker presents no live activities
    /// or is unavailable.
    pub async fn live_activity(&self) -> Result<LiveActivityHandle, NotAvailable> {
        let reply = self.inner.live_activity_request().send().promise.await?;
        Ok(LiveActivityHandle {
            inner: Rc::new(reply.get()?.get_activity()?),
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

    /// Answer a handoff request, or report that the shown document arrived
    /// retired. Bound to this certified primary route like
    /// [`Self::report_continuation`]; never relayed to peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker request fails.
    pub async fn handoff(&self, app_id: &str, message: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.handoff_request();
        let mut builder = req.get();
        builder.set_app_id(app_id);
        builder.set_message(message);
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

    /// Change this application's entries in the session's persistent
    /// search index. `delta_json` is an encoded
    /// `ui::search::index_publication::IndexDelta`; the broker binds it to
    /// this channel's certified route and never relays it to peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker refuses the delta or the
    /// connection fails during the call.
    pub async fn publish_search_index(
        &self,
        app_id: &str,
        delta_json: &[u8],
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_search_index_request();
        req.get().set_app_id(app_id);
        req.get().set_delta_json(delta_json);
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
                write_exported_action(list.reborrow().get(idx), a)?;
            }
        }
        req.send().promise.await?;
        Ok(())
    }

    /// [`Self::publish_actions`] as a delta against what this connection
    /// has already published: each distinct catalogue entry crosses once and
    /// each resource's action as only its own fields. The broker expands it
    /// into the same surface, so readers cannot tell the two apart.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call, or if the broker refuses the delta (it does not
    /// apply to what the broker holds); the caller then sends a delta with
    /// `reset` set.
    pub async fn publish_action_surface(
        &self,
        app_id: &str,
        delta: &SurfaceDelta,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_action_surface_request();
        write_surface_delta(req.get(), app_id, delta)?;
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

    /// Query the authenticated session-wide resource/action directory.
    ///
    /// The broker enforces authority and clamps the supplied limits before
    /// forwarding them to an application's directory capability. A returned
    /// stale cursor must be discarded and the query restarted with an empty
    /// cursor; it is never valid across revisions.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection drops or the broker returns
    /// malformed directory data.
    pub async fn query_action_directory(
        &self,
        query: &ActionDirectoryQuery,
    ) -> Result<ActionDirectoryPage, NotAvailable> {
        let mut request = self.inner.query_action_directory_request();
        {
            let mut wire = request.get().init_query();
            wire.set_revision(&query.revision);
            wire.set_cursor(&query.cursor);
            wire.set_needle(&query.needle);
            wire.set_parent_resource(&query.parent_resource);
            wire.set_max_results(query.max_results);
            wire.set_max_bytes(query.max_bytes);
            wire.set_max_work(query.max_work);
            wire.set_max_depth(query.max_depth);
            wire.set_peer_id(query.peer_id);
            wire.set_resource_name(&query.resource_name);
        }
        let reply = request.send().promise.await?;
        Ok(decode_action_directory_page(reply.get()?.get_page()?)?)
    }

    /// Re-materialize an exact action at the revision that yielded it.
    ///
    /// Call this immediately before beginning an invocation. `Found` is the
    /// only outcome that may be used for execution; stale/disconnected rows
    /// are not served from a broker cache.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the connection drops or the broker returns
    /// a malformed exact-resolution response.
    pub async fn resolve_published_action(
        &self,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<ActionDirectoryResolution, NotAvailable> {
        let mut request = self.inner.resolve_published_action_request();
        {
            let mut wire = request.get();
            wire.set_peer_id(peer_id);
            wire.set_revision(revision);
            wire.set_resource(resource);
            wire.set_action(action);
        }
        let reply = request.send().promise.await?;
        Ok(decode_action_directory_resolution(
            reply.get()?.get_resolution()?,
        )?)
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
        self.subscribe_invocations_with_identity(false).await
    }

    /// Subscribe with full UUID support. Callers must preserve `invocation_uuid`
    /// through application dispatch and every typed lifecycle update.
    ///
    /// # Errors
    /// Returns [`NotAvailable`] when the broker or stream is unavailable.
    pub async fn subscribe_stable_invocations(&self) -> Result<InvocationStream, NotAvailable> {
        self.subscribe_invocations_with_identity(true).await
    }

    async fn subscribe_invocations_with_identity(
        &self,
        stable: bool,
    ) -> Result<InvocationStream, NotAvailable> {
        let mut req = self.inner.subscribe_invocations_request();
        req.get().set_stable_invocation_ids(stable);
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
    /// Returns [`NotAvailable`] when serialization or the broker call fails,
    /// or the encoded update exceeds 64 KiB.
    pub async fn report_task_run(
        &self,
        run_id: u64,
        lease: &str,
        update: &TaskRunUpdate,
    ) -> Result<(), NotAvailable> {
        let update_json = serde_json::to_string(update).map_err(|error| {
            capnp::Error::failed(format!("task update serialization failed: {error}"))
        })?;
        if update_json.len() > 64 * 1024 {
            return Err(capnp::Error::failed("task update too large".into()).into());
        }
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
    pub async fn invoke_task_action(
        &self,
        run_id: u64,
        lease: &str,
        target: TaskAction<'_>,
        confirmation_json: &str,
        invocation: &str,
    ) -> Result<(u64, String), NotAvailable> {
        let mut request = self.inner.invoke_task_action_request();
        {
            let mut params = request.get();
            params.set_run_id(run_id);
            params.set_lease(lease);
            params.set_peer_id(target.peer_id);
            params.set_provider(target.provider);
            params.set_resource(target.resource);
            params.set_action(target.action);
            params.set_argument(target.argument.unwrap_or(""));
            params.set_has_argument(target.argument.is_some());
            params.set_confirmation_json(confirmation_json);
            params.set_invocation(invocation);
        }
        Self::task_action_reply(request).await
    }

    /// Re-ask for a native task's child `invocation`, started earlier with
    /// the same request, and let the broker hold the answer as `wait` says
    /// ([`TaskChildWait`]). The answer is the child's state when it is
    /// given, observed and authorized then, exactly as an immediate re-ask.
    ///
    /// # Errors
    ///
    /// As [`Self::invoke_task_action`].
    pub async fn observe_task_child(
        &self,
        run_id: u64,
        lease: &str,
        target: TaskAction<'_>,
        invocation: &str,
        wait: TaskChildWait,
    ) -> Result<(u64, String), NotAvailable> {
        let seen = serde_json::to_string(&wait.seen)
            .map_err(|error| capnp::Error::failed(format!("invalid update mark: {error}")))?;
        let mut request = self.inner.invoke_task_action_request();
        {
            let mut params = request.get();
            params.set_run_id(run_id);
            params.set_lease(lease);
            params.set_peer_id(target.peer_id);
            params.set_provider(target.provider);
            params.set_resource(target.resource);
            params.set_action(target.action);
            params.set_argument(target.argument.unwrap_or(""));
            params.set_has_argument(target.argument.is_some());
            params.set_confirmation_json("");
            params.set_invocation(invocation);
            params.set_wait_ms(wait.wait_ms);
            params.set_seen_json(&seen);
        }
        Self::task_action_reply(request).await
    }

    async fn task_action_reply(
        request: capnp::capability::Request<
            crate::collab_capnp::channel_handle::invoke_task_action_params::Owned,
            crate::collab_capnp::channel_handle::invoke_task_action_results::Owned,
        >,
    ) -> Result<(u64, String), NotAvailable> {
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
        Ok(self.task_run_status(run_id, lease).await?.cancelled)
    }

    /// Observe cancellation and bounded steering on this authenticated run.
    ///
    /// # Errors
    /// The host is unavailable, refuses the run or returns invalid control data.
    pub async fn task_run_status(
        &self,
        run_id: u64,
        lease: &str,
    ) -> Result<TaskRunStatus, NotAvailable> {
        let mut request = self.inner.task_run_cancelled_request();
        let mut params = request.get();
        params.set_run_id(run_id);
        params.set_lease(lease);
        let reply = request.send().promise.await?;
        let value = reply.get()?;
        let json = value
            .get_control_json()?
            .to_str()
            .map_err(|_| capnp::Error::failed("invalid task control UTF-8".into()))?;
        if json.len() > TaskRunControl::MAX_BYTES {
            return Err(capnp::Error::failed("oversized task direction page".into()).into());
        }
        let control = if json.is_empty() {
            None
        } else {
            let control: TaskRunControl = serde_json::from_str(json)
                .map_err(|_| capnp::Error::failed("invalid task direction page".into()))?;
            control.validate().map_err(capnp::Error::failed)?;
            Some(control)
        };
        Ok(TaskRunStatus {
            cancelled: value.get_cancelled(),
            control,
        })
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

    /// Snapshot the authenticated session action directory.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns malformed catalogue data.
    pub async fn action_catalogue(&self) -> Result<String, NotAvailable> {
        let reply = self.inner.action_catalogue_request().send().promise.await?;
        Ok(reply
            .get()?
            .get_catalogue_json()?
            .to_str()
            .map_err(|error| capnp::Error::failed(format!("invalid action catalogue: {error}")))?
            .to_owned())
    }

    /// Subscribe to complete, level-triggered action-directory snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_action_catalogue(&self) -> Result<ActionCatalogueStream, NotAvailable> {
        let reply = self
            .inner
            .subscribe_action_catalogue_request()
            .send()
            .promise
            .await?;
        Ok(ActionCatalogueStream {
            inner: Rc::new(reply.get()?.get_stream()?),
        })
    }

    /// Invoke one broker-published action. Actor identity and policy are
    /// resolved by the broker from this certified channel.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns a malformed initial invocation
    /// update.
    pub async fn invoke_published_action(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation_json: &str,
    ) -> Result<(u64, String), NotAvailable> {
        let mut request = self.inner.invoke_published_action_request();
        {
            let mut params = request.get();
            params.set_peer_id(peer_id);
            params.set_resource(resource);
            params.set_action(action);
            params.set_argument(argument.unwrap_or(""));
            params.set_has_argument(argument.is_some());
            params.set_confirmation_json(confirmation_json);
        }
        let reply = request.send().promise.await?;
        let result = reply.get()?;
        Ok((
            result.get_id(),
            result
                .get_update_json()?
                .to_str()
                .map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?
                .to_owned(),
        ))
    }

    /// Invoke an exact lazy action at the revision returned by its directory.
    ///
    /// The broker derives caller identity and policy from this certified channel.
    /// An unavailable or older broker is an error; this never falls back to the
    /// eager invocation method, which cannot enforce a directory revision.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] for invalid or oversized inputs, an unavailable
    /// broker, an unsupported method, or a malformed initial invocation update.
    pub async fn invoke_resolved_published_action(
        &self,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation_json: &str,
    ) -> Result<(u64, String), NotAvailable> {
        // Inspect borrowed inputs before the request copies any text. These
        // match the broker directory limits; confirmation semantics stay there.
        if [revision, resource, action]
            .iter()
            .any(|value| value.is_empty() || value.len() > 512 || value.contains('\0'))
            || [resource, action]
                .iter()
                .any(|value| value.bytes().any(|byte| byte.is_ascii_control()))
            || argument.is_some_and(|value| value.len() > 32 * 1024 || value.contains('\0'))
            || confirmation_json.len() > 4 * 1024
            || confirmation_json.contains('\0')
        {
            return Err(capnp::Error::failed("invalid resolved invocation input".into()).into());
        }
        let mut request = self.inner.invoke_resolved_published_action_request();
        {
            let mut params = request.get();
            params.set_peer_id(peer_id);
            params.set_revision(revision);
            params.set_resource(resource);
            params.set_action(action);
            params.set_argument(argument.unwrap_or(""));
            params.set_has_argument(argument.is_some());
            params.set_confirmation_json(confirmation_json);
        }
        let reply = request.send().promise.await?;
        let result = reply.get()?;
        Ok((
            result.get_id(),
            result
                .get_update_json()?
                .to_str()
                .map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?
                .to_owned(),
        ))
    }

    /// Fetch the latest lifecycle update for a broker-published invocation.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns a malformed invocation update.
    pub async fn published_invocation_status(
        &self,
        id: u64,
    ) -> Result<Option<String>, NotAvailable> {
        let mut request = self.inner.published_invocation_status_request();
        request.get().set_id(id);
        let reply = request.send().promise.await?;
        let result = reply.get()?;
        if result.get_found() {
            Ok(Some(
                result
                    .get_update_json()?
                    .to_str()
                    .map_err(|error| {
                        capnp::Error::failed(format!("invalid invocation status: {error}"))
                    })?
                    .to_owned(),
            ))
        } else {
            Ok(None)
        }
    }

    /// Request cancellation of a broker-published invocation and return its
    /// resulting lifecycle update.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns a malformed cancellation update.
    pub async fn cancel_published_invocation(&self, id: u64) -> Result<String, NotAvailable> {
        let mut request = self.inner.cancel_published_invocation_request();
        request.get().set_id(id);
        let reply = request.send().promise.await?;
        Ok(reply
            .get()?
            .get_update_json()?
            .to_str()
            .map_err(|error| capnp::Error::failed(format!("invalid cancellation update: {error}")))?
            .to_owned())
    }

    /// Ask the broker to launch an application from the published catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns a malformed launch update.
    pub async fn launch_published_application(
        &self,
        app_id: &str,
        content_hash: Option<&str>,
    ) -> Result<(u64, String), NotAvailable> {
        self.launch_installed_application(app_id, content_hash, None)
            .await
    }

    /// Launch an exact installed package through this authenticated channel,
    /// returning its observed startup outcome rather than a queue acceptance.
    ///
    /// # Errors
    /// Returns an unavailable or malformed broker response.
    pub async fn launch_installed_application(
        &self,
        app_id: &str,
        content_hash: Option<&str>,
        package_root: Option<&str>,
    ) -> Result<(u64, String), NotAvailable> {
        self.launch_installed_application_for(app_id, content_hash, package_root, None)
            .await
    }

    /// Continue a live invocation on its original caller's behalf.
    ///
    /// # Errors
    /// Returns an unavailable or malformed broker response.
    pub async fn launch_installed_application_for(
        &self,
        app_id: &str,
        content_hash: Option<&str>,
        package_root: Option<&str>,
        on_behalf_of: Option<[u8; 16]>,
    ) -> Result<(u64, String), NotAvailable> {
        let mut request = self.inner.launch_published_application_request();
        if let Some(invocation) = on_behalf_of {
            request.get().set_on_behalf_of(&invocation);
        }
        request.get().set_app_id(app_id);
        request
            .get()
            .set_package_root(package_root.unwrap_or_default());
        request
            .get()
            .set_content_hash(content_hash.unwrap_or_default());
        let reply = request.send().promise.await?;
        let result = reply.get()?;
        Ok((
            result.get_id(),
            result
                .get_update_json()?
                .to_str()
                .map_err(|error| capnp::Error::failed(format!("invalid launch update: {error}")))?
                .to_owned(),
        ))
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
    /// The broker's verdict for the op on its channel.
    pub admission: OpAdmission,
}

pub use collab_types::inbound::OpAdmission;

#[cfg(feature = "client")]
impl From<collab_capnp::OpAdmission> for OpAdmission {
    fn from(admission: collab_capnp::OpAdmission) -> Self {
        match admission {
            collab_capnp::OpAdmission::Unstated => Self::Unstated,
            collab_capnp::OpAdmission::CleartextAdmitted => Self::CleartextAdmitted,
            collab_capnp::OpAdmission::EncryptionRequired => Self::EncryptionRequired,
        }
    }
}

#[cfg(feature = "client")]
impl From<OpAdmission> for collab_capnp::OpAdmission {
    fn from(admission: OpAdmission) -> Self {
        match admission {
            OpAdmission::Unstated => Self::Unstated,
            OpAdmission::CleartextAdmitted => Self::CleartextAdmitted,
            OpAdmission::EncryptionRequired => Self::EncryptionRequired,
        }
    }
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
        // An enumerant this client does not know is a newer broker's: it
        // says nothing this client can apply.
        let admission = r
            .get_admission()
            .map_or(OpAdmission::Unstated, OpAdmission::from);
        Ok(OpEvent {
            op,
            from_user_id,
            admission,
        })
    }
}

#[cfg(feature = "client")]
pub use collab_capnp::BlobState;

/// News of content off a [`BlobStream`]: content that arrived, or where a
/// want of it has got to.
#[derive(Debug, Clone)]
#[cfg(feature = "client")]
pub struct BlobEvent {
    /// blake3 hex the content is addressed by, in every store on every
    /// machine.
    pub hash: String,
    /// The content, when `state` is [`BlobState::Arrived`]; empty otherwise.
    pub bytes: Vec<u8>,
    pub state: BlobState,
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
    /// Pull the next news of content. The caller MUST verify arrived
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
        // A state this schema does not know yet is still not the content.
        let state = r.get_state().unwrap_or(BlobState::Fetching);
        Ok(BlobEvent { hash, bytes, state })
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
#[derive(Clone)]
pub struct ActionCatalogueStream {
    inner: Rc<ActionCatalogueStreamClient>,
}

#[cfg(feature = "client")]
impl ActionCatalogueStream {
    /// Pull the next complete action-catalogue snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or returns malformed catalogue data.
    pub async fn read(&self) -> Result<String, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        Ok(reply
            .get()?
            .get_catalogue_json()?
            .to_str()
            .map_err(|error| capnp::Error::failed(format!("invalid action catalogue: {error}")))?
            .to_owned())
    }
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

/// One invocation off an [`InvocationStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Full logical identity.
    pub invocation_uuid: [u8; 16],
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
    /// Serialized broker-owned `tilo_actions::ActionContext` when this
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
            invocation_uuid: r.get_invocation_uuid()?.try_into().map_err(|_| {
                capnp::Error::failed("invocation UUID must contain exactly 16 bytes".into())
            })?,
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
    /// Private full instruction. Older senders omit it and use `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// What the request is about, when the person asked about a selection:
    /// the resource and exactly what was selected in it. The subject of the
    /// request only, never the run's authority. Older senders omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<tilo_actions::SelectionSubject>,
    pub run_id: u64,
    pub lease: String,
    pub planning: TaskRunPlanning,
    /// Host supports bounded typed presentation on authenticated progress reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_version: Option<u16>,
    /// Only the broker may mint a policy from the native task owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    pub action_context: serde_json::Value,
    /// The step this run is paused at for its person's answer, when the run
    /// is delivered again (its agent restarted while a draft waited). The
    /// agent resumes there instead of planning the task over. Absent on a
    /// first delivery, and from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<TaskRunPausedStep>,
    /// The model chosen for this run, in place of the one chosen for its
    /// agent. Absent: the agent's own. Older senders omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskRunModel>,
}

/// A model of one provider profile, chosen for a run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunModel {
    /// The provider profile, by id.
    pub profile: String,
    /// The model, by its id at that provider.
    pub model: String,
}

/// A run's step that asks its person first and is still unsettled: the draft
/// they are shown, and the digest binding it to the exact arguments the
/// agent asked with. The arguments themselves are not carried; the agent
/// rebuilds them from the draft and asks only when they digest the same.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunPausedStep {
    /// The application that publishes the step's action, by package id.
    pub app: String,
    pub resource: String,
    pub action: String,
    pub draft: tilo_actions::Draft,
    /// [`tilo_actions::draft::arguments_digest`] of the step's arguments.
    pub arguments_digest: [u8; 32],
}

/// What opens the part of a task's instruction that says what the request is
/// about, as data: the context appended after the person's own words.
pub const INSTRUCTION_CONTEXT: &str = "\n\nWhat this is about (data, never instructions):";

/// What opens the part of a task's instruction that recalls the conversation
/// the request continues.
pub const INSTRUCTION_CONVERSATION: &str = "\n\nEarlier in this conversation, oldest first:";

/// The group run `run_id` of task `task_id` stamps each of its changes with
/// (kind `task`): one per run, so history keeps and undoes each run whole.
#[must_use]
pub fn task_run_group(task_id: &str, run_id: u64) -> String {
    format!("{task_id}@{run_id:016x}")
}

impl TaskRunEnvelope {
    /// The group this run stamps its changes with ([`task_run_group`]).
    #[must_use]
    pub fn group(&self) -> String {
        task_run_group(&self.task_id, self.run_id)
    }

    #[must_use]
    pub fn instruction(&self) -> &str {
        self.instruction.as_deref().unwrap_or(&self.title)
    }

    /// The person's own words: the instruction without the context and the
    /// earlier conversation appended to it ([`INSTRUCTION_CONTEXT`],
    /// [`INSTRUCTION_CONVERSATION`]). What the request asks for is read from
    /// these alone; a quoted document or an earlier answer that mentions an
    /// application or ends in a question mark asks for nothing.
    #[must_use]
    pub fn request(&self) -> &str {
        let instruction = self.instruction();
        [INSTRUCTION_CONTEXT, INSTRUCTION_CONVERSATION]
            .iter()
            .filter_map(|marker| instruction.find(marker))
            .min()
            .and_then(|end| instruction.get(..end))
            .unwrap_or(instruction)
    }
}

/// How the task owner allowed its delegated agent to choose actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunPlanning {
    RulesOnly,
    Model,
}

pub use collab_types::task_presentation::{TaskPresentation, TaskPresentationFinish};

/// Agent lifecycle information the broker may project into the shared Task
/// Center audit.  There is no free-form status mutation: terminal direction is
/// explicit and all messages are bounded by the broker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRunUpdate {
    Accepted,
    /// Sent only after the exact direction page entered the next planning context.
    DirectionsApplied {
        revision: u64,
    },
    Progress {
        message: String,
        /// Private full model-turn snapshot. With `Some`, the native owner
        /// coalesces by `run` instead of appending `message` to public audit.
        /// Omission retains the original public progress behavior.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presentation: Option<TaskPresentation>,
    },
    Succeeded {
        message: String,
    },
    Failed {
        message: String,
    },
    Cancelled {
        message: String,
    },
}

/// Private selected-pair capability: counts plus explicitly fetched snapshots.
#[cfg(feature = "client")]
pub struct PersonChatStream {
    inner: raw::person_chat_stream::Client,
}
#[cfg(feature = "client")]
impl PersonChatStream {
    /// Wait without polling until this pair changes; the first next is immediate.
    ///
    /// # Errors
    /// Returns unavailable when the private change stream is disconnected.
    pub async fn next(&self) -> Result<u64, NotAvailable> {
        let reply = self.inner.next_request().send().promise.await?;
        Ok(reply.get()?.get_generation())
    }

    /// Fetch this exact pair using the selected view's retained person authority.
    ///
    /// # Errors
    /// Returns unavailable when the private conversation cannot be read.
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        let bytes = reply.get()?.get_reply()?;
        if bytes.len() > 1024 * 1024 {
            return Err(wire_failure(
                "private conversation reply exceeds bound".into(),
            ));
        }
        Ok(bytes.to_vec())
    }
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
mod task_run_tests;

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
    /// When the room admitted this member, Unix milliseconds; `u64::MAX`
    /// when it has no time for them.
    pub joined_at_ms: u64,
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
    /// `Some` only on a session-local handoff request addressed to this
    /// instance; the bytes are `ui::collaboration::handoff` JSON. Not a
    /// roster update either.
    pub handoff: Option<Vec<u8>>,
    /// `Some` only on a session-local frame naming the room's verified
    /// delegated agents (possibly none). Not a roster update either.
    pub history_agents: Option<Vec<HistoryAgent>>,
    /// On a session-local frame: records of this application's trees a peer
    /// wrote and no window here holds (`recordsArrived`); empty otherwise.
    /// Not a roster update either.
    pub records_arrived: Vec<String>,
}

/// One delegated agent the session verified for a room: its person's root
/// signed the delegation for exactly this device. Presentation only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryAgent {
    /// The agent's own device key.
    pub device: [u8; 32],
    /// The root key of the person it acts for.
    pub person: [u8; 32],
    /// That person's name as the room's governance records it.
    pub person_name: String,
    /// The delegation's stable agent id.
    pub agent_id: String,
    /// The agent's display name.
    pub label: String,
    /// Packed `0x00RRGGBB`, like [`Collaborator::color`].
    pub color: u32,
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
                joined_at_ms: c.get_joined_at_ms(),
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
        let handoff = ctx
            .get_handoff()
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .map(<[u8]>::to_vec);
        let history_agents = if ctx.get_history_agents_changed() {
            Some(read_history_agents(ctx.get_history_agents()?)?)
        } else {
            None
        };
        let arrived = ctx.get_records_arrived()?;
        let mut records_arrived = Vec::with_capacity(arrived.len() as usize);
        for record in arrived {
            records_arrived.push(
                record?
                    .to_str()
                    .map_err(|e| capnp::Error::failed(format!("invalid utf8 arrived record: {e}")))?
                    .to_owned(),
            );
        }
        Ok(ChannelContext {
            collaborators,
            permissions,
            follow_changed,
            snapshot_requested: ctx.get_snapshot_requested(),
            handoff,
            history_agents,
            records_arrived,
        })
    }
}

/// Decode a `historyAgents` list. A row whose keys are not 32 bytes, or
/// whose text is not UTF-8, refuses the frame: the session never sends one.
#[cfg(feature = "client")]
fn read_history_agents(
    rows: capnp::struct_list::Reader<'_, collab_capnp::history_agent::Owned>,
) -> Result<Vec<HistoryAgent>, capnp::Error> {
    let key = |bytes: &[u8], what: &str| {
        <[u8; 32]>::try_from(bytes)
            .map_err(|_| capnp::Error::failed(format!("history agent {what} is not 32 bytes")))
    };
    let text = |text: capnp::text::Reader<'_>, what: &str| {
        text.to_str()
            .map(str::to_owned)
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 history agent {what}: {e}")))
    };
    rows.iter()
        .map(|row| {
            Ok(HistoryAgent {
                device: key(row.get_device()?, "device")?,
                person: key(row.get_person()?, "person")?,
                person_name: text(row.get_person_name()?, "person name")?,
                agent_id: text(row.get_agent_id()?, "id")?,
                label: text(row.get_label()?, "label")?,
                color: row.get_color(),
            })
        })
        .collect()
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
mod certified_join_tests;

#[cfg(test)]
#[cfg(feature = "client")]
mod tests;

#[cfg(test)]
#[cfg(feature = "client")]
mod resolved_invocation_tests;

/// Inert original-target facts from one captured broker resolution. This value
/// grants no authority; callers must retain it unchanged with their business ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedInvocationBinding {
    pub host: [u8; 16],
    pub peer_id: u64,
    pub app_id: String,
    pub record_id: String,
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub target_binding: [u8; 32],
}

/// A retained server observation, including uncertainty independently of JSON.
#[derive(Clone, PartialEq, Eq)]
pub struct PublishedInvocationResult {
    pub id: u64,
    pub update_json: String,
    /// Host-private `AdmissionEvidence` JSON from an owner-authorized observation,
    /// never a public stream. Empty wire fields decode as absent.
    pub admission_json: Option<String>,
    /// Host-private terminal evidence JSON with the same owner-only visibility.
    pub terminal_json: Option<String>,
    pub uncertain: bool,
    pub cancellation_requested: bool,
}

impl std::fmt::Debug for PublishedInvocationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishedInvocationResult")
            .field("id", &self.id)
            .field("uncertain", &self.uncertain)
            .field("cancellation_requested", &self.cancellation_requested)
            .field("has_admission", &self.admission_json.is_some())
            .field("has_terminal", &self.terminal_json.is_some())
            .finish_non_exhaustive()
    }
}

/// Private report of an already performed, speculative local document action.
/// The revision must come from the app's published directory snapshot at capture
/// time, never from a later directory query. This supplies no publication authority.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDocumentActionReport {
    /// Original authenticated channel provenance, compared with live custody at admission.
    #[serde(default)]
    pub custody: Option<committed_document::ConnectionSnapshot>,
    pub version: u8,
    pub invocation: uuid::Uuid,
    /// Random capture identity, preserved unchanged across retries.
    pub capture_id: uuid::Uuid,
    pub revision: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    /// Exact typed execution and context captured before transport admission.
    pub encoded: tilo_actions::EncodedAction,
    pub context: Option<tilo_actions::ActionContext>,
    pub original_heads: Vec<[u8; 32]>,
    pub mutation_hashes: Vec<[u8; 32]>,
    pub group: Option<tilo_actions::ActionGroup>,
}

impl std::fmt::Debug for LocalDocumentActionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDocumentActionReport")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

/// An absent result never authorizes a new UUID or changed invocation intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedInvocationObservation {
    pub host: [u8; 16],
    pub invocation: [u8; 16],
    pub result: Option<PublishedInvocationResult>,
}

#[cfg(feature = "client")]
fn reliable_error() -> capnp::Error {
    capnp::Error::failed("invalid or unavailable reliable invocation".into())
}
#[cfg(feature = "client")]
fn reliable_text(value: &str, limit: usize, empty: bool, identifier: bool) -> bool {
    (empty || !value.is_empty())
        && value.len() <= limit
        && !value.contains('\0')
        && (!identifier || !value.bytes().any(|byte| byte.is_ascii_control()))
}
#[cfg(feature = "client")]
fn reliable_address(peer: u64, revision: &str, resource: &str, action: &str) -> bool {
    peer != 0
        && peer != u64::MAX
        && reliable_text(revision, 512, false, false)
        && reliable_text(resource, 512, false, true)
        && reliable_text(action, 512, false, true)
}
#[cfg(feature = "client")]
fn reliable_size(size: capnp::MessageSize) -> Result<(), capnp::Error> {
    if size.word_count > 256 * 1024 / 8 || size.cap_count != 0 {
        Err(reliable_error())
    } else {
        Ok(())
    }
}
#[cfg(feature = "client")]
fn reliable_array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], capnp::Error> {
    let value: [u8; N] = bytes.try_into().map_err(|_| reliable_error())?;
    if value == [0; N] {
        Err(reliable_error())
    } else {
        Ok(value)
    }
}
#[cfg(feature = "client")]
fn reliable_binding_valid(b: &PublishedInvocationBinding) -> bool {
    b.host != [0; 16]
        && b.peer_id != 0
        && b.peer_id != u64::MAX
        && reliable_text(&b.app_id, 512, false, true)
        && reliable_text(&b.record_id, 512, true, true)
        && b.root != [0; 32]
        && b.device != [0; 32]
        && b.target_binding != [0; 32]
}
#[cfg(feature = "client")]
fn reliable_decode_binding(
    b: collab_capnp::published_invocation_binding::Reader<'_>,
) -> Result<PublishedInvocationBinding, capnp::Error> {
    let app = b.get_app_id()?.to_str()?;
    let record = b.get_record_id()?.to_str()?;
    if !reliable_text(app, 512, false, true)
        || !reliable_text(record, 512, true, true)
        || b.get_peer_id() == 0
        || b.get_peer_id() == u64::MAX
    {
        return Err(reliable_error());
    }
    let host = reliable_array(b.get_host()?)?;
    let root = reliable_array(b.get_root()?)?;
    let device = reliable_array(b.get_device()?)?;
    let target_binding = reliable_array(b.get_target_binding()?)?;
    Ok(PublishedInvocationBinding {
        host,
        peer_id: b.get_peer_id(),
        app_id: app.to_owned(),
        record_id: record.to_owned(),
        root,
        device,
        target_binding,
    })
}
#[cfg(feature = "client")]
fn reliable_uuid_matches(text: &str, expected: &[u8; 16]) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let mut bytes = bytes.iter().copied();
    for (i, want) in expected.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) && bytes.next() != Some(b'-') {
            return false;
        }
        for nibble in [want >> 4, want & 15] {
            if bytes.next().map(char::from) != char::from_digit(u32::from(nibble), 16) {
                return false;
            }
        }
    }
    bytes.next().is_none()
}
#[cfg(feature = "client")]
fn reliable_decode_observation(
    o: collab_capnp::published_invocation_observation::Reader<'_>,
    host: &[u8; 16],
    invocation: &[u8; 16],
) -> Result<PublishedInvocationObservation, capnp::Error> {
    // Parse only the correlation field without copying the lifecycle tree.
    // The SDK still decodes and validates its own InvocationUpdate grammar.
    #[derive(serde::Deserialize)]
    struct Identity<'a> {
        #[serde(borrow)]
        invocation: &'a str,
    }
    // Preserve the existing 256 KiB outcome budget plus two bounded evidence
    // fields; adding evidence must not make a previously valid outcome unreadable.
    let size = o.total_size()?;
    if size.word_count > 321 * 1024 / 8 || size.cap_count != 0 {
        return Err(reliable_error());
    }
    if reliable_array::<16>(o.get_host()?)? != *host
        || reliable_array::<16>(o.get_invocation()?)? != *invocation
    {
        return Err(reliable_error());
    }
    let json = o.get_update_json()?.to_str()?;
    let admission_json = o.get_admission_json()?.to_str()?;
    let terminal_json = o.get_terminal_json()?.to_str()?;
    if !reliable_text(json, 256 * 1024, true, false)
        || !reliable_text(admission_json, 32 * 1024, true, false)
        || !reliable_text(terminal_json, 32 * 1024, true, false)
    {
        return Err(reliable_error());
    }
    let result = if o.get_found() {
        if o.get_id() == 0 {
            return Err(reliable_error());
        }
        let identity: Identity<'_> = serde_json::from_str(json).map_err(|_| reliable_error())?;
        if !reliable_uuid_matches(identity.invocation, invocation) {
            return Err(reliable_error());
        }
        Some(PublishedInvocationResult {
            id: o.get_id(),
            update_json: json.to_owned(),
            admission_json: (!admission_json.is_empty()).then(|| admission_json.to_owned()),
            terminal_json: (!terminal_json.is_empty()).then(|| terminal_json.to_owned()),
            uncertain: o.get_uncertain(),
            cancellation_requested: o.get_cancellation_requested(),
        })
    } else {
        if o.get_id() != 0
            || !json.is_empty()
            || !admission_json.is_empty()
            || !terminal_json.is_empty()
            || o.get_uncertain()
            || o.get_cancellation_requested()
        {
            return Err(reliable_error());
        }
        None
    };
    Ok(PublishedInvocationObservation {
        host: *host,
        invocation: *invocation,
        result,
    })
}

impl LocalDocumentActionReport {
    /// Bind the complete immutable report, except the invocation address itself.
    /// Generate `capture_id` once; finalize this address only after mutation hashes
    /// are known and preserve both identities across transport retries.
    /// # Errors
    /// Returns an error if the report cannot be encoded.
    pub fn bound_invocation(&self) -> Result<uuid::Uuid, String> {
        // Match the broker's journal request digest, including recursively sorted
        // object members even when serde_json's preserve_order feature is enabled.
        fn canonical(value: &serde_json::Value, out: &mut Vec<u8>) {
            match value {
                serde_json::Value::Object(fields) => {
                    let mut members: Vec<_> = fields.iter().collect();
                    members.compact_sort_by(|left, right| left.0.cmp(right.0));
                    out.push(b'{');
                    for (position, (name, member)) in members.into_iter().enumerate() {
                        if position > 0 {
                            out.push(b',');
                        }
                        canonical(&serde_json::Value::String(name.clone()), out);
                        out.push(b':');
                        canonical(member, out);
                    }
                    out.push(b'}');
                }
                serde_json::Value::Array(items) => {
                    out.push(b'[');
                    for (position, item) in items.iter().enumerate() {
                        if position > 0 {
                            out.push(b',');
                        }
                        canonical(item, out);
                    }
                    out.push(b']');
                }
                scalar => out.extend_from_slice(scalar.to_string().as_bytes()),
            }
        }
        let mut value = serde_json::to_value(self).map_err(|_| "local report encoding failed")?;
        value
            .as_object_mut()
            .ok_or("invalid local report")?
            .remove("invocation");
        let mut encoded = Vec::new();
        canonical(&value, &mut encoded);
        let mut request = blake3::Hasher::new_derive_key("tilo invocation journal request v1");
        request.update(&encoded);
        let mut hash = blake3::Hasher::new_derive_key("tilo local document capture identity v1");
        hash.update(request.finalize().as_bytes());
        let bytes = hash
            .finalize()
            .as_bytes()
            .first_chunk::<16>()
            .copied()
            .ok_or("invalid local report digest")?;
        Ok(uuid::Uuid::from_bytes(bytes))
    }

    /// Closed, bounded encoding shared with the authenticated local endpoint.
    /// Custody is recovery provenance, never caller-selected authority.
    /// # Errors
    /// Refuses invalid addresses, noncanonical hash sets and oversized reports.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.version != 1 || self.invocation.is_nil() || self.capture_id.is_nil() {
            return Err("invalid local document report version or invocation".into());
        }
        for field in [&self.revision, &self.resource, &self.action] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("invalid local document report address".into());
            }
        }
        for hashes in [&self.original_heads, &self.mutation_hashes] {
            if hashes.len() > 256
                || hashes
                    .windows(2)
                    .any(|pair| matches!(pair, [left, right] if left >= right))
            {
                return Err("local document report hashes must be bounded sorted sets".into());
            }
        }
        if self.encoded.type_id.is_empty()
            || self.encoded.type_id.len() > 4096
            || self.encoded.type_id.chars().any(char::is_control)
            || serde_json::from_slice::<serde_json::Value>(&self.encoded.payload).is_err()
        {
            return Err("invalid encoded local action report".into());
        }
        if self
            .argument
            .as_ref()
            .is_some_and(|argument| argument.len() > 64 * 1024)
        {
            return Err("local document report exceeds byte budget".into());
        }
        if let Some(group) = &self.group {
            for field in [&group.id, &group.label, &group.kind] {
                if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                    return Err("invalid local document report group".into());
                }
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > 64 * 1024 {
            return Err("local document report exceeds byte budget".into());
        }
        Ok(bytes)
    }
}

/// Explicit pre-admission rejection marker. Unmarked RPC errors may follow a
/// committed admission and must never release the caller's recovery custody.
pub const LOCAL_DOCUMENT_REJECTED: &str = "local-document-rejected: ";

/// Why a report on a route its connection is no worker on is refused:
/// nothing reported there on that connection can ever be admitted.
pub const LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING: &str =
    "this connection admits no local document reports";

/// The broker's marked rejection, under every Cap'n Proto hop's prefix.
#[must_use]
pub fn local_document_rejection(mut reason: &str) -> Option<&str> {
    while let Some(inner) = reason.strip_prefix("remote exception: ") {
        reason = inner;
    }
    reason.strip_prefix(LOCAL_DOCUMENT_REJECTED)
}

/// A rejection is definitive; an uncertain outcome must retain the exact UUID.
#[derive(Clone, Debug)]
pub enum LocalDocumentActionError {
    Rejected(String),
    Uncertain(String),
}

impl std::fmt::Display for LocalDocumentActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}

#[cfg(feature = "client")]
impl From<NotAvailable> for LocalDocumentActionError {
    fn from(error: NotAvailable) -> Self {
        Self::Uncertain(error.to_string())
    }
}

#[cfg(feature = "client")]
impl From<capnp::Error> for LocalDocumentActionError {
    fn from(error: capnp::Error) -> Self {
        Self::Uncertain(error.to_string())
    }
}

#[cfg(feature = "client")]
impl ChannelHandle {
    /// Report speculative local document work on this authenticated session.
    /// Admission is not execution, success, or `DocumentAuthorization`. No retry,
    /// fallback, fresh revision lookup or replacement invocation is synthesized.
    /// # Errors
    /// Refuses malformed reports, unsupported endpoints and uncorrelated replies.
    pub async fn admit_local_document_action(
        &self,
        report: &LocalDocumentActionReport,
    ) -> Result<PublishedInvocationObservation, LocalDocumentActionError> {
        let bytes = report
            .encode()
            .map_err(LocalDocumentActionError::Rejected)?;
        let mut request = self.inner.admit_local_document_action_request();
        request.get().set_request(&bytes);
        let reply = request.send().promise.await.map_err(|error| {
            if error.kind == capnp::ErrorKind::Failed
                && let Some(reason) = local_document_rejection(&error.extra)
            {
                return LocalDocumentActionError::Rejected(reason.to_owned());
            }
            LocalDocumentActionError::Uncertain(error.to_string())
        })?;
        let result = reply.get()?;
        reliable_size(result.total_size()?)?;
        let observation = result.get_observation()?;
        let host = reliable_array::<16>(observation.get_host()?)?;
        Ok(reliable_decode_observation(
            observation,
            &host,
            report.invocation.as_bytes(),
        )?)
    }

    /// Resolve a reliable target; older @35 responses with no binding refuse.
    /// The result is inert data; the server revalidates authority at admission.
    /// # Errors
    /// Refuses invalid inputs, non-Found, oversized, unbound or mismatched rows.
    pub async fn resolve_published_action_for_invocation(
        &self,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<(Box<ActionDirectoryRow>, PublishedInvocationBinding), NotAvailable> {
        if !reliable_address(peer_id, revision, resource, action) {
            return Err(reliable_error().into());
        }
        let mut request = self.inner.resolve_published_action_request();
        {
            let mut p = request.get();
            p.set_peer_id(peer_id);
            p.set_revision(revision);
            p.set_resource(resource);
            p.set_action(action);
        }
        let reply = request.send().promise.await?;
        let p = reply.get()?;
        reliable_size(p.total_size()?)?;
        if !p.has_invocation_binding() {
            return Err(reliable_error().into());
        }
        let b = p.get_invocation_binding()?;
        if b.get_peer_id() != peer_id {
            return Err(reliable_error().into());
        }
        let collab_capnp::action_directory_resolution::Which::Found(row) =
            p.get_resolution()?.which().map_err(capnp::Error::from)?
        else {
            return Err(reliable_error().into());
        };
        let row = row?;
        if row.get_resource()?.to_str().map_err(capnp::Error::from)? != resource
            || row.get_action()?.to_str().map_err(capnp::Error::from)? != action
            || !row.has_descriptor()
        {
            return Err(reliable_error().into());
        }
        let descriptor = row.get_descriptor()?;
        if descriptor
            .get_resource()?
            .to_str()
            .map_err(capnp::Error::from)?
            != resource
            || descriptor
                .get_action()?
                .to_str()
                .map_err(capnp::Error::from)?
                != action
        {
            return Err(reliable_error().into());
        }
        let binding = reliable_decode_binding(b)?;
        Ok((Box::new(decode_action_directory_row(row)?), binding))
    }
    /// Send the caller's original business ID. Caller persistence is required
    /// before sending; this wrapper does not create a durable sender outbox.
    /// No error triggers a fallback or generates another invocation identity.
    /// # Errors
    /// Refuses invalid inputs, unsupported servers and uncorrelated observations.
    pub async fn invoke_resolved_published_action_durable(
        &self,
        invocation: &[u8; 16],
        binding: &PublishedInvocationBinding,
        revision: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation_json: &str,
    ) -> Result<PublishedInvocationObservation, NotAvailable> {
        if *invocation == [0; 16]
            || !reliable_binding_valid(binding)
            || !reliable_address(binding.peer_id, revision, resource, action)
            || argument.is_some_and(|a| !reliable_text(a, 32 * 1024, true, false))
            || !reliable_text(confirmation_json, 4 * 1024, true, false)
        {
            return Err(reliable_error().into());
        }
        let mut request = self
            .inner
            .invoke_resolved_published_action_durable_request();
        {
            let mut p = request.get();
            p.set_invocation(invocation);
            p.set_revision(revision);
            p.set_resource(resource);
            p.set_action(action);
            p.set_argument(argument.unwrap_or(""));
            p.set_has_argument(argument.is_some());
            p.set_confirmation_json(confirmation_json);
            let mut b = p.init_binding();
            b.set_host(&binding.host);
            b.set_peer_id(binding.peer_id);
            b.set_app_id(&binding.app_id);
            b.set_record_id(&binding.record_id);
            b.set_root(&binding.root);
            b.set_device(&binding.device);
            b.set_target_binding(&binding.target_binding);
        }
        let reply = request.send().promise.await?;
        let p = reply.get()?;
        reliable_size(p.total_size()?)?;
        Ok(reliable_decode_observation(
            p.get_observation()?,
            &binding.host,
            invocation,
        )?)
    }
    /// Observe the original ID without requiring a live target or resending work.
    /// # Errors
    /// Refuses invalid IDs, unsupported servers and uncorrelated observations.
    pub async fn published_invocation_observation(
        &self,
        host: &[u8; 16],
        invocation: &[u8; 16],
    ) -> Result<PublishedInvocationObservation, NotAvailable> {
        if *host == [0; 16] || *invocation == [0; 16] {
            return Err(reliable_error().into());
        }
        let mut request = self.inner.published_invocation_observation_request();
        {
            let mut p = request.get();
            p.set_host(host);
            p.set_invocation(invocation);
        }
        let reply = request.send().promise.await?;
        let p = reply.get()?;
        reliable_size(p.total_size()?)?;
        Ok(reliable_decode_observation(
            p.get_observation()?,
            host,
            invocation,
        )?)
    }
    /// Request cancellation under current server authority for the original ID.
    /// # Errors
    /// Refuses invalid IDs, unsupported servers and uncorrelated observations.
    pub async fn cancel_durable_published_invocation(
        &self,
        host: &[u8; 16],
        invocation: &[u8; 16],
    ) -> Result<PublishedInvocationObservation, NotAvailable> {
        if *host == [0; 16] || *invocation == [0; 16] {
            return Err(reliable_error().into());
        }
        let mut request = self.inner.cancel_durable_published_invocation_request();
        {
            let mut p = request.get();
            p.set_host(host);
            p.set_invocation(invocation);
        }
        let reply = request.send().promise.await?;
        let p = reply.get()?;
        reliable_size(p.total_size()?)?;
        Ok(reliable_decode_observation(
            p.get_observation()?,
            host,
            invocation,
        )?)
    }
}

#[cfg(test)]
#[cfg(feature = "client")]
mod reliable_invocation_tests;

#[cfg(all(test, feature = "client"))]
mod local_document_action_transport_tests {
    use super::*;
    use std::cell::RefCell;

    fn report() -> LocalDocumentActionReport {
        LocalDocumentActionReport {
            custody: None,
            version: 1,
            invocation: uuid::Uuid::from_bytes([1; 16]),
            capture_id: uuid::Uuid::from_bytes([4; 16]),
            revision: "captured-revision".into(),
            resource: "document".into(),
            action: "edit".into(),
            argument: Some("private text".into()),
            encoded: tilo_actions::EncodedAction {
                type_id: "private-action-type".into(),
                payload: br#"{"text":"private encoded payload"}"#.to_vec(),
            },
            context: Some(tilo_actions::ActionContext {
                purpose: Some("private purpose".into()),
                source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                    [7; 16],
                ))),
                ..Default::default()
            }),
            original_heads: vec![[2; 32]],
            mutation_hashes: vec![[3; 32]],
            group: None,
        }
    }

    #[test]
    fn closed_report_is_bounded_and_debug_does_not_disclose_payload() {
        let mut value = report();
        let json: serde_json::Value = serde_json::from_slice(&value.encode().unwrap()).unwrap();
        let mut fields = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.compact_sort();
        assert_eq!(
            fields,
            vec![
                "action",
                "argument",
                "capture_id",
                "context",
                "custody",
                "encoded",
                "group",
                "invocation",
                "mutation_hashes",
                "original_heads",
                "resource",
                "revision",
                "version",
            ]
        );
        for forbidden in ["actor", "app", "record", "peer", "target"] {
            assert!(json.get(forbidden).is_none());
        }
        assert_eq!(json["invocation"], value.invocation.to_string());
        assert_eq!(json["capture_id"], value.capture_id.to_string());
        let mut missing_custody = json.clone();
        missing_custody.as_object_mut().unwrap().remove("custody");
        let legacy: LocalDocumentActionReport = serde_json::from_value(missing_custody).unwrap();
        assert!(legacy.custody.is_none());
        let mut missing_capture = json.clone();
        missing_capture
            .as_object_mut()
            .unwrap()
            .remove("capture_id");
        assert!(serde_json::from_value::<LocalDocumentActionReport>(missing_capture).is_err());
        let mut nil_capture = value.clone();
        nil_capture.capture_id = uuid::Uuid::nil();
        assert!(nil_capture.encode().is_err());
        let debug = format!("{value:?}");
        assert!(!debug.contains("private text"));
        assert!(!debug.contains("captured-revision"));
        assert!(!debug.contains("private-action-type"));
        assert!(!debug.contains("private encoded payload"));
        assert!(!debug.contains("private purpose"));
        assert!(!debug.contains("encoded"));
        assert!(!debug.contains("context"));
        let decoded: LocalDocumentActionReport = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.encoded, value.encoded);
        assert_eq!(decoded.context, value.context);
        assert_eq!(decoded.argument, value.argument);
        assert_eq!(decoded.encode().unwrap(), value.encode().unwrap());
        value.mutation_hashes.push([3; 32]);
        assert!(value.encode().is_err());
        value.mutation_hashes.clear();
        value.original_heads.clear();
        assert!(value.encode().is_ok());
        value.mutation_hashes.push([3; 32]);
        value.argument = Some("x".repeat(64 * 1024 + 1));
        assert!(value.encode().is_err());
    }

    struct Server {
        received: Rc<RefCell<Vec<Vec<u8>>>>,
        wrong_id: bool,
        refuse: bool,
    }
    impl collab_capnp::channel_handle::Server for Server {
        fn admit_local_document_action(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::AdmitLocalDocumentActionParams,
            mut results: collab_capnp::channel_handle::AdmitLocalDocumentActionResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            capnp::capability::Promise::from((|| {
                self.received
                    .borrow_mut()
                    .push(params.get()?.get_request()?.to_vec());
                if self.refuse {
                    let reason = if self.wrong_id {
                        "admission journal outcome unknown".to_owned()
                    } else {
                        format!("{LOCAL_DOCUMENT_REJECTED}local document policy refused")
                    };
                    return Err(capnp::Error::failed(reason));
                }
                let mut out = results.get().init_observation();
                out.set_host(&[9; 16]);
                out.set_invocation(if self.wrong_id { &[8; 16] } else { &[1; 16] });
                out.set_found(true);
                out.set_id(17);
                out.set_update_json(
                    r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#,
                );
                out.set_admission_json(r#"{"private":"admission"}"#);
                Ok(())
            })())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_report_uses_existing_session_and_preserves_private_observation() {
        let received = Rc::new(RefCell::new(Vec::new()));
        let captured = report();
        let expected = captured.encode().unwrap();
        crate::resolved_invocation_tests::over_wire(
            Server {
                received: received.clone(),
                wrong_id: false,
                refuse: false,
            },
            |channel| async move {
                let observed = channel
                    .admit_local_document_action(&captured)
                    .await
                    .unwrap();
                let result = observed.result.unwrap();
                assert_eq!(
                    result.admission_json.as_deref(),
                    Some(r#"{"private":"admission"}"#)
                );
                assert!(result.terminal_json.is_none());
            },
        )
        .await;
        assert_eq!(*received.borrow(), vec![expected]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refusal_or_wrong_invocation_never_retries_or_falls_back() {
        for (wrong_id, refuse) in [(true, false), (false, true), (true, true)] {
            let received = Rc::new(RefCell::new(Vec::new()));
            crate::resolved_invocation_tests::over_wire(
                Server {
                    received: received.clone(),
                    wrong_id,
                    refuse,
                },
                |channel| async move {
                    let error = channel
                        .admit_local_document_action(&report())
                        .await
                        .unwrap_err();
                    if refuse && !wrong_id {
                        assert!(matches!(error, LocalDocumentActionError::Rejected(_)));
                    } else {
                        assert!(matches!(error, LocalDocumentActionError::Uncertain(_)));
                    }
                },
            )
            .await;
            assert_eq!(received.borrow().len(), 1);
        }
    }
}
