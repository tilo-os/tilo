//! One relay co-edit session for an enrolled room: dialing the host its
//! enrollment pins, admitting what arrives with closed relay provenance, and
//! sending what the owner holds in outgoing custody. The host drives these
//! from its own event loop.
mod cursor;

use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostTrust};
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::{Reseal, TransportOrigin, join::TransportHint};
use tilo_relay_session::{RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCert, RevocationList};

/// An enrolled room to hold a session for.
#[derive(Clone)]
pub struct Connect {
    owner: NativeWorkspaceOwner,
    room: RoomId,
}
impl std::fmt::Debug for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NativeConnect").field(&self.room).finish()
    }
}
impl Connect {
    /// # Errors
    /// `room` has no committed enrollment.
    pub fn new(owner: NativeWorkspaceOwner, room: RoomId) -> Result<Self, String> {
        owner.with_current(room, |_| Ok(()))?;
        Ok(Self { owner, room })
    }
    #[must_use]
    pub fn room(&self) -> RoomId {
        self.room
    }
    /// What a session for this room must be pinned to now: the host hint,
    /// this device's certificate, the revocations to dial with and the
    /// owner's continuity.
    ///
    /// # Errors
    /// Custody differs from the enrollment, this device may no longer act in
    /// the room, or its host is revoked.
    pub fn material(
        &self,
        identity: &HostIdentity,
    ) -> Result<(TransportHint, DeviceCert, RevocationList, u64), String> {
        self.owner.with_current(self.room, |view| {
            let (cert, entries) = identity.current_native_material(view.workspace())?;
            if &cert != view.device() {
                return Err("native relay custody differs from enrollment".into());
            }
            view.peer()
                .permissions(cert.device_public_key, std::time::SystemTime::now())?;
            if !view.peer().certificates().contains(&cert) {
                return Err("selected certificate differs from current admitted roster".into());
            }
            let mut revocations = RevocationList::new();
            for entry in entries.into_iter().chain(view.peer().revocations()) {
                revocations.insert(entry).map_err(|e| e.to_string())?;
            }
            if revocations.is_revoked(&view.transport_hint().host.device_public_key) {
                return Err("native relay host is revoked".into());
            }
            Ok((
                view.transport_hint().clone(),
                cert,
                revocations,
                view.continuity(),
            ))
        })
    }
}
/// A live session's pin; closed when the session ends.
///
/// The pin is the host it reached and the certificate it authenticated
/// with. The owner's continuity moves with any security change of the room,
/// another member joining included; the session follows it in place once
/// this device's own standing was checked again ([`Connect::material`]),
/// rather than ending and redialling every member for one person's join.
pub struct Scope {
    connect: Connect,
    hint: TransportHint,
    device: DeviceCert,
    /// The owner continuity the session last validated against.
    continuity: std::sync::atomic::AtomicU64,
    live: Mutex<bool>,
    cursor: Mutex<cursor::Cursor>,
    refresh_sent: Mutex<Option<std::time::Instant>>,
}
impl Scope {
    #[must_use]
    pub fn new(connect: Connect, hint: TransportHint, device: DeviceCert, continuity: u64) -> Self {
        Self {
            connect,
            hint,
            device,
            continuity: std::sync::atomic::AtomicU64::new(continuity),
            live: Mutex::new(true),
            cursor: Mutex::new(cursor::Cursor::default()),
            refresh_sent: Mutex::new(None),
        }
    }
    #[must_use]
    pub fn room(&self) -> RoomId {
        self.connect.room
    }
    /// Whether this session is still pinned to this material: the same host
    /// and certificate. `continuity` is the owner's current one, which the
    /// caller read with the material that re-validated this device's standing
    /// ([`Connect::material`] refuses a device no longer admitted, permitted
    /// or behind an unrevoked host); the session adopts it.
    #[must_use]
    pub fn pinned_to(&self, hint: &TransportHint, device: &DeviceCert, continuity: u64) -> bool {
        let pinned = hint == &self.hint && device == &self.device;
        if pinned {
            self.continuity
                .store(continuity, std::sync::atomic::Ordering::SeqCst);
        }
        pinned
    }
    /// The owner continuity this session last validated against.
    fn continuity(&self) -> u64 {
        self.continuity.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn close(&self) {
        if let Ok(mut live) = self.live.lock() {
            *live = false;
        }
    }
}
impl Scope {
    /// A delta the authenticated relay pump received on this session.
    #[must_use]
    pub fn input(self: &Arc<Self>, author: [u8; 32], retained: bool, msg: NeighbourMsg) -> Input {
        Input {
            scope: self.clone(),
            author,
            retained,
            msg,
        }
    }
    /// Ask the pinned host to run its own checkpoint barrier, at most every
    /// 30 seconds. It grants no local admission and retires no outbox.
    ///
    /// # Errors
    /// The session closed, its pin or the owner changed, or the request
    /// cannot be sent.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the live flag and refresh cadence stay locked through the request so a close \
                  or a concurrent refresh cannot interleave"
    )]
    pub fn refresh(
        &self,
        session: &RelayCoeditSession,
        identity: &HostIdentity,
    ) -> Result<(), String> {
        let live = self
            .live
            .lock()
            .map_err(|_| "native refresh session poisoned")?;
        if !*live {
            return Err("native refresh session closed".into());
        }
        let (hint, device, _, continuity) = self.connect.material(identity)?;
        if !self.pinned_to(&hint, &device, continuity) {
            return Err("native refresh scope changed".into());
        }
        self.connect.owner.with_current(self.connect.room, |view| {
            if view.device() != &self.device
                || view.transport_hint() != &self.hint
                || view.continuity() != self.continuity()
            {
                return Err("native refresh current owner changed".into());
            }
            view.peer()
                .permissions(self.device.device_public_key, std::time::SystemTime::now())?;
            let mut last = self
                .refresh_sent
                .lock()
                .map_err(|_| "native refresh cadence poisoned")?;
            let now = std::time::Instant::now();
            if last
                .is_some_and(|sent| now.saturating_duration_since(sent) < Duration::from_secs(30))
            {
                return Ok(());
            }
            session
                .request_checkpoint_refresh()
                .map_err(|error| error.to_string())?;
            *last = Some(now);
            Ok(())
        })
    }
    /// Reconcile the owner's outgoing custody and send, once per session,
    /// every entry it may send now. Sealed writes that expired in custody
    /// are not renewed: see [`Self::publish_renewing`].
    ///
    /// # Errors
    /// The session closed, its pin, the owner or custody changed, or the
    /// session refuses a frame; unsent entries stay in durable custody.
    pub fn publish(
        &self,
        session: &RelayCoeditSession,
        identity: &HostIdentity,
    ) -> Result<(), String> {
        self.send(session, identity, None)
    }
    /// [`Self::publish`], first renewing this device's sealed writes that
    /// expired in custody through `reseal`, the channels' key holder (see
    /// [`NativeWorkspaceOwner::renew_outgoing`]).
    ///
    /// # Errors
    /// As [`Self::publish`].
    pub fn publish_renewing(
        &self,
        session: &RelayCoeditSession,
        identity: &HostIdentity,
        reseal: &mut Reseal<'_>,
    ) -> Result<(), String> {
        self.send(session, identity, Some(reseal))
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the live flag and cursor stay locked through publication so a close cannot interleave"
    )]
    fn send(
        &self,
        session: &RelayCoeditSession,
        identity: &HostIdentity,
        reseal: Option<&mut Reseal<'_>>,
    ) -> Result<(), String> {
        let live = self
            .live
            .lock()
            .map_err(|_| "native publication session poisoned")?;
        if !*live {
            return Err("native publication session closed".into());
        }
        let (hint, device, _, continuity) = self.connect.material(identity)?;
        if !self.pinned_to(&hint, &device, continuity) {
            return Err("native publication session changed".into());
        }
        // Reconciliation may persist a genuine authorized successor. Its
        // failure must not erase or prevent other fresh retained frames.
        if let Err(error) = self.connect.owner.reconcile_outgoing(
            self.connect.room,
            &identity.device_signing_key,
            std::time::SystemTime::now(),
        ) {
            tracing::debug!(%error,"native publication reconciliation blocked");
        }
        if let Some(reseal) = reseal
            && let Err(error) = self.connect.owner.renew_outgoing(
                self.connect.room,
                &identity.device_signing_key,
                reseal,
                std::time::SystemTime::now(),
            )
        {
            tracing::debug!(%error, "native sealed write renewal blocked");
        }
        self.connect.owner.with_outgoing(
            self.connect.room,
            std::time::SystemTime::now(),
            |view, outgoing| {
                if view.device() != &self.device
                    || view.transport_hint() != &self.hint
                    || view.continuity() != self.continuity()
                {
                    return Err("native publication authority changed".into());
                }
                let (selected, _) = identity.current_native_material(view.workspace())?;
                if &selected != view.device() {
                    return Err("native publication custody changed".into());
                }
                view.peer()
                    .permissions(selected.device_public_key, std::time::SystemTime::now())?;
                let mut cursor = self
                    .cursor
                    .lock()
                    .map_err(|_| "native publication cursor poisoned")?;
                cursor.drive(
                    outgoing.queued(),
                    outgoing.entries().map(|entry| {
                        (
                            entry.id(),
                            entry.bytes(),
                            entry.channel(),
                            entry.retention(),
                        )
                    }),
                    |bytes, channel, retention| {
                        match retention {
                            neighbour_wire::Retention::Delta => {
                                session.publish_retained_delta(channel, bytes.to_vec())
                            }
                            // Members never republish a governor checkpoint with
                            // their own relay author, even if a producer errs. A
                            // sealed checkpoint already arrives here as a delta
                            // (`publication::member_publishes_as_delta`).
                            neighbour_wire::Retention::Checkpoint => {
                                return Err("native member checkpoint publication refused".into());
                            }
                        }
                        .map_err(|e| e.to_string())
                    },
                )
            },
        )
    }
}
/// Only the authenticated relay pump can construct this. Generic gossip messages
/// cannot become `RelayDelta` evidence through a host's event dispatcher.
#[derive(Clone)]
pub struct Input {
    scope: Arc<Scope>,
    author: [u8; 32],
    retained: bool,
    msg: NeighbourMsg,
}
impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRelayInput")
            .field("room", &self.scope.connect.room)
            .field("retained", &self.retained)
            .finish_non_exhaustive()
    }
}
impl Input {
    #[must_use]
    pub fn room(&self) -> RoomId {
        self.scope.connect.room
    }
    #[must_use]
    pub fn author(&self) -> [u8; 32] {
        self.author
    }
    #[must_use]
    pub fn message(&self) -> &NeighbourMsg {
        &self.msg
    }
    /// Whether the host sent it as part of its retained replay, before
    /// answering the subscription.
    #[must_use]
    pub fn retained(&self) -> bool {
        self.retained
    }
    /// Observe the actual committed owner generation, without granting admission.
    ///
    /// # Errors
    /// The enrolled room is unavailable or fenced.
    pub fn generation(&self) -> Result<u64, String> {
        self.scope
            .connect
            .owner
            .with_current(self.room(), |view| Ok(view.generation()))
    }
    /// Admit the delta into the owner while the session is still live and
    /// pinned to the owner's current material.
    ///
    /// # Errors
    /// The session closed or its pin changed, or admission refuses it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the live flag stays locked through admission so a close cannot interleave"
    )]
    pub fn admit(
        &self,
        identity: &HostIdentity,
    ) -> Result<tilo_peer_state::AdmissionReport, String> {
        let live = self
            .scope
            .live
            .lock()
            .map_err(|_| "native relay continuity poisoned")?;
        if !*live {
            return Err("native relay session closed".into());
        }
        let (hint, device, _, continuity) = self.scope.connect.material(identity)?;
        if !self.scope.pinned_to(&hint, &device, continuity) {
            return Err("native relay pin changed".into());
        }
        let origin = if self.retained {
            TransportOrigin::RetainedRelayDelta(self.author)
        } else {
            TransportOrigin::RelayDelta(self.author)
        };
        self.scope.connect.owner.receive(
            self.room(),
            origin,
            self.msg.clone(),
            std::time::SystemTime::now(),
        )
    }
    /// The author's current standing in the room: the room's policy, and
    /// the bound on a device not theirs.
    ///
    /// # Errors
    /// The room is fenced or the author is not a member.
    pub fn standing(&self) -> Result<tilo_action_policy::Standing, String> {
        self.scope.connect.owner.with_current(self.room(), |view| {
            view.peer()
                .permissions(self.author, std::time::SystemTime::now())
                .map(tilo_peer_state::MemberAuthority::standing)
        })
    }
}

