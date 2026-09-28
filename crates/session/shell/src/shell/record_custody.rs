//! Record custody: a member of a room reaches the records that room
//! references (`docs/architecture/rooms.md`).
//!
//! A room holds references, not copies. For the reference to grant access,
//! someone who holds the record has to serve it where the room's members can
//! reach it. For the records the shell itself owns, that is this module:
//! for every room whose index references a record it holds, it serves the
//! record on the room's route for it, `(so.tilo.shell, room, record)`. The
//! records it holds are the ones in the shell's own record store (a
//! remark's reviewed recording is written there when it is sent). It
//! answers a local late joiner's snapshot request there like any holder,
//! and it publishes the document once, signed by this device, through the
//! broker's own ingress, so the room's other devices and its host keep it
//! and a member who joins later fetches it from their history. When the
//! reference goes, it stops serving (what was already synced stays synced,
//! the limit rooms.md names).
//!
//! The other half, [`RecordCustody::fetch`], is how this device reaches a
//! record another member serves: it asks the route's holders here, then the
//! room's devices for their retained history of the route.
//!
//! What may be served is enforced here: a `session` record leaves only as a
//! reviewed, redacted duplicate (one naming its origin with
//! [`session_trace::REDACTED_FROM_RELATION`]), never as a recording.
//!
//! A record's route is an encrypted channel like any route of its room.
//! Custody takes part there as this device's person, the identity its
//! signer's certificate names, and reaches the channel's content keys the
//! way every participant of the shell does
//! ([`crate::shell::participant_keys`]): it seals what it publishes and
//! answers in the rooms it serves in, and opens what it fetches. Without a
//! signer it has no identity in a room, so it serves and opens nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Instant, SystemTime};

use automerge::AutoCommit;
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{DocumentMutation, SignedOp};
use ui::collaboration::CollabEvent;
use ui::record::{RecordId, RecordKind};

use crate::shell::collab_broker::{CrdtRoute, FanoutState, LocalParticipant, RoomSubscription};
use crate::shell::participant_keys::{ChannelOpener, ChannelSealer, Participant, ParticipantKeys};
use crate::shell::room_records::SHELL_PACKAGE_ID;
use crate::shell::state::{RoomId, WorkspaceTreeHandle};

/// Roster id of the shell's record custody participant.
pub const CUSTODY_AGENT: &str = "records";

/// Signs the document ops this device authors, with the key its device
/// certificate names. The key itself stays with the signer.
pub trait DeviceSigner: Send + Sync {
    fn author(&self) -> collab_types::Ed25519PublicKey;
    /// The person whose certificate names this device.
    fn person(&self) -> [u8; 32];
    /// # Errors
    ///
    /// When the op cannot be signed.
    fn sign(&self, op: &mut SignedOp) -> Result<(), String>;
}

type SignerSlot = Arc<Mutex<Option<Arc<dyn DeviceSigner>>>>;

/// Who custody takes part in a room as: the person `signer`'s certificate
/// names.
fn custodian(signer: &SignerSlot) -> Option<Participant> {
    lock(signer).as_ref().map(|signer| {
        Participant::Person(crate::shell::collab_broker_capnp::principal_id_for_root(
            signer.person(),
        ))
    })
}

/// Why custody refused a record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CustodyError {
    #[error("the document names no record")]
    NotARecord,
    #[error("a recording leaves this device only as its reviewed, redacted duplicate")]
    NotReviewed,
    #[error("record {0} is not one this device can reach")]
    Unreachable(String),
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// This device's public key: the key its person's certificate names, and so
/// the owner of the comment records it writes. `None` on a device without a
/// provisioned identity.
pub fn device_public_key() -> Option<[u8; 32]> {
    let paths = crate::firstboot::collaboration_identity_paths();
    tilo_user_identity::persist::read_device_key(paths.device_privkey.as_path())
        .ok()
        .map(|key| key.verifying_key().to_bytes())
}

