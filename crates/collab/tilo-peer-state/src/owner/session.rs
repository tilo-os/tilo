//! One relay co-edit session for an enrolled room: dialing the host its
//! enrollment pins and holding the session ([`hold`]), admitting what
//! arrives with closed relay provenance, and sending what the owner holds
//! in outgoing custody.
mod cursor;
mod hold;
mod reach;

pub use hold::{Holder, Link, hold};
pub use reach::{OwnHost, Reach};

use super::publication::SealRecord;
use crate::owner::{NativeWorkspaceOwner, RoomId};
use crate::{Reseal, TransportOrigin, join::TransportHint};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_identity_host::custody::HostIdentity;
use tilo_relay_session::RelayCoeditSession;
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
            if revocations.is_revoked(&view.transport_hint()?.host.device_public_key) {
                return Err("native relay host is revoked".into());
            }
            Ok((
                view.transport_hint()?.clone(),
                cert,
                revocations,
                view.continuity(),
            ))
        })
    }
    /// The hosts the room's governor named besides itself
    /// ([`crate::room_hosts_of`]), in the order governance holds
    /// them: where a member's session goes while the governor is out of
    /// reach. A host the room revoked, or one naming this device, is left
    /// out.
    ///
    /// # Errors
    /// The room is not held here.
    pub fn hosts(&self, own: &[u8; 32]) -> Result<Vec<TransportHint>, String> {
        self.owner.with_current(self.room, |view| {
            let governor = view.transport_hint()?.host.device_public_key;
            let revocations = view.peer().revocations();
            Ok(view
                .peer()
                .governance()
                .room_hosts()
                .into_iter()
                .filter(|hint| {
                    let device = hint.host.device_public_key;
                    device != governor
                        && &device != own
                        && !revocations
                            .iter()
                            .any(|entry| entry.revoked_device_public_key == device)
                })
                .collect())
        })
    }

    /// The host to dial after `after` (the first when `None`) among
    /// [`Self::hosts`]: where the room's session goes next while its
    /// governor is out of reach. `None` once every host was tried.
    ///
    /// # Errors
    /// The room is not held here.
    pub fn next_host(
        &self,
        own: &[u8; 32],
        after: Option<&TransportHint>,
    ) -> Result<Option<TransportHint>, String> {
        let hosts = self.hosts(own)?;
        let next = after
            .and_then(|after| hosts.iter().position(|host| host == after))
            .map_or(0, |index| index.saturating_add(1));
        Ok(hosts.into_iter().nth(next))
    }

    /// Whether this device is the room's host itself: the host its
    /// enrollment pins is this very device. Such a room is never dialled.
    /// This device holds the host's copy, so there is nothing to catch up on
    /// and nobody else to reach.
    #[must_use]
    pub fn hosted_here(&self, identity: &HostIdentity) -> bool {
        self.owner
            .with_current(self.room, |view| {
                let (cert, _) = identity.current_native_material(view.workspace())?;
                Ok(view.transport_hint()?.host.device_public_key == cert.device_public_key)
            })
            .unwrap_or(false)
    }
    /// The renewal of the certificate the room's record holds, when this
    /// device's custody holds one: the device proves its membership with it
    /// ([`crate::owner::enroll::renew_membership`]) before its sessions dial again,
    /// as [`Self::material`] refuses custody that differs from the record.
    ///
    /// # Errors
    /// The room is not held here or custody cannot be read.
    pub fn renewal(&self, identity: &HostIdentity) -> Result<Option<DeviceCert>, String> {
        self.owner.with_current(self.room, |view| {
            let (cert, _) = identity.current_native_material(view.workspace())?;
            Ok(crate::owner::renewal::renews(&cert, view.device()).then_some(cert))
        })
    }
    /// The owner of the room's record.
    #[must_use]
    pub fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
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
    /// The host the session reached: the governor `hint` names, or a host
    /// the room's governance names while the governor is out of reach.
    via: TransportHint,
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
        Self::via(connect, hint.clone(), hint, device, continuity)
    }
    /// A session pinned to the room's governor `hint` that reached the host
    /// `via` instead.
    #[must_use]
    pub fn via(
        connect: Connect,
        hint: TransportHint,
        via: TransportHint,
        device: DeviceCert,
        continuity: u64,
    ) -> Self {
        Self {
            connect,
            hint,
            via,
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
    /// The host this session reached.
    #[must_use]
    pub fn host(&self) -> &TransportHint {
        &self.via
    }
    /// Whether this session reached the room's governor itself.
    #[must_use]
    pub fn reaches_governor(&self) -> bool {
        self.via == self.hint
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
                || view.transport_hint()? != &self.hint
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
    /// Compact the sealed record histories of this room that have used most
    /// of their bound into one checkpoint each, through `open` and `seal`,
    /// this device's channel keys, so the compactions publish with the
    /// device's own custody (see
    /// [`NativeWorkspaceOwner::compact_outgoing`]). Answers how many
    /// histories were compacted.
    ///
    /// # Errors
    /// This device is no longer the session's, or a compaction cannot be
    /// committed.
    pub fn compact(
        &self,
        identity: &HostIdentity,
        open: &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealRecord<'_>,
    ) -> Result<usize, String> {
        let (hint, device, _, continuity) = self.connect.material(identity)?;
        if !self.pinned_to(&hint, &device, continuity) {
            return Err("native compaction scope changed".into());
        }
        self.connect.owner.compact_outgoing(
            self.connect.room,
            &identity.device_signing_key,
            open,
            seal,
            std::time::SystemTime::now(),
        )
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
                    || view.transport_hint()? != &self.hint
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
                            // A checkpoint leaves as one: this device's sealed
                            // compaction of a record, which covers the writes
                            // before it, and the room's own governance
                            // checkpoint. The store then keeps it as the whole
                            // of what this author retained there. A cleartext
                            // snapshot never reaches here: the governor's
                            // baseline stays the governor's to send.
                            neighbour_wire::Retention::Checkpoint => {
                                session.publish_checkpoint(channel, bytes.to_vec())
                            }
                        }
                        .map_err(|e| e.to_string())
                    },
                )
            },
        )
    }
}
/// Only the authenticated relay pump can construct this. A message from any
/// other path cannot become `RelayDelta` evidence through a host's event
/// dispatcher.
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
    pub fn admit(&self, identity: &HostIdentity) -> Result<crate::AdmissionReport, String> {
        Self::admit_batch(vec![self.clone()], identity)
            .inputs
            .pop()
            .ok_or_else(|| "native owner admitted no input".to_owned())?
            .1
    }
    /// Whether `other` came over the same relay session as this one: what
    /// [`Self::admit_batch`] admits together.
    #[must_use]
    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.scope, &other.scope)
    }
    /// Admit `inputs`, all of one relay session, in the order they came, in
    /// one commit of the owner, while the session is still live and pinned
    /// to the owner's current material: each is authorized as one alone is,
    /// and a burst of them persists once. Each input comes back with its
    /// report; inputs that span sessions, a closed session, a changed pin
    /// or a failed commit refuse them all.
    #[must_use]
    pub fn admit_batch(inputs: Vec<Self>, identity: &HostIdentity) -> Admitted {
        let Some(first) = inputs.first() else {
            return Admitted {
                inputs: Vec::new(),
                changed: false,
            };
        };
        let before = first.generation().ok();
        let reports = Self::admit_pinned(&inputs, identity);
        let changed = before != first.generation().ok();
        let inputs = match reports {
            Ok(reports) => inputs.into_iter().zip(reports).collect(),
            Err(error) => inputs
                .into_iter()
                .map(|input| (input, Err(error.clone())))
                .collect(),
        };
        Admitted { inputs, changed }
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the live flag stays locked through admission so a close cannot interleave"
    )]
    fn admit_pinned(
        inputs: &[Self],
        identity: &HostIdentity,
    ) -> Result<Vec<Result<crate::AdmissionReport, String>>, String> {
        let Some(first) = inputs.first() else {
            return Ok(Vec::new());
        };
        if !inputs.iter().all(|input| first.same_session(input)) {
            return Err("native relay inputs of different sessions".into());
        }
        let live = first
            .scope
            .live
            .lock()
            .map_err(|_| "native relay continuity poisoned")?;
        if !*live {
            return Err("native relay session closed".into());
        }
        let (hint, device, _, continuity) = first.scope.connect.material(identity)?;
        if !first.scope.pinned_to(&hint, &device, continuity) {
            return Err("native relay pin changed".into());
        }
        let batch = inputs
            .iter()
            .map(|input| {
                let origin = if input.retained {
                    TransportOrigin::RetainedRelayDelta(input.author)
                } else {
                    TransportOrigin::RelayDelta(input.author)
                };
                (origin, input.msg.clone())
            })
            .collect();
        first
            .scope
            .connect
            .owner
            .receive_batch(first.room(), batch, std::time::SystemTime::now())
    }
    /// The app write `report` took in from another device, for this
    /// device's apps: `None` for anything else, a write the owner neither
    /// accepted nor already held, this device's own write, a write naming
    /// another room, or an author without standing.
    #[must_use]
    pub fn app_write(&self, report: &crate::AdmissionReport) -> Option<AppWrite> {
        if !matches!(
            report.primary(),
            crate::InputDisposition::Accepted(_) | crate::InputDisposition::Duplicate(_)
        ) || self.author == self.scope.device.device_public_key
        {
            return None;
        }
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = &self.msg
        else {
            return None;
        };
        if cluster_id != &self.room().to_string() {
            return None;
        }
        Some(AppWrite {
            peer: *peer,
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            record_id: record_id.clone(),
            author: self.standing().ok()?,
            replayed: self.retained,
            envelope: (**envelope).clone(),
        })
    }
    /// The author's current standing in the room: the room's policy, and
    /// the bound on a device not theirs.
    pub fn standing(&self) -> Result<tilo_action_policy::Standing, String> {
        self.scope.connect.owner.with_current(self.room(), |view| {
            view.peer()
                .permissions(self.author, std::time::SystemTime::now())
                .map(crate::MemberAuthority::standing)
        })
    }
}