/// Why dialing a room's host did not give a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialError {
    /// The host speaks peer protocol `theirs`, not this device's: they do
    /// not sync until the one behind has updated, which the person is told
    /// (docs/architecture/compatibility.md). Dialing again changes nothing
    /// until then.
    PeerVersion {
        /// The host's version.
        theirs: u32,
    },
    /// The host closed the stream without answering this device's hello:
    /// it refused this device, or it predates versioned handshakes. After a
    /// few of these in a row, [`probe_version`] tells which.
    NoAnswer(String),
    /// The host admits nobody of this device's person any more: they were
    /// removed (a public room prunes guests away for long), and joining
    /// again, where the room lets them, admits them anew. Dialing again
    /// changes nothing until then.
    NotAdmitted,
    /// Anything else, in words for the log; dialing again may succeed.
    Failed(String),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PeerVersion { theirs } => write!(
                f,
                "the host speaks peer protocol {theirs}, this device {}",
                neighbour_wire::PEER_PROTOCOL_VERSION
            ),
            Self::NoAnswer(reason) => {
                write!(f, "the host closed the stream without answering: {reason}")
            }
            Self::NotAdmitted => f.write_str("the host admits this device's person no longer"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for DialError {}

impl From<String> for DialError {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl From<&str> for DialError {
    fn from(reason: &str) -> Self {
        Self::Failed(reason.to_owned())
    }
}

impl From<DialError> for String {
    fn from(error: DialError) -> Self {
        error.to_string()
    }
}

impl From<tilo_relay_session::ClientError> for DialError {
    fn from(error: tilo_relay_session::ClientError) -> Self {
        match &error {
            tilo_relay_session::ClientError::NoHandshakeAnswer(reason) => {
                return Self::NoAnswer(reason.clone());
            }
            tilo_relay_session::ClientError::Https(tilo_relay_client::LinkError::Denied(
                tilo_relay_protocol::web::SessionRefusal::NotAdmitted,
            )) => return Self::NotAdmitted,
            _ => {}
        }
        match error.peer_version() {
            Some(theirs) => Self::PeerVersion { theirs },
            None => Self::Failed(error.to_string()),
        }
    }
}

/// How many dials in a row the room's host may close without answering
/// before [`probe_version`] asks whether it predates versioned handshakes.
pub const UNANSWERED_BEFORE_PROBE: u32 = 3;

/// The peer protocol version of the host `hint` pins, when a probe shows it
/// is not this device's: a host that predates versioned handshakes reads as
/// [`tilo_relay_protocol::auth::UNVERSIONED_PEER_PROTOCOL`]. `None` when the
/// host speaks this device's version (it refused this device instead), or
/// the probe could not tell.
pub async fn probe_version(
    endpoint: &iroh::Endpoint,
    hint: &TransportHint,
    cert: &DeviceCert,
) -> Option<u32> {
    let (payload, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(hint).ok()?;
    let probe = tokio::time::timeout(
        Duration::from_secs(10),
        RelayClient::probe_handshake_version(endpoint, payload.endpoint_addr.clone(), cert),
    )
    .await
    .ok()?
    .ok()?;
    match probe {
        tilo_relay_session::client::HandshakeProbe::Unversioned => {
            Some(tilo_relay_protocol::auth::UNVERSIONED_PEER_PROTOCOL)
        }
        tilo_relay_session::client::HandshakeProbe::Versioned(theirs)
            if theirs != neighbour_wire::PEER_PROTOCOL_VERSION =>
        {
            Some(theirs)
        }
        tilo_relay_session::client::HandshakeProbe::Versioned(_)
        | tilo_relay_session::client::HandshakeProbe::Unknown => None,
    }
}

/// Dial the host `hint` pins for workspace `ws` and join its co-edit
/// session as `cert`.
///
/// # Errors
/// The hint does not authenticate for `ws`, the host authenticates as
/// another certificate or speaks another peer protocol version
/// ([`DialError::PeerVersion`]), or the connection fails or takes over 10
/// seconds.
pub async fn dial(
    endpoint: iroh::Endpoint,
    ws: u64,
    hint: &TransportHint,
    cert: DeviceCert,
    key: SigningKey,
    revocations: RevocationList,
) -> Result<RelayCoeditSession, DialError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (payload, trust) = IrohWorkspaceHostHintV1::authenticate_transport_hint(hint)
            .map_err(|e| e.to_string())?;
        if payload.workspace_id != ws {
            return Err("native relay workspace pin mismatch".into());
        }
        let addr = payload.endpoint_addr.clone();
        let own_device = cert.device_public_key;
        let client = match trust {
            WorkspaceHostTrust::CertifiedHost => {
                RelayClient::connect_v2_to_enrolled_home_peer(
                    endpoint,
                    addr,
                    hint.host.user_public_key,
                    hint.host.device_public_key,
                    cert,
                    key,
                    revocations,
                )
                .await
            }
            WorkspaceHostTrust::InteractiveOwner(trust) => {
                RelayClient::connect_v2_to_interactive_workspace_owner(
                    endpoint,
                    addr,
                    trust,
                    cert,
                    key,
                    revocations,
                )
                .await
            }
        }
        .map_err(DialError::from)?;
        if client.relay_peer.cert != hint.host {
            return Err("native relay authenticated a different host certificate".into());
        }
        RelayCoeditSession::join(client, ws, own_device)
            .map_err(|error| DialError::Failed(error.to_string()))
    })
    .await
    .map_err(|_| DialError::from("native relay connection timed out"))?
}