/// The route a shell-owned record is served on in `room`.
pub fn record_route(room: &str, record: &RecordId) -> CrdtRoute {
    CrdtRoute::record(SHELL_PACKAGE_ID, room, record.as_str())
}

struct Held {
    doc: Vec<u8>,
    /// Rooms it is served in, with whether it was published there.
    served: BTreeMap<String, bool>,
}

/// A record being fetched from a room.
struct Fetch {
    routes: Vec<CrdtRoute>,
    frames: Vec<SignedOp>,
    done: Option<Vec<u8>>,
    asked_peers: bool,
}

#[derive(Default)]
struct State {
    held: BTreeMap<RecordId, Held>,
    fetching: BTreeMap<RecordId, Fetch>,
    seq: u64,
}

/// The shell's record custody. One per session.
pub struct RecordCustody {
    tree: WorkspaceTreeHandle,
    fanout: Weak<FanoutState>,
    signer: SignerSlot,
    keys: ParticipantKeys,
    state: Arc<Mutex<State>>,
    subscription: Arc<RoomSubscription>,
}

impl std::fmt::Debug for RecordCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordCustody")
            .field("held", &lock(&self.state).held.len())
            .finish_non_exhaustive()
    }
}

impl RecordCustody {
    pub fn attach(fanout: &Arc<FanoutState>, tree: WorkspaceTreeHandle) -> Arc<Self> {
        let state: Arc<Mutex<State>> = Arc::default();
        let signer: SignerSlot = Arc::default();
        let keys = ParticipantKeys::new(
            crate::shell::channel_encryption::ChannelKeyProvider::default(),
            Arc::clone(&tree),
        );
        let sink_state = Arc::clone(&state);
        let (sink_signer, sink_keys) = (Arc::clone(&signer), keys.clone());
        let holder_state = Arc::clone(&state);
        let holder_fanout = Arc::downgrade(fanout);
        let (holder_signer, holder_keys) = (Arc::clone(&signer), keys.clone());
        let subscription = fanout.subscribe_rooms_holding(
            LocalParticipant {
                agent: CUSTODY_AGENT.into(),
                label: "Shared records".into(),
            },
            Vec::new(),
            Box::new(move |event, route| {
                let opener = custodian(&sink_signer).map(|who| sink_keys.opener(who));
                observe(&sink_state, opener.as_ref(), event, route);
            }),
            Box::new(move |route| {
                let Some(who) = custodian(&holder_signer) else {
                    return false;
                };
                let sealer = holder_keys.sealer(who, &route.cluster_id);
                answer(&holder_state, &holder_fanout, &sealer, route)
            }),
        );
        Arc::new(Self {
            tree,
            fanout: Arc::downgrade(fanout),
            signer,
            keys,
            state,
            subscription,
        })
    }

    /// The device signer published records are signed with, whose person
    /// custody takes part in rooms as. Without one, nothing is served.
    pub fn set_signer(&self, signer: Arc<dyn DeviceSigner>) {
        *lock(&self.signer) = Some(signer);
    }

    /// The channel service whose content keys custody seals and opens
    /// records with: its broker's.
    pub fn set_channel_keys(
        &self,
        access: Option<crate::shell::channel_encryption::ChannelKeyAccess>,
    ) {
        self.keys.set_channels(access);
    }

    /// Whether `doc` may be served to a room that references it.
    ///
    /// # Errors
    ///
    /// When `doc` names no record, or is a recording rather than its
    /// reviewed duplicate.
    pub fn check_servable(doc: &AutoCommit) -> Result<RecordId, CustodyError> {
        let identity = ui::record::identity(doc).ok_or(CustodyError::NotARecord)?;
        if !servable(doc, &identity.kind) {
            return Err(CustodyError::NotReviewed);
        }
        Ok(identity.id)
    }

