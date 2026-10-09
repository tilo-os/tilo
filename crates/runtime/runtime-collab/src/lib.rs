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

pub mod file_clipboard;

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
/// The host distinguishes local UI intent from continuation of a published action.
/// A published id is resolved only against broker-retained original custody.
#[derive(Clone, Copy, Debug)]
pub enum DisplayOrigin {
    LocalHost,
    Published([u8; 16]),
}

mod action_directory;
pub use action_directory::*;
pub mod audio_output;
pub mod committed_document;
#[cfg(feature = "client")]
pub mod live_activity;
pub mod native_media;
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
        })
    }
}

/// One published application action a leased task asks to run: the app
/// peer that publishes it, its resource and action, and the argument it
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskAction<'a> {
    pub peer_id: u64,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
}

/// Outcome of [`ChannelHandle::invoke_task_step`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStepReply {
    /// The host committed this step: the exact child observation and the
    /// parent's committed next-step continuation.
    Committed {
        update_json: String,
        continuation: Vec<u8>,
    },
    /// The step is bound but not settled yet, or its reply was lost. Re-send
    /// the identical step; never start a new invocation for it.
    Retry,
}

/// Whether a failed step call left its outcome uncertain but recoverable by
/// re-sending the same step: the broker's explicit "pending" reply, or a
/// connection lost after the request may have been bound.
#[cfg(feature = "client")]
fn task_step_retryable(error: &capnp::Error) -> bool {
    error.kind == capnp::ErrorKind::Disconnected || error.extra.contains("retry the same step")
}