/// What [`Input::admit_batch`] decided.
pub struct Admitted {
    /// Each input, in the order it came, with what its admission decided.
    pub inputs: Vec<(Input, Result<crate::AdmissionReport, String>)>,
    /// Whether the admission committed a new owner generation.
    pub changed: bool,
}

/// Another device's app write the owner took in ([`Input::app_write`]).
pub struct AppWrite {
    /// The writer's peer handle, as the write names it.
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// The author's standing in the room.
    pub author: tilo_action_policy::Standing,
    /// Whether the host replayed it from what it retains.
    pub replayed: bool,
    pub envelope: collab_types::SignedOp,
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
    /// it refused this device, or it predates versioned handshakes. After
    /// [`UNANSWERED_BEFORE_PROBE`] of these in a row, a probe tells which.
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
/// before a dial asks whether it predates versioned handshakes.
pub const UNANSWERED_BEFORE_PROBE: u32 = 3;

/// Dial the host `hint` pins for workspace `ws` and join its co-edit
/// session as `cert`.
///
/// # Errors
/// As [`Reach::session`].
pub async fn dial(
    endpoint: iroh::Endpoint,
    ws: u64,
    hint: &TransportHint,
    cert: DeviceCert,
    key: SigningKey,
    revocations: RevocationList,
) -> Result<RelayCoeditSession, DialError> {
    let reach = Reach {
        endpoint,
        https: None,
        own_host: None,
    };
    reach.session(ws, hint, cert, key, revocations, false).await
}

pub use crate::owner::hosted_by;

#[cfg(test)]
mod dial_error_tests {
    use super::DialError;
    use tilo_relay_session::ClientError;

    #[test]
    fn a_host_at_another_version_stays_a_version_mismatch_through_the_dial() {
        let behind = neighbour_wire::PEER_PROTOCOL_VERSION - 1;
        assert_eq!(
            DialError::from(ClientError::PeerVersion { theirs: behind }),
            DialError::PeerVersion { theirs: behind }
        );
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