    /// Take up the records of `store` that rooms reference: each is held
    /// while some room this device is in references it and it may be
    /// served, and let go of otherwise.
    fn hold_referenced(&self, store: &ui::record::RecordStore) {
        let referenced: BTreeSet<RecordId> = {
            let tree = self.tree.read();
            tree.desktop
                .workspaces
                .iter()
                .filter(|workspace| !workspace.ephemeral)
                .filter_map(|workspace| tree.desktop.room_records_held.get(&workspace.room))
                .flat_map(|held| held.keys())
                .filter_map(|id| RecordId::parse(id))
                .collect()
        };
        let mut state = lock(&self.state);
        state.held.retain(|id, _| referenced.contains(id));
        for id in referenced {
            if state.held.contains_key(&id) {
                continue;
            }
            let Some(doc) = store.read(&id, |doc| {
                let mut doc = doc.clone();
                match Self::check_servable(&doc) {
                    Ok(named) if named == id => Some(doc.save()),
                    Ok(_) => None,
                    Err(error) => {
                        tracing::debug!(record = %id, %error, "custody: not serving a referenced record");
                        None
                    }
                }
            }) .flatten() else {
                continue;
            };
            state.held.insert(
                id,
                Held {
                    doc,
                    served: BTreeMap::new(),
                },
            );
        }
    }

    /// The document of a record this device holds.
    pub fn held(&self, id: &RecordId) -> Option<AutoCommit> {
        lock(&self.state)
            .held
            .get(id)
            .and_then(|held| AutoCommit::load(&held.doc).ok())
    }