/// [`dial`] the host `hint` pins over its HTTPS routes behind `http`: a
/// host that publishes no iroh address. Only a certified host serves them,
/// so the session pins its user and device as [`dial`] does an enrolled
/// host; the session then carries no private action channel.
///
/// # Errors
/// As [`dial`], and for a hint that pins a person's own device rather than a
/// certified host.
pub async fn dial_https(
    http: std::sync::Arc<dyn tilo_relay_client::HttpClient>,
    ws: u64,
    hint: &TransportHint,
    cert: DeviceCert,
    key: SigningKey,
    revocations: RevocationList,
) -> Result<RelayCoeditSession, DialError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (payload, trust) = IrohWorkspaceHostHintV1::authenticate_transport_hint(hint)
            .map_err(|e| e.to_string())?;
        if payload.workspace_id != ws {
            return Err("native relay workspace pin mismatch".into());
        }
        if !matches!(trust, WorkspaceHostTrust::CertifiedHost) {
            return Err("only a certified host is reached over HTTPS".into());
        }
        let own_device = cert.device_public_key;
        let client =
            RelayClient::connect_https(http, hint.host.device_public_key, cert, &key, revocations)
                .await
                .map_err(DialError::from)?;
        if client.relay_peer.cert != hint.host {
            return Err("native relay authenticated a different host certificate".into());
        }
        RelayCoeditSession::join(client, ws, own_device)
            .map_err(|error| DialError::Failed(error.to_string()))
    })
    .await
    .map_err(|_| DialError::from("native relay connection timed out"))?
}