/// A subscribed-to channel. Holds a typed capability backed by the
/// broker; dropping the handle closes the subscription. Cheap to
/// clone (the underlying capability is reference-counted).
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct ChannelHandle {
    inner: Rc<collab_capnp::channel_handle::Client>,
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
impl ChannelHandle {
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

    /// Run a bounded local Files command under its original invocation, if any.
    /// Submit a local Files command on this admitted channel.
    ///
    /// # Errors
    /// Rejects oversized, malformed or unavailable commands and replies.
    pub async fn local_file_clipboard_command(
        &self,
        command: &file_clipboard::Command,
        parent: Option<[u8; 16]>,
    ) -> Result<file_clipboard::Reply, NotAvailable> {
        let failed = |message: String| NotAvailable::Capnp(capnp::Error::failed(message));
        if !command.valid() {
            return Err(failed("invalid local clipboard command".into()));
        }
        let bytes = serde_json::to_vec(command).map_err(|error| failed(error.to_string()))?;
        if bytes.len() > file_clipboard::MAX_WIRE_BYTES {
            return Err(failed("clipboard command exceeds bound".into()));
        }
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
        let failed = |message: String| NotAvailable::Capnp(capnp::Error::failed(message));
        if !command.valid() {
            return Err(failed("invalid speaker command".into()));
        }
        let command_json = serde_json::to_vec(command)
            .map_err(|error| failed(format!("encode speaker command: {error}")))?;
        if command_json.len() > MAX_SPEAKER_WIRE_BYTES {
            return Err(failed("speaker command exceeds wire bound".into()));
        }
        let mut request = self.inner.speaker_command_request();
        request.get().set_app_id(app_id);
        request.get().set_command_json(&command_json);
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply_json()?;
        if bytes.len() > MAX_SPEAKER_WIRE_BYTES {
            return Err(failed("speaker reply exceeds wire bound".into()));
        }
        let decoded: SpeakerReply = serde_json::from_slice(bytes)
            .map_err(|error| failed(format!("decode speaker reply: {error}")))?;
        if !decoded.valid() {
            return Err(failed("invalid speaker reply".into()));
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
            params.set_resource(target.resource);
            params.set_action(target.action);
            params.set_argument(target.argument.unwrap_or(""));
            params.set_has_argument(target.argument.is_some());
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

    /// Invoke one durable step of a leased enrolled task. The broker maps
    /// `step` to exactly one child before execution; a retry of the same step
    /// returns that child's committed result and the parent's committed
    /// next-step continuation instead of a new invocation.
    ///
    /// A step the broker reports as pending, or whose reply was lost with the
    /// connection, is [`TaskStepReply::Retry`]: re-send the identical step.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker refuses the step or its reply
    /// is malformed, including a reply without a committed continuation.
    pub async fn invoke_task_step(
        &self,
        run_id: u64,
        lease: &str,
        step: u32,
        target: TaskAction<'_>,
    ) -> Result<TaskStepReply, NotAvailable> {
        let mut request = self.inner.invoke_task_action_request();
        {
            let mut params = request.get();
            params.set_run_id(run_id);
            params.set_lease(lease);
            params.set_peer_id(target.peer_id);
            params.set_resource(target.resource);
            params.set_action(target.action);
            params.set_argument(target.argument.unwrap_or(""));
            params.set_has_argument(target.argument.is_some());
            params.set_confirmation_json("");
            params.set_step_index(step);
            params.set_has_step(true);
        }
        let reply = match request.send().promise.await {
            Ok(reply) => reply,
            Err(error) if task_step_retryable(&error) => return Ok(TaskStepReply::Retry),
            Err(error) => return Err(error.into()),
        };
        let result = reply.get()?;
        let update_json = result
            .get_update_json()?
            .to_str()
            .map_err(|error| capnp::Error::failed(format!("invalid task step update: {error}")))?
            .to_owned();
        let continuation = result.get_continuation()?.to_vec();
        if continuation.is_empty() {
            return Err(capnp::Error::failed("task step reply has no continuation".into()).into());
        }
        Ok(TaskStepReply::Committed {
            update_json,
            continuation,
        })
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
    pub run_id: u64,
    pub lease: String,
    pub planning: TaskRunPlanning,
    /// Only the broker may mint a policy from the native task owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
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
            execution_budget: None,
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
    /// `Some` only on a session-local handoff request addressed to this
    /// instance; the bytes are `ui::collaboration::handoff` JSON. Not a
    /// roster update either.
    pub handoff: Option<Vec<u8>>,
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
        let handoff = ctx
            .get_handoff()
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .map(<[u8]>::to_vec);
        Ok(ChannelContext {
            collaborators,
            permissions,
            follow_changed,
            snapshot_requested: ctx.get_snapshot_requested(),
            handoff,
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
    #[cfg(feature = "client")]
    #[test]
    fn action_semantic_identity_is_optional_bounded_and_kind_exact() {
        let decode = |identity: Option<(&str, &str)>| {
            let mut message = capnp::message::Builder::new_default();
            let mut row = message.init_root::<collab_capnp::exported_action::Builder<'_>>();
            row.set_resource("opaque-alias");
            row.set_resource_kind("store.app");
            row.set_resource_label("A misleading label");
            if let Some((kind, key)) = identity {
                let mut out = row.reborrow().init_semantic_identity();
                out.set_kind(kind);
                out.set_key(key);
            }
            decode_exported_action(
                message
                    .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
                    .unwrap(),
            )
        };
        assert!(decode(None).unwrap().semantic_identity.is_none());
        let decoded = decode(Some(("store.app", "app:so.tilo.chat"))).unwrap();
        assert_eq!(decoded.resource, "opaque-alias");
        assert_eq!(decoded.semantic_identity.unwrap().key, "app:so.tilo.chat");
        assert!(decode(Some(("person", "app:so.tilo.chat"))).is_err());
        assert!(decode(Some(("store.app", ""))).is_err());
        assert!(decode(Some(("store.app", "key\nsecret"))).is_err());
        assert!(decode(Some(("store.app", &"x".repeat(4097)))).is_err());
        assert!(decode(Some((&"x".repeat(129), "key"))).is_err());
        assert!(decode(Some(("store.app", &"x".repeat(4096)))).is_ok());
    }

    #[test]
    fn airdrop_extension_preserves_old_json_and_accepts_no_application_trust_claim() {
        use super::{NearbyTransferCommand as Command, NearbyTransferReply as Reply};
        assert_eq!(
            serde_json::to_string(&Command::Snapshot).unwrap(),
            r#"{"command":"snapshot"}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::Accepted).unwrap(),
            r#"{"result":"accepted"}"#
        );
        let command = r#"{"command":"send-air-drop","source_path":[47,116,109,112,47,102],"address":"127.0.0.1","port":443}"#;
        let parsed = serde_json::from_str::<Command>(command).unwrap();
        assert!(parsed.valid());
        for claimed in ["certificate_sha256", "verified", "person", "remember"] {
            let value = command.strip_suffix('}').unwrap().to_owned()
                + &format!(",\"{claimed}\":\"claimed\"}}");
            assert!(serde_json::from_str::<Command>(&value).is_err());
        }
        for address in ["receiver.example", "0.0.0.0", "239.1.1.1", "::"] {
            let mut value = serde_json::from_str::<serde_json::Value>(command).unwrap();
            value["address"] = address.into();
            assert!(!serde_json::from_value::<Command>(value).unwrap().valid());
        }
        let mut value = serde_json::from_str::<serde_json::Value>(command).unwrap();
        value["port"] = 0.into();
        assert!(!serde_json::from_value::<Command>(value).unwrap().valid());
    }
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
    fn nearby_transfer_contract_is_bounded_and_rejects_impossible_progress() {
        // phase: nearby_transfer_contract_is_bounded_and_path_bytes_are_opaque
        {
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
        // phase: nearby_transfer_snapshot_rejects_unbounded_or_impossible_progress
        {
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
    }

    #[test]
    fn wifi_continuity_is_bounded_redacted_and_bound_to_a_local_profile() {
        // phase: wifi_continuity_contract_is_bounded_and_redacts_secrets
        {
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
        // phase: saved_wifi_profile_reference_requires_an_exact_local_uuid
        {
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
    }

    #[test]
    fn action_directory_and_view_metadata_round_trip() {
        // phase: action_directory_resource_presentation_round_trips
        {
            let mut message = capnp::message::Builder::new_default();
            {
                let mut page =
                    message.init_root::<collab_capnp::action_directory_page::Builder<'_>>();
                page.set_revision("r1");
                page.set_exhausted(true);
                let mut rows = page.init_rows(1);
                let mut row = rows.reborrow().get(0);
                row.set_resource("totp:mail");
                row.set_resource_kind("totp");
                row.set_resource_label("Mail code");
                row.set_resource_presentation_json(
                    r#"{"detail":"expires in 18 s","badge":"438 291"}"#,
                );
            }
            let reader = message
                .get_root_as_reader::<collab_capnp::action_directory_page::Reader<'_>>()
                .expect("page reader");
            let decoded = decode_action_directory_page(reader).expect("page decodes");
            assert_eq!(
                decoded.rows[0].presentation,
                Some(tilo_actions::ResourcePresentation {
                    detail: Some("expires in 18 s".into()),
                    badge: Some("438 291".into()),
                    preview: None,
                })
            );
        }
        // phase: exported_view_metadata_round_trips
        {
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
                let mut range = view.reborrow().init_range();
                range.set_action("show-range");
                range.set_start_param("start");
                range.set_end_param("end");
                range.set_unit_param("unit");
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
                    range: Some(ExportedViewRange {
                        action: "show-range".into(),
                        start_param: "start".into(),
                        end_param: "end".into(),
                        unit_param: "unit".into()
                    }),
                    mount: Some(ExportedViewMount {
                        action: "mount-editor".into(),
                        capability_param: "slot_capability".into(),
                        view_param: "slot_view".into(),
                    }),
                })
            );
        }
    }
}

#[cfg(test)]
mod enrolled_tests {
    use super::*;
    fn envelope() -> EnrolledTaskEnvelope {
        EnrolledTaskEnvelope {
            version: 1,
            message_id: "message-1".into(),
            task_id: "task-1".into(),
            run_id: "run-1".into(),
            launch_id: "launch-1".into(),
            runtime_epoch: "epoch-1".into(),
            recipient_agent: "agent-1".into(),
            channel_lease: "channel-1".into(),
            history: None,
            task: TaskRunEnvelope {
                version: 1,
                task_id: "task-1".into(),
                title: "Private instruction".into(),
                run_id: 9,
                lease: "lease-1".into(),
                planning: TaskRunPlanning::Model,
                execution_budget: None,
                action_context: serde_json::json!({}),
            },
        }
    }
    #[test]
    fn enrolled_recipient_retains_exact_acceptance_and_never_reexecutes_duplicate() {
        let first = envelope();
        let mut custody =
            EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
        let accepted = custody.accept(&first).unwrap();
        assert!(accepted.execute);
        assert_eq!(accepted.acceptance.acceptance_sequence, 1);
        let duplicate = custody.accept(&first).unwrap();
        assert!(!duplicate.execute);
        assert_eq!(accepted.acceptance, duplicate.acceptance);
        let mut changed = first.clone();
        changed.task.title = "Different private instruction".into();
        assert!(custody.accept(&changed).is_err());
        changed = first.clone();
        changed.channel_lease = "other-channel".into();
        assert!(custody.accept(&changed).is_err());
        changed = first.clone();
        changed.task.task_id = "other-task".into();
        assert!(custody.accept(&changed).is_err());
    }
    #[test]
    fn enrolled_budget_is_bounded_and_same_message_cannot_rewrite_it() {
        use collab_types::task_budget::TaskExecutionBudget;
        let mut first = envelope();
        assert!(
            serde_json::to_value(&first).unwrap()["task"]
                .get("execution_budget")
                .is_none()
        );
        first.task.execution_budget = Some(TaskExecutionBudget {
            turn_ms: 900_000,
            total_ms: 7_200_000,
        });
        let mut custody =
            EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
        assert!(custody.accept(&first).unwrap().execute);
        assert!(!custody.accept(&first).unwrap().execute);
        let mut changed = first.clone();
        changed.task.execution_budget = Some(TaskExecutionBudget {
            turn_ms: 1,
            total_ms: 1,
        });
        assert!(custody.accept(&changed).is_err());
        changed.task.execution_budget = Some(TaskExecutionBudget {
            turn_ms: 900_001,
            total_ms: 7_200_000,
        });
        assert!(changed.validate().is_err());
        assert!(custody.accept(&changed).is_err());
    }

    #[test]
    fn enrolled_full_private_input_and_retention_bounds_fail_closed() {
        let mut first = envelope();
        first.task.title = "𐐀".repeat(8000);
        assert!(first.validate().is_ok());
        first.task.title.push('x');
        assert!(first.validate().is_err());
        let mut custody =
            EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
        let original = envelope();
        let accepted = custody.accept(&original).unwrap();
        let mut refused = false;
        for index in 2..=1025 {
            let mut next = envelope();
            next.message_id = format!("message-{index}");
            next.task_id = format!("task-{index}");
            next.task.task_id = next.task_id.clone();
            next.run_id = format!("run-{index}");
            next.task.run_id = index;
            next.task.title = "𐐀".repeat(8000);
            if custody.accept(&next).is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused, "retained byte budget must refuse new work");
        let replay = custody.accept(&original).unwrap();
        assert!(!replay.execute);
        assert_eq!(replay.acceptance, accepted.acceptance);
    }
    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn enrolled_old_peer_cannot_acknowledge_delivery_from_queue_success() {
        struct OldPeer;
        impl collab_capnp::channel_handle::Server for OldPeer {}
        let channel = ChannelHandle {
            inner: Rc::new(capnp_rpc::new_client(OldPeer)),
        };
        let first = envelope();
        assert!(channel.subscribe_enrolled_tasks().await.is_err());
        assert!(
            channel
                .accept_enrolled_task(&first.acceptance(1))
                .await
                .is_err()
        );
        assert!(
            channel
                .report_enrolled_task(
                    &first.acceptance(1),
                    1,
                    &TaskRunUpdate::Succeeded {
                        message: "done".into()
                    }
                )
                .await
                .is_err()
        );
    }
    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn enrolled_report_preserves_full_private_result_and_bounds_encoded_frame() {
        struct Recipient(Rc<std::cell::Cell<usize>>);
        impl collab_capnp::channel_handle::Server for Recipient {
            fn report_enrolled_task(
                self: Rc<Self>,
                params: collab_capnp::channel_handle::ReportEnrolledTaskParams,
                _results: collab_capnp::channel_handle::ReportEnrolledTaskResults,
            ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
                let result = (|| {
                    let request = params.get()?;
                    let update: TaskRunUpdate =
                        serde_json::from_str(request.get_update_json()?.to_str()?)
                            .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    match update {
                        TaskRunUpdate::Succeeded { message } => {
                            assert_eq!(message.chars().count(), 8000);
                        }
                        _ => panic!("unexpected update"),
                    }
                    self.0.set(self.0.get() + 1);
                    Ok(())
                })();
                capnp::capability::Promise::from(result)
            }
        }
        let calls = Rc::new(std::cell::Cell::new(0));
        let channel = ChannelHandle {
            inner: Rc::new(capnp_rpc::new_client(Recipient(calls.clone()))),
        };
        let acceptance = envelope().acceptance(1);
        for message in ["𐐀".repeat(8000), "\u{0001}".repeat(8000)] {
            channel
                .report_enrolled_task(&acceptance, 1, &TaskRunUpdate::Succeeded { message })
                .await
                .unwrap();
        }
        assert_eq!(calls.get(), 2);
        assert!(
            channel
                .report_enrolled_task(
                    &acceptance,
                    2,
                    &TaskRunUpdate::Succeeded {
                        message: "x".repeat(64 * 1024)
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(
            calls.get(),
            2,
            "oversized encoded update must not reach the peer"
        );
    }
    #[test]
    fn enrolled_protocol_rejects_unknown_fields_and_preserves_legacy_literal() {
        let legacy = r#"{"version":1,"task_id":"task-1","title":"Private instruction","run_id":9,"lease":"lease-1","planning":"model","action_context":{}}"#;
        let task: TaskRunEnvelope = serde_json::from_str(legacy).unwrap();
        assert_eq!(task, envelope().task);
        assert_eq!(serde_json::to_string(&task).unwrap(), legacy);
        let mut value = serde_json::to_value(envelope()).unwrap();
        value["credential"] = serde_json::json!("not permitted");
        assert!(serde_json::from_value::<EnrolledTaskEnvelope>(value).is_err());
    }
}

/// Private managed delivery, independently versioned from the legacy task transport.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledTaskEnvelope {
    pub version: u16,
    pub message_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub runtime_epoch: String,
    pub recipient_agent: String,
    pub channel_lease: String,
    pub task: TaskRunEnvelope,
    /// The recipient agent's own earlier tasks (requests, steps, outcomes,
    /// replies) as compact JSON, composed by the host from its durable
    /// records. Private to that agent, display input only, never authority.
    /// Absent on older hosts; omitted from the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
}
/// Most bytes of [`EnrolledTaskEnvelope::history`].
pub const MAX_ENROLLED_HISTORY_BYTES: usize = 12 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledTaskAcceptance {
    pub version: u16,
    pub message_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub runtime_epoch: String,
    pub recipient_agent: String,
    pub channel_lease: String,
    pub acceptance_sequence: u64,
}
impl EnrolledTaskEnvelope {
    pub const MAX_JSON_BYTES: usize = 128 * 1024;
    /// Check the envelope's version, labels and bounds.
    ///
    /// # Errors
    ///
    /// Returns a description of the first violated constraint.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.task.version != 1
            || self.task.run_id == 0
            || self.task.task_id != self.task_id
            || self.task.planning != TaskRunPlanning::Model
            || self.task.title.is_empty()
            || self.task.title.len() > 32 * 1024
            || self.task.title.chars().count() > 8000
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.len() > MAX_ENROLLED_HISTORY_BYTES)
            || !enrolled_label(&self.task.lease)
            || [
                &self.message_id,
                &self.task_id,
                &self.run_id,
                &self.launch_id,
                &self.runtime_epoch,
                &self.recipient_agent,
                &self.channel_lease,
            ]
            .iter()
            .any(|s| !enrolled_label(s))
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > Self::MAX_JSON_BYTES
        {
            return Err("invalid enrolled task envelope".into());
        }
        if let Some(budget) = self.task.execution_budget {
            budget.validate().map_err(str::to_owned)?;
        }
        Ok(())
    }
    fn acceptance(&self, sequence: u64) -> EnrolledTaskAcceptance {
        EnrolledTaskAcceptance {
            version: 1,
            message_id: self.message_id.clone(),
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            launch_id: self.launch_id.clone(),
            runtime_epoch: self.runtime_epoch.clone(),
            recipient_agent: self.recipient_agent.clone(),
            channel_lease: self.channel_lease.clone(),
            acceptance_sequence: sequence,
        }
    }
}
fn enrolled_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
impl EnrolledTaskAcceptance {
    pub fn matches(&self, envelope: &EnrolledTaskEnvelope) -> bool {
        self.acceptance_sequence != 0 && *self == envelope.acceptance(self.acceptance_sequence)
    }
}
/// Process-lifetime recipient deduplication. No eviction, persistence import or replay permission.
/// The host must retain the matching live process/channel custody; after restart it cannot resend.
pub struct EnrolledRecipientCustody {
    recipient: [String; 4],
    retained_bytes: usize,
    retained: std::collections::HashMap<String, (EnrolledTaskEnvelope, EnrolledTaskAcceptance)>,
}
pub struct EnrolledAcceptanceDecision {
    pub acceptance: EnrolledTaskAcceptance,
    /// True once, before the caller waits for the host's actual acceptance RPC reply.
    /// An ambiguous reply ends the task channel; it never rolls this reservation back.
    pub execute: bool,
}
impl EnrolledRecipientCustody {
    pub const CAPACITY: usize = 1024;
    /// Custody for one exact enrolled recipient.
    ///
    /// # Errors
    ///
    /// Refuses a recipient with an invalid label.
    pub fn new(agent: &str, launch: &str, epoch: &str, channel: &str) -> Result<Self, String> {
        let recipient = [agent, launch, epoch, channel].map(str::to_owned);
        if recipient.iter().any(|s| !enrolled_label(s)) {
            return Err("invalid enrolled recipient".into());
        }
        Ok(Self {
            recipient,
            retained_bytes: 0,
            retained: std::collections::HashMap::default(),
        })
    }
    /// Accept an envelope once, replaying the retained acceptance for an exact retry.
    ///
    /// # Errors
    ///
    /// Refuses invalid, misaddressed, changed or reused envelopes and full retention.
    pub fn accept(
        &mut self,
        envelope: &EnrolledTaskEnvelope,
    ) -> Result<EnrolledAcceptanceDecision, String> {
        envelope.validate()?;
        if self.recipient
            != [
                envelope.recipient_agent.clone(),
                envelope.launch_id.clone(),
                envelope.runtime_epoch.clone(),
                envelope.channel_lease.clone(),
            ]
        {
            return Err("wrong enrolled recipient custody".into());
        }
        if let Some((prior, acceptance)) = self.retained.get(&envelope.message_id) {
            if prior != envelope {
                return Err("enrolled message changed after acceptance".into());
            }
            return Ok(EnrolledAcceptanceDecision {
                acceptance: acceptance.clone(),
                execute: false,
            });
        }
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| e.to_string())?
            .len();
        let retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or("enrolled retention overflow")?;
        if self.retained.len() >= Self::CAPACITY || retained_bytes > 8 * 1024 * 1024 {
            return Err("enrolled acceptance retention full".into());
        }
        if self.retained.values().any(|(prior, _)| {
            prior.task_id == envelope.task_id
                || prior.run_id == envelope.run_id
                || prior.task.run_id == envelope.task.run_id
        }) {
            return Err("enrolled task/run reused for another message".into());
        }
        let sequence = u64::try_from(self.retained.len())
            .map_err(|_| "acceptance sequence overflow")?
            .checked_add(1)
            .ok_or("acceptance sequence overflow")?;
        let acceptance = envelope.acceptance(sequence);
        self.retained_bytes = retained_bytes;
        self.retained.insert(
            envelope.message_id.clone(),
            (envelope.clone(), acceptance.clone()),
        );
        Ok(EnrolledAcceptanceDecision {
            acceptance,
            execute: true,
        })
    }
}

#[cfg(feature = "client")]
#[derive(Clone)]
pub struct EnrolledTaskStream {
    inner: Rc<collab_capnp::enrolled_task_stream::Client>,
}
#[cfg(feature = "client")]
impl EnrolledTaskStream {
    /// Read the next enrolled task.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the stream fails or delivers an oversized or invalid envelope.
    pub async fn read(&self) -> Result<EnrolledTaskEnvelope, NotAvailable> {
        let reply = self.inner.read_request().send().promise.await?;
        let json = reply
            .get()?
            .get_task_json()?
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        if json.len() > EnrolledTaskEnvelope::MAX_JSON_BYTES {
            return Err(capnp::Error::failed("enrolled task too large".into()).into());
        }
        let envelope: EnrolledTaskEnvelope =
            serde_json::from_str(json).map_err(|e| capnp::Error::failed(e.to_string()))?;
        envelope.validate().map_err(capnp::Error::failed)?;
        Ok(envelope)
    }
}
#[cfg(feature = "client")]
impl ChannelHandle {
    /// Subscribe to this channel's enrolled tasks.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or drops during the call.
    pub async fn subscribe_enrolled_tasks(&self) -> Result<EnrolledTaskStream, NotAvailable> {
        let reply = self
            .inner
            .subscribe_enrolled_tasks_request()
            .send()
            .promise
            .await?;
        Ok(EnrolledTaskStream {
            inner: Rc::new(reply.get()?.get_stream()?),
        })
    }
    /// Report an enrolled task's acceptance to the host.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] for an oversized acceptance or a failed broker call.
    pub async fn accept_enrolled_task(
        &self,
        acceptance: &EnrolledTaskAcceptance,
    ) -> Result<(), NotAvailable> {
        let json =
            serde_json::to_string(acceptance).map_err(|e| capnp::Error::failed(e.to_string()))?;
        if json.len() > 4096 {
            return Err(capnp::Error::failed("enrolled acceptance too large".into()).into());
        }
        let mut request = self.inner.accept_enrolled_task_request();
        request.get().set_acceptance_json(&json);
        request.send().promise.await?;
        Ok(())
    }
    /// Report an accepted enrolled task's run update.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] for an invalid or oversized update or a failed broker call.
    pub async fn report_enrolled_task(
        &self,
        acceptance: &EnrolledTaskAcceptance,
        sequence: u64,
        update: &TaskRunUpdate,
    ) -> Result<(), NotAvailable> {
        let acceptance =
            serde_json::to_string(acceptance).map_err(|e| capnp::Error::failed(e.to_string()))?;
        let update =
            serde_json::to_string(update).map_err(|e| capnp::Error::failed(e.to_string()))?;
        if acceptance.len() > 4096 || update.len() > 64 * 1024 || sequence == 0 {
            return Err(capnp::Error::failed("invalid enrolled update".into()).into());
        }
        let mut request = self.inner.report_enrolled_task_request();
        request.get().set_acceptance_json(&acceptance);
        request.get().set_sequence(sequence);
        request.get().set_update_json(&update);
        request.send().promise.await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "client"))]
mod resolved_invocation_tests {
    use super::*;

    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, twoparty};
    use std::cell::{Cell, RefCell};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    #[derive(Debug, PartialEq, Eq)]
    struct Received {
        peer: u64,
        revision: String,
        resource: String,
        action: String,
        argument: Option<String>,
        confirmation: String,
    }

    struct ResolvedServer(Rc<RefCell<Vec<Received>>>);
    impl collab_capnp::channel_handle::Server for ResolvedServer {
        fn invoke_resolved_published_action(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::InvokeResolvedPublishedActionParams,
            mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                let p = params.get()?;
                self.0.borrow_mut().push(Received {
                    peer: p.get_peer_id(),
                    revision: p.get_revision()?.to_str()?.to_owned(),
                    resource: p.get_resource()?.to_str()?.to_owned(),
                    action: p.get_action()?.to_str()?.to_owned(),
                    argument: p
                        .get_has_argument()
                        .then(|| Ok::<String, capnp::Error>(p.get_argument()?.to_str()?.to_owned()))
                        .transpose()?,
                    confirmation: p.get_confirmation_json()?.to_str()?.to_owned(),
                });
                let mut out = results.get();
                out.set_id(987);
                out.set_update_json(r#"{"state":"accepted"}"#);
                Ok(())
            })())
        }
    }

    // The server owns only the old eager method. The generated unsupported
    // method returns the same Unimplemented category an older dispatch does.
    struct LegacyServer(Rc<Cell<usize>>);
    impl collab_capnp::channel_handle::Server for LegacyServer {
        fn invoke_published_action(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::InvokePublishedActionParams,
            mut results: collab_capnp::channel_handle::InvokePublishedActionResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                let p = params.get()?;
                assert_eq!(p.get_peer_id(), 9);
                assert_eq!(p.get_resource()?.to_str()?, "r");
                assert_eq!(p.get_action()?.to_str()?, "a");
                assert!(!p.get_has_argument());
                self.0.set(self.0.get().saturating_add(1));
                results.get().set_id(21);
                Ok(())
            })())
        }
    }

    async fn over_wire<S, F, Fut>(server: S, action: F)
    where
        S: collab_capnp::channel_handle::Server + 'static,
        F: FnOnce(ChannelHandle) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_io, client_io) = tokio::io::duplex(512);
                let raw: collab_capnp::channel_handle::Client = capnp_rpc::new_client(server);
                let (read, write) = tokio::io::split(server_io);
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    capnp_rpc::rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let task =
                    tokio::task::spawn_local(RpcSystem::new(Box::new(network), Some(raw.client)));
                let connection = runtime_portal::CapnpClient::from_connected_io(
                    client_io,
                    capnp_rpc::rpc_twoparty_capnp::Side::Client,
                );
                let channel = ChannelHandle {
                    inner: Rc::new(connection.bootstrap()),
                };
                tilo_test_env::within("the RPC under test", action(channel)).await;
                task.abort();
                let _ = task.await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolved_invocation_preserves_exact_revision_and_optional_argument_over_wire() {
        let received = Rc::new(RefCell::new(Vec::new()));
        over_wire(ResolvedServer(received.clone()), |channel| async move {
            for argument in [None, Some(""), Some("exact argument")] {
                assert_eq!(
                    channel
                        .invoke_resolved_published_action(
                            42,
                            "revision-7",
                            "notes.block:p:b",
                            "open:hash",
                            argument,
                            "{\"approved\":true}"
                        )
                        .await
                        .unwrap(),
                    (987, r#"{"state":"accepted"}"#.into())
                );
            }
        })
        .await;
        let received = received.borrow();
        assert_eq!(received.len(), 3);
        for (row, argument) in received
            .iter()
            .zip([None, Some(""), Some("exact argument")])
        {
            assert_eq!(
                *row,
                Received {
                    peer: 42,
                    revision: "revision-7".into(),
                    resource: "notes.block:p:b".into(),
                    action: "open:hash".into(),
                    argument: argument.map(str::to_owned),
                    confirmation: "{\"approved\":true}".into()
                }
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolved_invocation_accepts_exact_byte_limits() {
        let received = Rc::new(RefCell::new(Vec::new()));
        over_wire(ResolvedServer(received.clone()), |channel| async move {
            channel
                .invoke_resolved_published_action(
                    9,
                    &"é".repeat(256),
                    &"r".repeat(512),
                    &"a".repeat(512),
                    Some(&"é".repeat(16 * 1024)),
                    &" ".repeat(4 * 1024),
                )
                .await
                .unwrap();
        })
        .await;
        let rows = received.borrow();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].revision.len(), 512);
        assert_eq!(rows[0].resource.len(), 512);
        assert_eq!(rows[0].action.len(), 512);
        assert_eq!(rows[0].argument.as_ref().unwrap().len(), 32 * 1024);
        assert_eq!(rows[0].confirmation.len(), 4 * 1024);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolved_invocation_refuses_invalid_input_before_rpc() {
        let received = Rc::new(RefCell::new(Vec::new()));
        over_wire(ResolvedServer(received.clone()), |channel| async move {
            for (field, invalid) in [
                (0, String::new()),
                (0, "x".repeat(513)),
                (0, "revision\0tail".into()),
                (1, String::new()),
                (1, "r".repeat(513)),
                (1, "r\n".into()),
                (1, "r\0".into()),
                (2, String::new()),
                (2, "a".repeat(513)),
                (2, "a\u{7f}".into()),
                (2, "a\0".into()),
                (3, "x".repeat(32769)),
                (3, "é".repeat(16385)),
                (3, "arg\0tail".into()),
                (4, "x".repeat(4097)),
                (4, "{}\0".into()),
            ] {
                let mut args = ["revision", "r", "a", "argument", "{}"];
                args[field] = &invalid;
                assert!(
                    channel
                        .invoke_resolved_published_action(
                            9,
                            args[0],
                            args[1],
                            args[2],
                            Some(args[3]),
                            args[4]
                        )
                        .await
                        .is_err(),
                    "invalid field {field} reached RPC"
                );
            }
        })
        .await;
        assert!(
            received.borrow().is_empty(),
            "rejected inputs were transmitted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolved_invocation_old_server_is_unimplemented_without_eager_fallback() {
        let eager_calls = Rc::new(Cell::new(0));
        let seen = eager_calls.clone();
        over_wire(LegacyServer(eager_calls), |channel| async move {
            assert_eq!(channel.invoke_published_action(9, "r", "a", None, "{}").await.unwrap(), (21, String::new()));
            assert_eq!(seen.get(), 1);
            let error = channel.invoke_resolved_published_action(9, "revision", "r", "a", None, "{}").await.unwrap_err();
            assert!(matches!(error, NotAvailable::Capnp(error) if error.kind == capnp::ErrorKind::Unimplemented));
            assert_eq!(seen.get(), 1, "unsupported revisioned call fell back to eager invocation");
        }).await;
    }

    #[test]
    fn resolved_invocation_keeps_legacy_eager_parameter_bytes() {
        // Literal pre-@47 @21 parameters: peer9, r, a, present empty argument,
        // confirmation {}. Two data words and four pointer words are unchanged.
        let golden: &[u8] = &[
            0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 2, 0, 4, 0, 9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
            0, 0, 0, 13, 0, 0, 0, 18, 0, 0, 0, 13, 0, 0, 0, 18, 0, 0, 0, 13, 0, 0, 0, 10, 0, 0, 0,
            13, 0, 0, 0, 26, 0, 0, 0, b'r', 0, 0, 0, 0, 0, 0, 0, b'a', 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, b'{', b'}', 0, 0, 0, 0, 0, 0,
        ];
        let decoded = capnp::serialize::read_message(
            &mut &golden[..],
            capnp::message::ReaderOptions::default(),
        )
        .unwrap();
        let old = decoded
            .get_root::<collab_capnp::channel_handle::invoke_published_action_params::Reader<'_>>()
            .unwrap();
        assert_eq!(old.get_peer_id(), 9);
        assert_eq!(old.get_resource().unwrap().to_str().unwrap(), "r");
        assert_eq!(old.get_action().unwrap().to_str().unwrap(), "a");
        assert!(old.get_has_argument());
        assert_eq!(old.get_argument().unwrap().to_str().unwrap(), "");
        assert_eq!(old.get_confirmation_json().unwrap().to_str().unwrap(), "{}");
        let mut encoded = capnp::message::Builder::new_default();
        let mut p = encoded
            .init_root::<collab_capnp::channel_handle::invoke_published_action_params::Builder<'_>>(
            );
        p.set_peer_id(9);
        p.set_resource("r");
        p.set_action("a");
        p.set_argument("");
        p.set_has_argument(true);
        p.set_confirmation_json("{}");
        assert_eq!(capnp::serialize::write_message_to_words(&encoded), golden);
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedInvocationResult {
    pub id: u64,
    pub update_json: String,
    pub uncertain: bool,
    pub cancellation_requested: bool,
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
    reliable_size(o.total_size()?)?;
    if reliable_array::<16>(o.get_host()?)? != *host
        || reliable_array::<16>(o.get_invocation()?)? != *invocation
    {
        return Err(reliable_error());
    }
    let json = o.get_update_json()?.to_str()?;
    if !reliable_text(json, 256 * 1024, true, false) {
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
            uncertain: o.get_uncertain(),
            cancellation_requested: o.get_cancellation_requested(),
        })
    } else {
        if o.get_id() != 0
            || !json.is_empty()
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

#[cfg(feature = "client")]
impl ChannelHandle {
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

#[cfg(all(test, feature = "client"))]
mod reliable_invocation_tests {
    use super::*;

    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, twoparty};
    use std::cell::{Cell, RefCell};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    const HOST: [u8; 16] = [2; 16];
    const ID: [u8; 16] = [1; 16];
    const UPDATE: &str =
        r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#;
    fn binding() -> PublishedInvocationBinding {
        PublishedInvocationBinding {
            host: HOST,
            peer_id: 9,
            app_id: "notes".into(),
            record_id: String::new(),
            root: [3; 32],
            device: [4; 32],
            target_binding: [5; 32],
        }
    }
    fn write_binding(mut b: collab_capnp::published_invocation_binding::Builder<'_>) {
        b.set_host(&HOST);
        b.set_peer_id(9);
        b.set_app_id("notes");
        b.set_record_id("");
        b.set_root(&[3; 32]);
        b.set_device(&[4; 32]);
        b.set_target_binding(&[5; 32]);
    }
    /// A complete exact row for `r`/`a`, as the broker writes one: the
    /// decoder refuses a descriptor without its operation kind and contract.
    fn row(r: collab_capnp::action_directory_row::Builder<'_>) {
        let contract = tilo_actions::ActionContract::view();
        let descriptor = ExportedAction {
            resource: "r".into(),
            semantic_identity: None,
            action: "a".into(),
            resource_kind: "note".into(),
            resource_label: "Note".into(),
            action_label: "Open".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: contract.clone(),
            view: None,
            input: ExportedInput::None,
        };
        write_action_directory_row(
            r,
            &ActionDirectoryRow {
                parent_resource: String::new(),
                resource: "r".into(),
                resource_kind: "note".into(),
                resource_label: "Note".into(),
                presentation: None,
                action: Some("a".into()),
                descriptor: Some(descriptor),
                has_children: false,
                visibility: Some(RowVisibility {
                    action: "a".into(),
                    contract,
                }),
            },
        )
        .unwrap();
    }
    fn observation(mut o: collab_capnp::published_invocation_observation::Builder<'_>, mode: u8) {
        o.set_host(&HOST);
        o.set_invocation(&ID);
        o.set_found(true);
        o.set_id(99);
        o.set_update_json(UPDATE);
        match mode {
            1 => o.set_host(&[8;16]),
            2 => o.set_invocation(&[8;16]),
            3 => o.set_host(&[2;15]),
            4 => o.set_invocation(&[1;17]),
            5 => o.set_update_json(r#"{"state":"accepted","invocation":"08080808-0808-0808-0808-080808080808"}"#),
            6 => o.set_update_json("not json"),
            7 => o.set_update_json("{}"),
            8 => o.set_found(false),
            9 => {o.set_found(false);o.set_id(0);o.set_update_json("");o.set_uncertain(true);},
            10 => {o.set_found(false);o.set_id(0);o.set_update_json("");o.set_cancellation_requested(true);},
            11 => o.set_update_json("x".repeat(256*1024+1)),
            12 => o.set_update_json(""),
            13 => o.set_update_json(r#"{"invocation":"01010101-0101-0101-0101-010101010101","invocation":"01010101-0101-0101-0101-010101010101"}"#),
            14 => {o.set_found(false);o.set_id(0);o.set_update_json("");},
            15 => {o.set_uncertain(true);o.set_cancellation_requested(true);},
            16 => o.set_host(&[0;16]),
            17 => o.set_invocation(&[0;16]),
            18 => o.init_update_json(1).as_bytes_mut()[0]=0xff,
            19 => o.set_update_json(r#"{"invocation":"01010101010101010101010101010101"}"#),
            20 => o.set_update_json(format!(r#"{{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101","padding":"{}"}}"#,"x".repeat(240*1024))),
            21 => o.set_id(0),
            _ => {}
        }
    }
    type Received = Rc<RefCell<Vec<(String, String, String, Option<String>, String)>>>;
    struct Server {
        calls: Rc<Cell<usize>>,
        mode: u8,
        received: Received,
    }
    impl collab_capnp::channel_handle::Server for Server {
        fn resolve_published_action(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::ResolvePublishedActionParams,
            mut results: collab_capnp::channel_handle::ResolvePublishedActionResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                self.calls.set(self.calls.get().saturating_add(1));
                let p = params.get()?;
                assert_eq!(p.get_peer_id(), 9);
                row(results.get().init_resolution().init_found());
                if self.mode != 1 {
                    write_binding(results.get().init_invocation_binding());
                }
                match self.mode {
                    2 => results.get().get_invocation_binding()?.set_host(&[2; 15]),
                    3 => results.get().get_invocation_binding()?.set_peer_id(10),
                    4 => results
                        .get()
                        .get_invocation_binding()?
                        .set_app_id("x".repeat(513)),
                    5 => results.get().get_invocation_binding()?.set_root(&[0; 32]),
                    6 => results.get().init_resolution().set_not_found(()),
                    7 => results
                        .get()
                        .get_invocation_binding()?
                        .set_target_binding(&[0; 32]),
                    8 => results
                        .get()
                        .init_resolution()
                        .init_found()
                        .set_resource("other"),
                    9 => results
                        .get()
                        .init_resolution()
                        .init_found()
                        .set_resource_label("x".repeat(256 * 1024)),
                    10 => results.get().get_invocation_binding()?.set_root(&[3; 31]),
                    11 => results.get().get_invocation_binding()?.set_device(&[4; 33]),
                    12 => results
                        .get()
                        .get_invocation_binding()?
                        .set_target_binding(&[5; 31]),
                    13 => results
                        .get()
                        .get_invocation_binding()?
                        .set_app_id("notes\n"),
                    14 => results
                        .get()
                        .get_invocation_binding()?
                        .set_record_id("r".repeat(513)),
                    15 => results.get().get_invocation_binding()?.set_host(&[0; 16]),
                    _ => {}
                }
                Ok(())
            })())
        }
        fn invoke_resolved_published_action_durable(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::InvokeResolvedPublishedActionDurableParams,
            mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionDurableResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                self.calls.set(self.calls.get().saturating_add(1));
                let p = params.get()?;
                assert_eq!(p.get_invocation()?, ID);
                let b = p.get_binding()?;
                assert_eq!(b.get_host()?, HOST);
                assert_eq!(b.get_peer_id(), 9);
                assert_eq!(b.get_app_id()?.to_str()?, "notes");
                assert_eq!(b.get_record_id()?.to_str()?, "");
                assert_eq!(b.get_root()?, [3; 32]);
                assert_eq!(b.get_device()?, [4; 32]);
                assert_eq!(b.get_target_binding()?, [5; 32]);
                self.received.borrow_mut().push((
                    p.get_revision()?.to_str()?.into(),
                    p.get_resource()?.to_str()?.into(),
                    p.get_action()?.to_str()?.into(),
                    if p.get_has_argument() {
                        Some(p.get_argument()?.to_str()?.into())
                    } else {
                        None
                    },
                    p.get_confirmation_json()?.to_str()?.into(),
                ));
                observation(results.get().init_observation(), self.mode);
                Ok(())
            })())
        }
        fn published_invocation_observation(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::PublishedInvocationObservationParams,
            mut results: collab_capnp::channel_handle::PublishedInvocationObservationResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                self.calls.set(self.calls.get().saturating_add(1));
                let p = params.get()?;
                assert_eq!(p.get_host()?, HOST);
                assert_eq!(p.get_invocation()?, ID);
                observation(results.get().init_observation(), self.mode);
                Ok(())
            })())
        }
        fn cancel_durable_published_invocation(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::CancelDurablePublishedInvocationParams,
            mut results: collab_capnp::channel_handle::CancelDurablePublishedInvocationResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            Promise::from((|| {
                self.calls.set(self.calls.get().saturating_add(1));
                let p = params.get()?;
                assert_eq!(p.get_host()?, HOST);
                assert_eq!(p.get_invocation()?, ID);
                observation(results.get().init_observation(), self.mode);
                Ok(())
            })())
        }
    }
    fn server(mode: u8) -> (Server, Rc<Cell<usize>>, Received) {
        let calls = Rc::new(Cell::new(0));
        let received = Rc::default();
        (
            Server {
                calls: calls.clone(),
                mode,
                received: Rc::clone(&received),
            },
            calls,
            received,
        )
    }
    async fn over_wire<S, F, Fut>(server: S, action: F)
    where
        S: collab_capnp::channel_handle::Server + 'static,
        F: FnOnce(ChannelHandle) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_io, client_io) = tokio::io::duplex(512);
                let raw: collab_capnp::channel_handle::Client = capnp_rpc::new_client(server);
                let (read, write) = tokio::io::split(server_io);
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    capnp_rpc::rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let task =
                    tokio::task::spawn_local(RpcSystem::new(Box::new(network), Some(raw.client)));
                let connection = runtime_portal::CapnpClient::from_connected_io(
                    client_io,
                    capnp_rpc::rpc_twoparty_capnp::Side::Client,
                );
                let channel = ChannelHandle {
                    inner: Rc::new(connection.bootstrap()),
                };
                tilo_test_env::within("the RPC under test", action(channel)).await;
                task.abort();
                let _ = task.await;
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_round_trip_preserves_business_identity_and_optional_argument() {
        let (s, calls, received) = server(0);
        over_wire(s, |c| async move {
            let (row, b) = c
                .resolve_published_action_for_invocation(9, "v", "r", "a")
                .await
                .unwrap();
            assert_eq!(*row.action.as_ref().unwrap(), "a");
            assert_eq!(b, binding());
            for a in [None, Some(""), Some("exact")] {
                let o = c
                    .invoke_resolved_published_action_durable(&ID, &b, "v", "r", "a", a, "{}")
                    .await
                    .unwrap();
                assert_eq!(o.host, HOST);
                assert_eq!(o.invocation, ID);
                let r = o.result.unwrap();
                assert_eq!(r.id, 99);
                assert_eq!(r.update_json, UPDATE);
                assert!(!r.uncertain);
            }
            assert_eq!(
                c.published_invocation_observation(&HOST, &ID)
                    .await
                    .unwrap(),
                c.cancel_durable_published_invocation(&HOST, &ID)
                    .await
                    .unwrap()
            );
        })
        .await;
        assert_eq!(calls.get(), 6);
        assert_eq!(
            *received.borrow(),
            [None, Some(""), Some("exact")].map(|a| (
                "v".into(),
                "r".into(),
                "a".into(),
                a.map(str::to_owned),
                "{}".into()
            ))
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_old_35_without_binding_refuses_before_effect() {
        let (s, calls, _) = server(1);
        over_wire(s, |c| async move {
            assert!(matches!(
                c.resolve_published_action(9, "v", "r", "a").await.unwrap(),
                ActionDirectoryResolution::Found(_)
            ));
            assert!(
                c.resolve_published_action_for_invocation(9, "v", "r", "a")
                    .await
                    .is_err()
            );
        })
        .await;
        assert_eq!(calls.get(), 2);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_rejects_malformed_or_replaced_binding_and_resolution() {
        for mode in 2..=15 {
            let (s, calls, _) = server(mode);
            over_wire(s, move |c| async move {
                assert!(
                    c.resolve_published_action_for_invocation(9, "v", "r", "a")
                        .await
                        .is_err(),
                    "mode {mode}"
                );
            })
            .await;
            assert_eq!(calls.get(), 1);
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_rejects_uncorrelated_or_inconsistent_observation() {
        for mode in (1..=13).chain(16..=19) {
            let (s, calls, _) = server(mode);
            over_wire(s, move |c| async move {
                assert!(
                    c.invoke_resolved_published_action_durable(
                        &ID,
                        &binding(),
                        "v",
                        "r",
                        "a",
                        None,
                        "{}"
                    )
                    .await
                    .is_err(),
                    "invoke mode {mode}"
                );
                assert!(
                    c.published_invocation_observation(&HOST, &ID)
                        .await
                        .is_err(),
                    "observe mode {mode}"
                );
                assert!(
                    c.cancel_durable_published_invocation(&HOST, &ID)
                        .await
                        .is_err(),
                    "cancel mode {mode}"
                );
            })
            .await;
            assert_eq!(calls.get(), 3);
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_retains_absence_and_uncertainty_without_inventing_outcome() {
        for mode in [14, 15] {
            let (s, _, _) = server(mode);
            over_wire(s, move |c| async move {
                let o = c
                    .published_invocation_observation(&HOST, &ID)
                    .await
                    .unwrap();
                if mode == 14 {
                    assert!(o.result.is_none());
                } else {
                    let r = o.result.unwrap();
                    assert!(r.uncertain);
                    assert!(r.cancellation_requested);
                    assert_eq!(r.update_json, UPDATE);
                }
            })
            .await;
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_input_bounds_precede_every_request_copy() {
        let (s, calls, _) = server(0);
        over_wire(s, |c| async move {
            for (field, value) in [
                (0, String::new()),
                (0, "v".repeat(513)),
                (0, "v\0".into()),
                (1, String::new()),
                (1, "r\n".into()),
                (1, "r".repeat(513)),
                (2, "a\u{7f}".into()),
                (2, "a".repeat(513)),
                (3, "é".repeat(16385)),
                (3, "a\0".into()),
                (4, " ".repeat(4097)),
                (4, "{}\0".into()),
            ] {
                let mut a = ["v", "r", "a", "arg", "{}"];
                a[field] = &value;
                assert!(
                    c.invoke_resolved_published_action_durable(
                        &ID,
                        &binding(),
                        a[0],
                        a[1],
                        a[2],
                        Some(a[3]),
                        a[4]
                    )
                    .await
                    .is_err()
                );
                if field <= 2 {
                    assert!(
                        c.resolve_published_action_for_invocation(9, a[0], a[1], a[2])
                            .await
                            .is_err()
                    );
                }
            }
            for mode in 0..9 {
                let mut b = binding();
                match mode {
                    0 => b.host = [0; 16],
                    1 => b.peer_id = 0,
                    2 => b.peer_id = u64::MAX,
                    3 => b.app_id.clear(),
                    4 => b.app_id = "x".repeat(513),
                    5 => b.record_id = "x\n".into(),
                    6 => b.root = [0; 32],
                    7 => b.device = [0; 32],
                    _ => b.target_binding = [0; 32],
                }
                assert!(
                    c.invoke_resolved_published_action_durable(&ID, &b, "v", "r", "a", None, "{}")
                        .await
                        .is_err()
                );
            }
            assert!(
                c.invoke_resolved_published_action_durable(
                    &[0; 16],
                    &binding(),
                    "v",
                    "r",
                    "a",
                    None,
                    "{}"
                )
                .await
                .is_err()
            );
            for peer in [0, u64::MAX] {
                assert!(
                    c.resolve_published_action_for_invocation(peer, "v", "r", "a")
                        .await
                        .is_err()
                );
            }
            for (host, id) in [([0; 16], ID), (HOST, [0; 16])] {
                assert!(
                    c.published_invocation_observation(&host, &id)
                        .await
                        .is_err()
                );
                assert!(
                    c.cancel_durable_published_invocation(&host, &id)
                        .await
                        .is_err()
                );
            }
        })
        .await;
        assert_eq!(calls.get(), 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_accepts_inclusive_utf8_input_limits() {
        let (s, _, received) = server(0);
        over_wire(s, |c| async move {
            c.invoke_resolved_published_action_durable(
                &ID,
                &binding(),
                &"é".repeat(256),
                &"r".repeat(512),
                &"a".repeat(512),
                Some(&"é".repeat(16384)),
                &" ".repeat(4096),
            )
            .await
            .unwrap();
        })
        .await;
        assert_eq!(received.borrow()[0].3.as_ref().unwrap().len(), 32768);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_accepts_bounded_large_observation_without_correlation_loss() {
        let (s, _, _) = server(20);
        over_wire(s, |c| async move {
            let o = c
                .published_invocation_observation(&HOST, &ID)
                .await
                .unwrap();
            assert_eq!(o.invocation, ID);
            assert!(o.result.unwrap().update_json.len() > 240 * 1024);
        })
        .await;
    }
    async fn rejects_zero_wire_id(method: u8) {
        let (s, calls, _) = server(21);
        over_wire(s, move |c| async move {
            let result = match method {
                48 => {
                    c.invoke_resolved_published_action_durable(
                        &ID,
                        &binding(),
                        "v",
                        "r",
                        "a",
                        None,
                        "{}",
                    )
                    .await
                }
                49 => c.published_invocation_observation(&HOST, &ID).await,
                50 => c.cancel_durable_published_invocation(&HOST, &ID).await,
                _ => unreachable!("test selects one actual reliable RPC"),
            };
            assert!(
                result.is_err(),
                "found=true id=0 was accepted by RPC @{method}"
            );
        })
        .await;
        assert_eq!(
            calls.get(),
            1,
            "actual malformed RPC reply must be observed"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_zero_wire_id_invoke_refuses() {
        rejects_zero_wire_id(48).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_zero_wire_id_observe_refuses() {
        rejects_zero_wire_id(49).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_zero_wire_id_cancel_refuses() {
        rejects_zero_wire_id(50).await;
    }
    struct Old47(Rc<Cell<usize>>);
    impl collab_capnp::channel_handle::Server for Old47 {
        fn invoke_resolved_published_action(
            self: Rc<Self>,
            _: collab_capnp::channel_handle::InvokeResolvedPublishedActionParams,
            mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            self.0.set(self.0.get().saturating_add(1));
            results.get().set_id(47);
            Promise::ok(())
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_invocation_old_47_server_has_no_fallback_effect() {
        let effects = Rc::new(Cell::new(0));
        let seen = effects.clone();
        over_wire(Old47(effects), |c| async move {
            let e = c
                .invoke_resolved_published_action_durable(
                    &ID,
                    &binding(),
                    "v",
                    "r",
                    "a",
                    None,
                    "{}",
                )
                .await
                .unwrap_err();
            assert!(matches!(e,NotAvailable::Capnp(e) if e.kind==capnp::ErrorKind::Unimplemented));
            assert_eq!(seen.get(), 0);
            assert_eq!(
                c.invoke_resolved_published_action(9, "v", "r", "a", None, "{}")
                    .await
                    .unwrap()
                    .0,
                47
            );
            assert_eq!(seen.get(), 1);
        })
        .await;
    }
    #[test]
    fn reliable_invocation_old_35_and_47_parameter_words_stay_exact() {
        let fixtures: [(bool, &[u64]); 2] = [
            (
                false,
                &[
                    0x0000_0008_0000_0000,
                    0x0003_0001_0000_0000,
                    9,
                    0x0000_0012_0000_0009,
                    0x0000_0012_0000_0009,
                    0x0000_0012_0000_0009,
                    0x76,
                    0x72,
                    0x61,
                ],
            ),
            (
                true,
                &[
                    0x0000_000d_0000_0000,
                    0x0005_0002_0000_0000,
                    9,
                    1,
                    0x0000_0012_0000_0011,
                    0x0000_0012_0000_0011,
                    0x0000_0012_0000_0011,
                    0x0000_000a_0000_0011,
                    0x0000_001a_0000_0011,
                    0x76,
                    0x72,
                    0x61,
                    0,
                    0x7d7b,
                ],
            ),
        ];
        for (lazy, words) in fixtures {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let decoded = capnp::serialize::read_message(
                &mut bytes.as_slice(),
                capnp::message::ReaderOptions::default(),
            )
            .unwrap();
            let mut out = capnp::message::Builder::new_default();
            if lazy {
                let p=decoded.get_root::<collab_capnp::channel_handle::invoke_resolved_published_action_params::Reader<'_>>().unwrap();
                assert!(p.get_has_argument());
                assert_eq!(p.get_revision().unwrap().to_str().unwrap(), "v");
                let mut p=out.init_root::<collab_capnp::channel_handle::invoke_resolved_published_action_params::Builder<'_>>();
                p.set_peer_id(9);
                p.set_has_argument(true);
                p.set_revision("v");
                p.set_resource("r");
                p.set_action("a");
                p.set_argument("");
                p.set_confirmation_json("{}");
            } else {
                let p=decoded.get_root::<collab_capnp::channel_handle::resolve_published_action_params::Reader<'_>>().unwrap();
                assert_eq!(p.get_revision().unwrap().to_str().unwrap(), "v");
                let mut p=out.init_root::<collab_capnp::channel_handle::resolve_published_action_params::Builder<'_>>();
                p.set_peer_id(9);
                p.set_revision("v");
                p.set_resource("r");
                p.set_action("a");
            }
            assert_eq!(capnp::serialize::write_message_to_words(&out), bytes);
        }
    }
}