    /// The rooms a held record is served in.
    pub fn served_in(&self, id: &RecordId) -> Vec<String> {
        lock(&self.state)
            .held
            .get(id)
            .map(|held| held.served.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Rooms this device is in whose index references `id`.
    fn referencing_rooms(&self, id: &RecordId) -> Vec<RoomId> {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .filter(|workspace| !workspace.ephemeral)
            .filter(|workspace| {
                tree.desktop
                    .room_records_held
                    .get(&workspace.room)
                    .is_some_and(|held| held.contains_key(id.as_str()))
            })
            .map(|workspace| workspace.room)
            .collect()
    }

    /// Frame-loop upkeep: serve held records in the rooms that reference
    /// them and stop in the rooms that no longer do; move fetches along.
    /// Returns whether a fetch finished.
    pub fn pump(&self, store: &ui::record::RecordStore) -> bool {
        self.hold_referenced(store);
        let ids: Vec<RecordId> = lock(&self.state).held.keys().cloned().collect();
        let mut publish: Vec<(RecordId, String)> = Vec::new();
        for id in &ids {
            let rooms: BTreeSet<String> = self
                .referencing_rooms(id)
                .into_iter()
                .map(|room| room.to_string())
                .collect();
            let mut state = lock(&self.state);
            let Some(held) = state.held.get_mut(id) else {
                continue;
            };
            held.served.retain(|room, _| rooms.contains(room));
            for room in rooms {
                let published = held.served.entry(room.clone()).or_insert(false);
                if !*published {
                    publish.push((id.clone(), room));
                }
            }
            drop(state);
        }
        for (id, room) in publish {
            if self.publish(&id, &room)
                && let Some(held) = lock(&self.state).held.get_mut(&id)
            {
                held.served.insert(room, true);
            }
        }
        self.resync_rooms();
        self.advance_fetches()
    }

    /// Publish a held record once on `room`'s route: to this device's own
    /// subscribers, into the channel's history, and to the room's devices.
    fn publish(&self, id: &RecordId, room: &str) -> bool {
        let Some(signer) = lock(&self.signer).clone() else {
            return false;
        };
        let Some(fanout) = self.fanout.upgrade() else {
            return false;
        };
        let Some(doc) = lock(&self.state).held.get(id).map(|held| held.doc.clone()) else {
            return false;
        };
        let route = record_route(room, id);
        let Some(who) = custodian(&self.signer) else {
            return false;
        };
        let sealer = self.keys.sealer(who, room);
        let frames = match signed_frames(&*signer, &sealer, &route, &doc, &self.state) {
            Ok(frames) => frames,
            Err(error) => {
                tracing::warn!(record = %id, %error, "custody: the record could not be signed");
                return false;
            }
        };
        let from = signer_id(&*signer);
        for frame in frames {
            if let Err(error) = fanout.accept_shell_document(&route, frame, signer.person(), &from)
            {
                tracing::warn!(record = %id, %room, %error, "custody: the record was refused on its route");
                return false;
            }
        }
        true
    }

    /// Hold the subscription to exactly the rooms custody serves or fetches in.
    fn resync_rooms(&self) {
        let state = lock(&self.state);
        let mut rooms: BTreeSet<String> = state
            .held
            .values()
            .flat_map(|held| held.served.keys().cloned())
            .collect();
        rooms.extend(
            state
                .fetching
                .values()
                .flat_map(|fetch| fetch.routes.iter().map(|route| route.cluster_id.clone())),
        );
        drop(state);
        self.subscription.set_rooms(rooms.into_iter().collect());
    }

    /// Reach record `id` through a room this device is in whose index
    /// references it. Returns the document once it has arrived; until then
    /// `None`, having asked the route's holders and the room's devices.
    ///
    /// # Errors
    ///
    /// When no room this device is in references `id`.
    pub fn fetch(&self, id: &RecordId) -> Result<Option<AutoCommit>, CustodyError> {
        if let Some(held) = self.held(id) {
            return Ok(Some(held));
        }
        let done = lock(&self.state)
            .fetching
            .get(id)
            .and_then(|fetch| fetch.done.clone());
        if let Some(done) = done {
            return Ok(AutoCommit::load(&done).ok());
        }
        let rooms = self.referencing_rooms(id);
        if rooms.is_empty() {
            return Err(CustodyError::Unreachable(id.as_str().to_string()));
        }
        let fresh = !lock(&self.state).fetching.contains_key(id);
        if fresh {
            let routes: Vec<CrdtRoute> = rooms
                .iter()
                .map(|room| record_route(&room.to_string(), id))
                .collect();
            lock(&self.state).fetching.insert(
                id.clone(),
                Fetch {
                    routes: routes.clone(),
                    frames: Vec::new(),
                    done: None,
                    asked_peers: false,
                },
            );
            self.resync_rooms();
            if let Some(fanout) = self.fanout.upgrade() {
                for route in &routes {
                    fanout.request_route_snapshot(route);
                }
            }
        }
        self.advance_fetches();
        Ok(lock(&self.state)
            .fetching
            .get(id)
            .and_then(|fetch| fetch.done.clone())
            .and_then(|done| AutoCommit::load(&done).ok()))
    }

    /// Ask the rooms' devices for retained history where nothing answered
    /// locally, and complete fetches from what history holds.
    fn advance_fetches(&self) -> bool {
        let Some(fanout) = self.fanout.upgrade() else {
            return false;
        };
        let mut finished = false;
        let opener = custodian(&self.signer).map(|who| self.keys.opener(who));
        let pending: Vec<(RecordId, Vec<CrdtRoute>, bool)> = lock(&self.state)
            .fetching
            .iter()
            .filter(|(_, fetch)| fetch.done.is_none())
            .map(|(id, fetch)| (id.clone(), fetch.routes.clone(), fetch.asked_peers))
            .collect();
        for (id, routes, asked) in pending {
            let mut history: Vec<SignedOp> = Vec::new();
            for route in &routes {
                history.extend(opened(opener.as_ref(), route, fanout.route_history(route)));
            }
            let mut state = lock(&self.state);
            let Some(fetch) = state.fetching.get_mut(&id) else {
                continue;
            };
            fetch.frames.extend(history);
            if let Some(doc) = assemble(&fetch.frames) {
                fetch.done = Some(doc);
                finished = true;
                continue;
            }
            if !asked {
                fetch.asked_peers = true;
                drop(state);
                for route in &routes {
                    fanout.request_route_history(route);
                }
            }
        }
        finished
    }
}

impl Drop for RecordCustody {
    fn drop(&mut self) {
        if let Some(fanout) = self.fanout.upgrade() {
            fanout.unsubscribe(&self.subscription);
        }
    }
}

/// Whether a record of `kind` may be served. A session is only ever served
/// as a reviewed, redacted duplicate: a new record naming the recording it
/// was made from.
fn servable(doc: &AutoCommit, kind: &RecordKind) -> bool {
    if kind.as_str() != session_trace::SESSION_RECORD_KIND {
        return true;
    }
    ui::record::references(doc)
        .iter()
        .any(|reference| reference.relation == session_trace::REDACTED_FROM_RELATION)
        && session_trace::read(doc).is_ok()
}

fn signer_id(signer: &dyn DeviceSigner) -> String {
    hex::encode(signer.author().as_bytes())
}

/// The signed frames carrying `doc` as a snapshot on `route`, sealed for its
/// channel.
fn signed_frames(
    signer: &dyn DeviceSigner,
    sealer: &ChannelSealer,
    route: &CrdtRoute,
    doc: &[u8],
    state: &Mutex<State>,
) -> Result<Vec<SignedOp>, String> {
    let payloads = DocumentMutation::Snapshot(doc.to_vec())
        .encode_frames()
        .map_err(|error| error.to_string())?;
    let artifact =
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
    let attestation =
        collab_types::document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
    let class = collab_types::document_op_class_for_record(&route.record_id);
    // Sequence numbers only rise per author and channel; wall-clock
    // microseconds keep them rising across restarts too.
    let now = SystemTime::now();
    let clock = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let mut frames = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let seq = {
            let mut state = lock(state);
            state.seq = state.seq.saturating_add(1).max(clock);
            state.seq
        };
        let mut op = sealed_op(
            sealer,
            route,
            (artifact, attestation),
            signer.author(),
            class,
            (now, seq),
            &payload,
        )?;
        signer.sign(&mut op)?;
        frames.push(op);
    }
    Ok(frames)
}

/// An unsigned op carrying `cleartext` on `route`, sealed for its channel.
fn sealed_op(
    sealer: &ChannelSealer,
    route: &CrdtRoute,
    (artifact, attestation): (collab_types::ArtifactId, collab_types::AttestationId),
    author: collab_types::Ed25519PublicKey,
    class: collab_types::OpClass,
    (now, seq): (SystemTime, u64),
    cleartext: &[u8],
) -> Result<SignedOp, String> {
    let (payload, encryption) = sealer
        .seal(route, artifact, cleartext)
        .map_err(|error| error.to_string())?;
    Ok(match encryption {
        Some(encryption) => SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            class,
            now,
            seq,
            encryption,
        ),
        None => SignedOp::new_unsigned(payload, artifact, author, attestation, class, now, seq),
    })
}