#[cfg(test)]
mod dial_error_tests {
    use super::DialError;
    use tilo_relay_session::ClientError;

    // USER REQUIREMENT (2026-10-19): "If a v6 client meets the v5 relay, the user must see a clear "update needed" state, never a silent empty room."
    #[test]
    fn a_host_at_another_version_stays_a_version_mismatch_through_the_dial() {
        let behind = neighbour_wire::PEER_PROTOCOL_VERSION - 1;
        assert_eq!(
            DialError::from(ClientError::PeerVersion { theirs: behind }),
            DialError::PeerVersion { theirs: behind }
        );
        // Over a site's HTTPS routes, from a host that predates versioned
        // handshakes too.
        let unversioned = tilo_relay_protocol::auth::UNVERSIONED_PEER_PROTOCOL;
        assert_eq!(
            DialError::from(ClientError::Https(
                tilo_relay_client::LinkError::PeerVersion {
                    theirs: unversioned
                }
            )),
            DialError::PeerVersion {
                theirs: unversioned
            }
        );
        assert!(matches!(
            DialError::from(ClientError::Https(
                tilo_relay_client::LinkError::Unavailable("status 503".into())
            )),
            DialError::Failed(_)
        ));
        // A person the room removed is told apart, so the device can join
        // again through the room's door.
        assert_eq!(
            DialError::from(ClientError::Https(tilo_relay_client::LinkError::Denied(
                tilo_relay_protocol::web::SessionRefusal::NotAdmitted
            ))),
            DialError::NotAdmitted
        );
    }
}