/// `ops` of `route`, opened where `opener` can: what custody assembles a
/// fetched record from.
fn opened(opener: Option<&ChannelOpener>, route: &CrdtRoute, ops: Vec<SignedOp>) -> Vec<SignedOp> {
    ops.into_iter()
        .map(|op| match opener {
            Some(opener) if op.encryption.is_some() => opener.opened(route, &op).unwrap_or(op),
            _ => op,
        })
        .collect()
}

/// A late joiner on one of custody's routes: answer with the held record.
fn answer(
    state: &Mutex<State>,
    fanout: &Weak<FanoutState>,
    sealer: &ChannelSealer,
    route: &CrdtRoute,
) -> bool {
    if route.app_id != SHELL_PACKAGE_ID {
        return false;
    }
    let Some(id) = RecordId::parse(&route.record_id) else {
        return false;
    };
    let frames = {
        let state = lock(state);
        let Some(held) = state.held.get(&id) else {
            return false;
        };
        if !held.served.contains_key(&route.cluster_id) {
            return false;
        }
        let doc = held.doc.clone();
        drop(state);
        let Ok(payloads) = DocumentMutation::Snapshot(doc).encode_frames() else {
            return false;
        };
        payloads
    };
    let Some(fanout) = fanout.upgrade() else {
        return false;
    };
    // Delivered on this device only: this answers a request here, and the
    // published copy already reached the room's other devices.
    let artifact =
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
    let attestation =
        collab_types::document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
    for (seq, payload) in (1_u64..).zip(frames) {
        let envelope = match sealed_op(
            sealer,
            route,
            (artifact, attestation),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::document_op_class_for_record(&route.record_id),
            (SystemTime::now(), seq),
            &payload,
        ) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::debug!(%error, "custody: a record could not be sealed for its route");
                return false;
            }
        };
        fanout.fanout_to_route(
            &CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id: CUSTODY_AGENT.into(),
                decrypted_payload: None,
            },
            u64::MAX,
            route,
        );
    }
    true
}

/// What arrives on a route custody is fetching, opened by `opener`.
fn observe(
    state: &Mutex<State>,
    opener: Option<&ChannelOpener>,
    event: &CollabEvent,
    route: &CrdtRoute,
) {
    let ops: Vec<SignedOp> = match event {
        CollabEvent::Document { envelope, .. } => vec![(**envelope).clone()],
        CollabEvent::ScrollbackBatch { ops, .. } => ops.iter().map(|op| (**op).clone()).collect(),
        _ => return,
    };
    let Some(id) = RecordId::parse(&route.record_id) else {
        return;
    };
    let fetching = lock(state)
        .fetching
        .get(&id)
        .is_some_and(|fetch| fetch.done.is_none() && fetch.routes.contains(route));
    if !fetching {
        return;
    }
    let ops = opened(opener, route, ops);
    let mut state = lock(state);
    let Some(fetch) = state.fetching.get_mut(&id) else {
        return;
    };
    if fetch.done.is_some() {
        return;
    }
    fetch.frames.extend(ops);
    if let Some(doc) = assemble(&fetch.frames) {
        fetch.done = Some(doc);
    }
    drop(state);
}

/// The document a set of frames completes: a whole snapshot, or every
/// segment of one.
fn assemble(frames: &[SignedOp]) -> Option<Vec<u8>> {
    let mut reassembler = DocumentReassembler::default();
    let now = Instant::now();
    for frame in frames {
        if frame.encryption.is_some() {
            continue;
        }
        match DocumentFrame::decode(&frame.payload).ok()? {
            DocumentFrame::Whole(DocumentMutation::Snapshot(bytes)) => return Some(bytes),
            DocumentFrame::Whole(DocumentMutation::Incremental(_)) => {}
            DocumentFrame::Segment(segment) => {
                if let Ok(Some(done)) = reassembler.accept(frame, segment, now)
                    && let Ok(DocumentMutation::Snapshot(bytes)) =
                        DocumentMutation::decode(&done.payload)
                {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::record::{RecordCheckout, RecordRef};

    fn session_trace() -> session_trace::SessionTrace {
        session_trace::SessionTrace {
            origin: session_trace::Origin::Explicit,
            started_unix_ms: 0,
            duration: session_trace::Offset(1),
            environment: session_trace::Environment::default(),
            snapshots: Vec::new(),
            timeline: Vec::new(),
            tags: Vec::new(),
            layer: session_trace::SessionLayer::Logic,
            redactions: session_trace::Redactions::default(),
            truncated: false,
        }
    }

    fn record(kind: &str, reviewed: bool) -> AutoCommit {
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new(kind));
        let mut doc = AutoCommit::new();
        ui::record::init(&mut doc, &checkout.id, &checkout.kind, "personal");
        if reviewed {
            ui::record::add_reference(
                &mut doc,
                &RecordRef::new(RecordId::minted(), session_trace::REDACTED_FROM_RELATION),
            );
        }
        if kind == session_trace::SESSION_RECORD_KIND {
            session_trace::write(&mut doc, &session_trace()).expect("session");
        }
        doc
    }

    #[test]
    fn a_recording_is_served_only_as_its_reviewed_duplicate() {
        assert_eq!(
            RecordCustody::check_servable(&record(session_trace::SESSION_RECORD_KIND, false)),
            Err(CustodyError::NotReviewed),
            "an unreviewed recording never leaves the device"
        );
        assert!(
            RecordCustody::check_servable(&record(session_trace::SESSION_RECORD_KIND, true))
                .is_ok()
        );
        assert!(
            RecordCustody::check_servable(&record("note", false)).is_ok(),
            "other shell records are served as they are"
        );
        assert_eq!(
            RecordCustody::check_servable(&AutoCommit::new()),
            Err(CustodyError::NotARecord)
        );
    }

    /// Signs as the fixture's device.
    struct Device;

    impl DeviceSigner for Device {
        fn author(&self) -> collab_types::Ed25519PublicKey {
            tilo_action_broker::channel_encryption::testing::public(
                crate::shell::participant_keys::tests::DEVICE,
            )
        }
        fn person(&self) -> [u8; 32] {
            *self.author().as_bytes()
        }
        fn sign(&self, op: &mut SignedOp) -> Result<(), String> {
            let key = tilo_action_broker::channel_encryption::testing::key(
                crate::shell::participant_keys::tests::DEVICE,
            );
            collab_types::sign_op(op, &key).map_err(|error| error.to_string())
        }
    }

    /// A record custody publishes in an encrypted room is sealed for its
    /// route, and custody reassembles it from what it fetches back.
    #[test]
    fn a_record_round_trips_sealed_through_an_encrypted_room() {
        use crate::shell::participant_keys::tests::Fixture;

        let fixture = Fixture::new(collab_types::ChannelClass::Encrypted);
        let signer: SignerSlot = Arc::new(Mutex::new(Some(Arc::new(Device) as _)));
        let who = custodian(&signer).expect("custody takes part as the device's person");
        assert_eq!(who, fixture.person());
        let id = RecordId::minted();
        let route = record_route(&fixture.room, &id);
        let doc = vec![3_u8; collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES + 9];
        let frames = signed_frames(
            &Device,
            &fixture.keys.sealer(who.clone(), &fixture.room),
            &route,
            &doc,
            &Mutex::new(State::default()),
        )
        .expect("signed");
        assert!(frames.len() > 1);
        assert!(frames.iter().all(|frame| frame.encryption.is_some()));
        assert_eq!(assemble(&frames), None, "sealed frames are not a document");
        let opened = opened(Some(&fixture.keys.opener(who)), &route, frames);
        assert_eq!(assemble(&opened), Some(doc));
    }

    #[test]
    fn a_snapshot_is_reassembled_from_its_frames() {
        let bytes = vec![7_u8; collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES * 2 + 3];
        let frames: Vec<SignedOp> = DocumentMutation::Snapshot(bytes.clone())
            .encode_frames()
            .expect("frames")
            .into_iter()
            .zip(1_u64..)
            .map(|(payload, seq)| {
                SignedOp::new_unsigned(
                    payload,
                    collab_types::ArtifactId(uuid::Uuid::nil()),
                    collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                    collab_types::AttestationId(uuid::Uuid::nil()),
                    collab_types::OpClass::Splice,
                    SystemTime::UNIX_EPOCH,
                    seq,
                )
            })
            .collect();
        assert!(frames.len() > 1);
        assert_eq!(assemble(&frames), Some(bytes));
        assert_eq!(
            assemble(frames.get(..1).unwrap_or_default()),
            None,
            "an incomplete transfer is not a document"
        );
    }
}
