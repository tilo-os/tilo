//! The replay peer: the recorded participant, played back into a replay room.
//!
//! A replay peer is an in-shell agent participant, [`REPLAY_AGENT`], admitted
//! to one replay room. It holds the recorded documents and publishes, through
//! the broker, what the recording saw: the document as a late joiner's
//! snapshot, then each recorded change and presence update as the timeline
//! reaches it. The viewer's app follows the recorded participant exactly as it
//! would follow a live one, with [`FollowMode::Replay`].
//!
//! Recorded envelopes are bound to the room they were recorded in (their
//! artifact and attestation ids are derived from it), so they are never
//! forwarded. Each recorded change is re-framed onto the replay room's route
//! in an envelope signed by this peer's own session key: the Automerge change
//! bytes, and the authorship they carry, are exactly what was recorded, and
//! the transport says who is actually speaking. The replay room's routes are
//! encrypted channels like any room's, so each re-framed change is sealed for
//! the channel it goes out on, with a [`ChannelSealer`] that writes in the
//! replay room and nowhere else. Presence goes out, only ever
//! in the replay room, as [`replay_participant`] of the recorded participant:
//! the recorded identity, named as a replay of it. It is never the id of a
//! live person, so the person replaying their own session can follow it.
//!
//! Seeking backwards rebuilds the documents as they were at the target. The
//! peer keeps a few [checkpoints](CHECKPOINT_EVERY) of its documents as the
//! playhead passes them, so a short scrub back starts from the nearest one
//! rather than from the beginning of the recording.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Instant, SystemTime};

use automerge::AutoCommit;
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{DocumentMutation, OpClass, SignedOp};
use ed25519_dalek::SigningKey;
use session_trace::{Event, PresenceChange, PresenceUpdate, Route, SessionTrace, SnapshotPoint};
use tilo_automerge::ApplyIncoming as _;
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;
use ui::view::FollowMode;

use crate::shell::collab_broker::{CrdtRoute, FanoutState, LocalParticipant, RoomSubscription};
use crate::shell::participant_keys::ChannelSealer;

/// The replay peer's agent principal id: the capability subject it is
/// admitted to a replay room as, and its roster id.
pub const REPLAY_AGENT: &str = "replay";
/// The admission a replay room grants its peer, in the shape every agent
/// admission has. It publishes documents, so it is a contributor.
pub const REPLAY_ADMISSION: &str = "agent-member:replay:contributor";

/// The participant a replay of `recorded` speaks as.
pub fn replay_participant(recorded: &str) -> String {
    format!("{REPLAY_AGENT}:{recorded}")
}

/// Why a session cannot be replayed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("the session recorded no document to replay")]
    Empty,
    #[error(
        "the recording of {app} is end-to-end encrypted, so only a reviewed copy of it can be replayed"
    )]
    Encrypted { app: String },
    #[error("the recorded document of {app} could not be read: {reason}")]
    Malformed { app: String, reason: String },
}

/// Every this many timeline entries the peer keeps its documents as they
/// stand, so a seek backwards resumes from there.
pub const CHECKPOINT_EVERY: usize = 64;
/// The checkpoints kept at most; the one farthest from a new one goes.
const CHECKPOINTS_KEPT: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The documents and what the recorded participant showed once timeline
/// entries `0..at` happened, where `at` is the checkpoint's key.
struct Checkpoint {
    docs: Vec<Vec<u8>>,
    presence: BTreeMap<(String, Vec<u8>), (usize, PresenceUpdate)>,
}

/// One recorded document route and its state at the playhead.
struct RouteDoc {
    recorded: Route,
    /// The document as the recording started.
    start: Vec<u8>,
    /// The document with every published change applied.
    doc: AutoCommit,
    /// The replay room's route an app announced for this document.
    live: Option<CrdtRoute>,
    seq: u64,
}

/// A timeline entry, decoded once.
enum Decoded {
    /// A complete recorded mutation of `route` (an index into the routes).
    Mutation {
        route: usize,
        from: String,
        mutation: DocumentMutation,
    },
    Presence(PresenceUpdate),
    /// A segment of a mutation that completes later, or an inspect keyframe:
    /// nothing to publish on its own.
    Nothing,
}

struct PeerState {
    routes: Vec<RouteDoc>,
    timeline: Vec<Decoded>,
    /// Timeline entries `0..applied` are reflected in the documents.
    applied: usize,
    /// The last presence per `(participant, scope)`, in first-seen order, so
    /// an app that joins late sees what the recorded participant showed.
    presence: BTreeMap<(String, Vec<u8>), (usize, PresenceUpdate)>,
    key: SigningKey,
    /// Seals what the peer says, in its replay room only.
    sealer: ChannelSealer,
    /// Checkpoints by the number of entries they reflect.
    checkpoints: BTreeMap<usize, Checkpoint>,
}

impl PeerState {
    /// Apply timeline entry `index`, as [`advance`] does, keeping a
    /// checkpoint when the documents reach one.
    fn step(
        &mut self,
        index: usize,
        outbound: Option<&mut Vec<(CollabEvent, CrdtRoute)>>,
        room: &str,
    ) {
        advance(self, index, outbound, room);
        let at = index.saturating_add(1);
        if at.is_multiple_of(CHECKPOINT_EVERY) && !self.checkpoints.contains_key(&at) {
            self.keep_checkpoint(at);
        }
    }

    fn keep_checkpoint(&mut self, at: usize) {
        let checkpoint = Checkpoint {
            docs: self
                .routes
                .iter_mut()
                .map(|route| route.doc.save())
                .collect(),
            presence: self.presence.clone(),
        };
        self.checkpoints.insert(at, checkpoint);
        while self.checkpoints.len() > CHECKPOINTS_KEPT {
            let Some(farthest) = self
                .checkpoints
                .keys()
                .copied()
                .max_by_key(|kept| kept.abs_diff(at))
            else {
                break;
            };
            self.checkpoints.remove(&farthest);
        }
    }

    /// Stand at the latest checkpoint at or before `end`, or at the start of
    /// the recording. Returns the entries the documents then reflect.
    fn restore(&mut self, end: usize) -> usize {
        let from = self
            .checkpoints
            .range(..=end)
            .next_back()
            .map(|(at, checkpoint)| (*at, checkpoint));
        let (at, docs, presence) = match from {
            Some((at, checkpoint)) => (at, checkpoint.docs.clone(), checkpoint.presence.clone()),
            None => (
                0,
                self.routes
                    .iter()
                    .map(|route| route.start.clone())
                    .collect(),
                BTreeMap::new(),
            ),
        };
        for (route, bytes) in self.routes.iter_mut().zip(docs) {
            route.doc = AutoCommit::load(&bytes).unwrap_or_else(|_| AutoCommit::new());
            route.live = None;
        }
        self.presence = presence;
        self.applied = at;
        at
    }
}

/// A recorded session being played into one replay room.
pub struct ReplayPeer {
    room: String,
    fanout: Weak<FanoutState>,
    state: Arc<Mutex<PeerState>>,
    subscription: Arc<RoomSubscription>,
    followed: String,
    recorded: String,
}

impl std::fmt::Debug for ReplayPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayPeer")
            .field("room", &self.room)
            .field("followed", &self.followed)
            .finish_non_exhaustive()
    }
}

impl ReplayPeer {
    /// Join `room` as the replay peer of `trace`, speaking there through
    /// `sealer`. The documents start as the recording did; nothing is
    /// published until an app asks or the playhead moves.
    ///
    /// # Errors
    ///
    /// When the trace holds no document, holds ciphertext, or a recorded
    /// document cannot be read.
    pub fn join(
        fanout: &Arc<FanoutState>,
        room: String,
        trace: &SessionTrace,
        sealer: ChannelSealer,
    ) -> Result<Arc<Self>, ReplayError> {
        let (routes, timeline) = decode(trace)?;
        let recorded = followed_participant(trace).unwrap_or_else(|| REPLAY_AGENT.to_string());
        let followed = replay_participant(&recorded);
        let state = Arc::new(Mutex::new(PeerState {
            routes,
            timeline,
            applied: 0,
            presence: BTreeMap::new(),
            key: session_key(),
            sealer,
            checkpoints: BTreeMap::new(),
        }));
        let holder_state = Arc::clone(&state);
        let holder_fanout = Arc::downgrade(fanout);
        let holder_room = room.clone();
        let holder_followed = followed.clone();
        let subscription = fanout.subscribe_rooms_holding(
            LocalParticipant {
                agent: REPLAY_AGENT.into(),
                label: format!("Replay of {recorded}"),
            },
            vec![room.clone()],
            // The peer speaks in its room; it has nothing to learn from it.
            Box::new(|_, _| {}),
            Box::new(move |route| {
                answer_snapshot(
                    &holder_state,
                    &holder_fanout,
                    &holder_room,
                    &holder_followed,
                    route,
                )
            }),
        );
        Ok(Arc::new(Self {
            room,
            fanout: Arc::downgrade(fanout),
            state,
            subscription,
            followed,
            recorded,
        }))
    }

    /// The participant who was recorded.
    pub fn recorded(&self) -> &str {
        &self.recorded
    }

    /// The participant the viewer follows: the replay of the recorded one.
    pub fn followed(&self) -> &str {
        &self.followed
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    /// Timeline entries reflected in the documents.
    pub fn applied(&self) -> usize {
        lock(&self.state).applied
    }

    /// Publish timeline entries `range` in order: each change is applied to
    /// its document and sent on its live route, each presence update is sent
    /// to the room. Entries already applied are skipped.
    pub fn publish(&self, range: std::ops::Range<usize>) {
        let mut outbound: Vec<(CollabEvent, CrdtRoute)> = Vec::new();
        {
            let mut state = lock(&self.state);
            let start = range.start.max(state.applied);
            let end = range.end.min(state.timeline.len());
            for index in start..end {
                state.step(index, Some(&mut outbound), &self.room);
            }
            state.applied = state.applied.max(end);
        }
        self.send(outbound);
    }

    /// Rebuild the documents as they were after timeline entries `0..end`,
    /// publishing nothing: the apps that asked before hold later state, so
    /// the caller starts fresh ones, and they ask for this. The rebuild
    /// starts at the nearest checkpoint before `end`; returns the entries
    /// that checkpoint reflected (0 when it started from the beginning).
    pub fn rewind(&self, end: usize) -> usize {
        let mut state = lock(&self.state);
        let end = end.min(state.timeline.len());
        let from = state.restore(end);
        for index in from..end {
            state.step(index, None, &self.room);
        }
        state.applied = end;
        from
    }

    /// Take over the checkpoints `previous`, a peer of the same recording,
    /// kept: a seek backwards reopens the room with a new peer, and what
    /// the old one passed still stands.
    pub fn inherit_checkpoints(&self, previous: &Self) {
        if std::ptr::eq(self, previous) {
            return;
        }
        let kept = std::mem::take(&mut lock(&previous.state).checkpoints);
        lock(&self.state).checkpoints.extend(kept);
    }

    /// The timeline positions the peer holds checkpoints at.
    pub fn checkpoints(&self) -> Vec<usize> {
        lock(&self.state).checkpoints.keys().copied().collect()
    }

    /// Ask every app in the room to follow the recorded participant.
    pub fn follow(&self) {
        self.send(vec![follow_event(&self.room, &self.followed)]);
    }

    /// The replayed document of `recorded`, as the playhead has it.
    pub fn document(&self, recorded: &Route) -> Option<AutoCommit> {
        lock(&self.state)
            .routes
            .iter()
            .find(|route| &route.recorded == recorded)
            .map(|route| route.doc.clone())
    }

    /// Recorded routes and the replay room's routes apps announced for them.
    pub fn routes(&self) -> Vec<(Route, Option<CrdtRoute>)> {
        lock(&self.state)
            .routes
            .iter()
            .map(|route| (route.recorded.clone(), route.live.clone()))
            .collect()
    }

    fn send(&self, outbound: Vec<(CollabEvent, CrdtRoute)>) {
        let Some(fanout) = self.fanout.upgrade() else {
            return;
        };
        for (event, route) in outbound {
            fanout.fanout_to_route(&event, u64::MAX, &route);
        }
    }
}

impl Drop for ReplayPeer {
    fn drop(&mut self) {
        if let Some(fanout) = self.fanout.upgrade() {
            fanout.unsubscribe(&self.subscription);
        }
    }
}

fn follow_event(room: &str, followed: &str) -> (CollabEvent, CrdtRoute) {
    (
        CollabEvent::FollowChanged {
            peer_id: Some(followed.to_string()),
            mode: FollowMode::Replay,
        },
        CrdtRoute::cluster(room.to_string()),
    )
}

/// A fresh signing key for this replay. It names the replay, not anyone.
fn session_key() -> SigningKey {
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use rand_core::Rng as _;
    let mut seed = [0_u8; 32];
    UnwrapErr(SysRng).fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

/// Apply timeline entry `index` to the documents, collecting what to send
/// when `outbound` is given.
fn advance(
    state: &mut PeerState,
    index: usize,
    outbound: Option<&mut Vec<(CollabEvent, CrdtRoute)>>,
    room: &str,
) {
    let PeerState {
        routes,
        timeline,
        presence,
        key,
        sealer,
        ..
    } = state;
    match timeline.get(index) {
        Some(Decoded::Mutation {
            route,
            from,
            mutation,
        }) => {
            let Some(target) = routes.get_mut(*route) else {
                return;
            };
            apply(&mut target.doc, mutation);
            if let Some(outbound) = outbound
                && let Some(live) = target.live.clone()
            {
                let from = replay_participant(from);
                outbound.extend(
                    frame(key, sealer, &live, &mut target.seq, mutation)
                        .into_iter()
                        .map(|envelope| (document_event(envelope, &from), live.clone())),
                );
            }
        }
        Some(Decoded::Presence(update)) => {
            let slot = (update.participant.clone(), update.scope.clone());
            let order = presence.get(&slot).map_or(index, |(order, _)| *order);
            presence.insert(slot, (order, update.clone()));
            if let Some(outbound) = outbound
                && let Some(event) = presence_event(update)
            {
                outbound.push((event, CrdtRoute::cluster(room.to_string())));
            }
        }
        Some(Decoded::Nothing) | None => {}
    }
}

fn apply(doc: &mut AutoCommit, mutation: &DocumentMutation) {
    let applied = match mutation {
        DocumentMutation::Incremental(bytes) => doc.load_incoming(bytes).map(|_| ()),
        DocumentMutation::Snapshot(bytes) => {
            AutoCommit::load(bytes).and_then(|mut other| doc.merge_incoming(&mut other).map(|_| ()))
        }
    };
    if let Err(error) = applied {
        tracing::warn!(%error, "replay: a recorded change did not apply");
    }
}

fn document_event(envelope: SignedOp, from: &str) -> CollabEvent {
    CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: from.to_string(),
        decrypted_payload: None,
    }
}

/// The signed frames carrying `mutation` on `live`.
fn frame(
    key: &SigningKey,
    sealer: &ChannelSealer,
    live: &CrdtRoute,
    seq: &mut u64,
    mutation: &DocumentMutation,
) -> Vec<SignedOp> {
    let payloads = match mutation.encode_frames() {
        Ok(payloads) => payloads,
        Err(error) => {
            tracing::warn!(%error, "replay: a recorded change is too large to carry");
            return Vec::new();
        }
    };
    let artifact =
        collab_types::document_artifact_id(&live.app_id, &live.cluster_id, &live.record_id);
    let attestation =
        collab_types::document_attestation_id(&live.app_id, &live.cluster_id, &live.record_id);
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    payloads
        .into_iter()
        .filter_map(|payload| {
            let (payload, encryption) = sealer
                .seal(live, artifact, &payload)
                .inspect_err(|error| {
                    tracing::warn!(%error, "replay: a recorded change could not be sealed");
                })
                .ok()?;
            *seq = seq.saturating_add(1);
            let (now, seq) = (SystemTime::now(), *seq);
            let mut op = match encryption {
                Some(encryption) => SignedOp::new_unsigned_with_encryption(
                    payload,
                    artifact,
                    author,
                    attestation,
                    OpClass::Splice,
                    now,
                    seq,
                    encryption,
                ),
                None => SignedOp::new_unsigned(
                    payload,
                    artifact,
                    author,
                    attestation,
                    OpClass::Splice,
                    now,
                    seq,
                ),
            };
            collab_types::sign_op(&mut op, key).ok()?;
            Some(op)
        })
        .collect()
}

/// The broker asks for `route`'s document on behalf of an app that joined:
/// hand it the document at the playhead, then what the recorded participant
/// currently shows, then who to follow.
fn answer_snapshot(
    state: &Mutex<PeerState>,
    fanout: &Weak<FanoutState>,
    room: &str,
    followed: &str,
    route: &CrdtRoute,
) -> bool {
    if route.cluster_id != room || route.app_id.is_empty() {
        return false;
    }
    let outbound = {
        let mut state = lock(state);
        let Some(index) = resolve(&state.routes, route) else {
            return false;
        };
        let PeerState {
            routes,
            presence,
            key,
            sealer,
            ..
        } = &mut *state;
        let Some(target) = routes.get_mut(index) else {
            return false;
        };
        target.live = Some(route.clone());
        let snapshot = DocumentMutation::Snapshot(target.doc.save());
        let from = REPLAY_AGENT;
        let mut outbound: Vec<(CollabEvent, CrdtRoute)> =
            frame(key, sealer, route, &mut target.seq, &snapshot)
                .into_iter()
                .map(|envelope| (document_event(envelope, from), route.clone()))
                .collect();
        let mut shown: Vec<&(usize, PresenceUpdate)> = presence.values().collect();
        shown.sort_by_key(|(order, _)| *order);
        outbound.extend(shown.into_iter().filter_map(|(_, update)| {
            presence_event(update).map(|event| (event, CrdtRoute::cluster(room.to_string())))
        }));
        outbound.push(follow_event(room, followed));
        drop(state);
        outbound
    };
    // Sent with the state released: delivery reaches in-process sinks.
    if let Some(fanout) = fanout.upgrade() {
        for (event, target) in outbound {
            fanout.fanout_to_route(&event, u64::MAX, &target);
        }
    }
    true
}

/// Which recorded document an app's route in the replay room stands for:
/// the one already bound to it, else the recorded route of the same app and
/// record, else the only recorded route of that app of the same shape. A
/// room route (no record, carrying the room index) stands only for a
/// recorded room route and a record route only for a record route: an app
/// joins its room route too, and that must not take the record it shows.
fn resolve(routes: &[RouteDoc], live: &CrdtRoute) -> Option<usize> {
    if let Some(index) = routes
        .iter()
        .position(|route| route.live.as_ref() == Some(live))
    {
        return Some(index);
    }
    let unbound = |route: &&RouteDoc| {
        route.recorded.app_id == live.app_id
            && route.live.is_none()
            && route.recorded.record_id.is_empty() == live.record_id.is_empty()
    };
    if let Some(index) = routes
        .iter()
        .position(|route| unbound(&route) && route.recorded.record_id == live.record_id)
    {
        return Some(index);
    }
    let mut candidates = routes
        .iter()
        .enumerate()
        .filter(|(_, route)| unbound(route));
    match (candidates.next(), candidates.next()) {
        (Some((index, _)), None) => Some(index),
        _ => None,
    }
}

/// The participant a viewer follows: whoever the recording saw looking
/// (their first View), else whoever changed a document first.
fn followed_participant(trace: &SessionTrace) -> Option<String> {
    trace
        .events()
        .find_map(|entry| match &entry.event {
            Event::Presence(update) if update.change == PresenceChange::View => {
                Some(update.participant.clone())
            }
            _ => None,
        })
        .or_else(|| {
            trace.events().find_map(|entry| match &entry.event {
                Event::Op(op) => Some(op.from.clone()),
                _ => None,
            })
        })
}

/// A recorded presence update in the SDK's own terms.
fn presence_event(update: &PresenceUpdate) -> Option<CollabEvent> {
    let key: PresenceKey = postcard::from_bytes(&update.scope).ok()?;
    let narrow = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
    let payload = match &update.change {
        // A View rides in its key; the payload is inert.
        PresenceChange::View | PresenceChange::Arrived => PresencePayload::Arrived,
        PresenceChange::Left => PresencePayload::Left,
        PresenceChange::Pointer { x, y } => PresencePayload::Pointer { x: *x, y: *y },
        PresenceChange::Caret { pos } => PresencePayload::Caret { pos: narrow(*pos) },
        PresenceChange::Selection { start, end } => PresencePayload::Selection {
            start: narrow(*start),
            end: narrow(*end),
        },
        PresenceChange::Typing { start, end } => PresencePayload::Typing {
            start: narrow(*start),
            end: narrow(*end),
        },
        PresenceChange::Following { target } => PresencePayload::Following {
            target: target.clone(),
        },
        PresenceChange::Interaction(interaction) => {
            PresencePayload::Interaction(interaction.clone())
        }
        PresenceChange::Status { text, automatic } => PresencePayload::Status {
            text: text.clone(),
            automatic: *automatic,
        },
    };
    Some(CollabEvent::RemotePresenceUpdate {
        user_id: replay_participant(&update.participant),
        key,
        payload,
    })
}

/// Decode every recorded document and timeline entry once, refusing a trace
/// replay could not reproduce.
fn decode(trace: &SessionTrace) -> Result<(Vec<RouteDoc>, Vec<Decoded>), ReplayError> {
    let routes: Vec<Route> = trace
        .routes()
        .into_iter()
        .filter(|route| !route.app_id.is_empty())
        .collect();
    if routes.is_empty() {
        return Err(ReplayError::Empty);
    }
    let mut reassembler = DocumentReassembler::default();
    let now = Instant::now();
    let mut timeline = Vec::with_capacity(trace.timeline.len());
    for entry in &trace.timeline {
        let decoded = match &entry.event {
            Event::Op(op) => match routes.iter().position(|route| *route == op.route) {
                Some(route) => match mutation_of(&op.route, &op.envelope, &mut reassembler, now)? {
                    Some(mutation) => Decoded::Mutation {
                        route,
                        from: op.from.clone(),
                        mutation,
                    },
                    None => Decoded::Nothing,
                },
                None => Decoded::Nothing,
            },
            Event::Presence(update) => Decoded::Presence(update.clone()),
            Event::Inspect(_) => Decoded::Nothing,
        };
        timeline.push(decoded);
    }
    let mut docs = Vec::with_capacity(routes.len());
    for route in routes {
        let start = starting_document(trace, &route, &timeline, docs.len())?;
        docs.push(RouteDoc {
            doc: AutoCommit::load(&start).unwrap_or_else(|_| AutoCommit::new()),
            start,
            recorded: route,
            live: None,
            seq: 0,
        });
    }
    Ok((docs, timeline))
}

fn malformed(route: &Route, reason: impl std::fmt::Display) -> ReplayError {
    ReplayError::Malformed {
        app: route.app_id.clone(),
        reason: reason.to_string(),
    }
}

/// The complete mutation `envelope` carries, reassembling segments.
fn mutation_of(
    route: &Route,
    envelope: &SignedOp,
    reassembler: &mut DocumentReassembler,
    now: Instant,
) -> Result<Option<DocumentMutation>, ReplayError> {
    if envelope.encryption.is_some() {
        return Err(ReplayError::Encrypted {
            app: route.app_id.clone(),
        });
    }
    match DocumentFrame::decode(&envelope.payload).map_err(|error| malformed(route, error))? {
        DocumentFrame::Whole(mutation) => Ok(Some(mutation)),
        DocumentFrame::Segment(segment) => {
            match reassembler
                .accept(envelope, segment, now)
                .map_err(|error| malformed(route, error))?
            {
                Some(whole) => DocumentMutation::decode(&whole.payload)
                    .map(Some)
                    .map_err(|error| malformed(route, error)),
                None => Ok(None),
            }
        }
    }
}

/// The document of `route` as the recording started. A start snapshot is
/// that document; an end snapshot (a retroactive recording) already holds
/// the window's changes, so the start is it without them; a route with no
/// snapshot starts empty.
fn starting_document(
    trace: &SessionTrace,
    route: &Route,
    timeline: &[Decoded],
    index: usize,
) -> Result<Vec<u8>, ReplayError> {
    let Some(snapshot) = trace.snapshot_of(route) else {
        return Ok(AutoCommit::new().save());
    };
    let mut reassembler = DocumentReassembler::default();
    let now = Instant::now();
    let mut whole = None;
    for frame in &snapshot.frames {
        if let Some(mutation) = mutation_of(route, frame, &mut reassembler, now)? {
            whole = Some(mutation);
        }
    }
    let Some(DocumentMutation::Snapshot(bytes)) = whole else {
        return Err(malformed(route, "its snapshot is not a complete document"));
    };
    match snapshot.point {
        SnapshotPoint::Start => Ok(bytes),
        SnapshotPoint::End => {
            let mut end = AutoCommit::load(&bytes).map_err(|error| malformed(route, error))?;
            let mut recorded = std::collections::HashSet::new();
            for entry in timeline {
                if let Decoded::Mutation {
                    route: of,
                    mutation: DocumentMutation::Incremental(change),
                    ..
                } = entry
                    && *of == index
                {
                    recorded.extend(change_hashes(change));
                }
            }
            let before: Vec<automerge::Change> = end
                .get_changes(&[])
                .into_iter()
                .filter(|change| !recorded.contains(&change.hash()))
                .collect();
            let mut start = AutoCommit::new();
            start
                .apply_incoming(before)
                .map_err(|error| malformed(route, error))?;
            Ok(start.save())
        }
    }
}

/// Hashes of the changes in an incremental payload: a run of Automerge
/// change chunks, each `magic(4) checksum(4) type(1) uleb128(len) body`.
fn change_hashes(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    const HEADER: usize = 9;
    let mut hashes = Vec::new();
    let mut rest = bytes;
    while rest.len() > HEADER {
        let Some(after_type) = rest.get(HEADER..) else {
            break;
        };
        let mut length: u64 = 0;
        let mut width = 0_usize;
        for (index, byte) in after_type.iter().take(10).enumerate() {
            let shift = u32::try_from(index).unwrap_or(9).saturating_mul(7);
            length |= u64::from(byte & 0x7f).checked_shl(shift).unwrap_or(0);
            width = index.saturating_add(1);
            if byte & 0x80 == 0 {
                break;
            }
        }
        let Some(end) = usize::try_from(length)
            .ok()
            .and_then(|length| HEADER.checked_add(width)?.checked_add(length))
        else {
            break;
        };
        let Some(chunk) = rest.get(..end) else {
            break;
        };
        if let Ok(change) = automerge::Change::try_from(chunk) {
            hashes.push(change.hash());
        }
        rest = rest.get(end..).unwrap_or_default();
    }
    hashes
}

#[cfg(test)]
mod tests {
    use collab_types::ChannelClass;

    use super::*;
    use crate::shell::participant_keys::Participant;
    use crate::shell::participant_keys::tests::{Fixture, replay};

    /// An app joins its room route and the record route it shows. Whichever
    /// asks first, the recorded record goes to the record route, even when
    /// the replayed record's id differs from the recorded one.
    #[test]
    fn a_recorded_record_stands_for_the_record_route_not_the_room_route() {
        let doc = |record: &str| RouteDoc {
            recorded: Route::new("so.tilo.notes", "recorded-room", record),
            start: Vec::new(),
            doc: AutoCommit::new(),
            live: None,
            seq: 0,
        };
        let live = |record: &str| CrdtRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "replay-room".into(),
            record_id: record.into(),
        };
        let routes = vec![doc("derived:default-notebook")];
        assert_eq!(resolve(&routes, &live("")), None, "the room route");
        assert_eq!(resolve(&routes, &live("derived:default-notebook")), Some(0));
        assert_eq!(
            resolve(&routes, &live("uuid:another")),
            Some(0),
            "the only recorded record stands for the record route"
        );
        let with_room = vec![doc("derived:default-notebook"), doc("")];
        assert_eq!(resolve(&with_room, &live("")), Some(1));
    }

    /// What the peer says in an encrypted replay room is sealed for the
    /// route's channel, and a member of the room opens it to exactly the
    /// recorded change.
    #[test]
    fn a_replayed_change_travels_sealed_and_opens_as_recorded() {
        let fixture = Fixture::new(ChannelClass::Encrypted);
        let route = fixture.route();
        let mut doc = AutoCommit::new();
        let snapshot = DocumentMutation::Snapshot(doc.save());
        let mut seq = 0;
        let frames = frame(
            &session_key(),
            &fixture.keys.sealer(replay(), &fixture.room),
            &route,
            &mut seq,
            &snapshot,
        );
        assert!(!frames.is_empty());
        let reader = fixture
            .keys
            .opener(Participant::Person(fixture.owner.clone()));
        for op in &frames {
            assert!(
                op.encryption.is_some(),
                "an encrypted room carries no plaintext"
            );
            assert!(
                DocumentFrame::decode(&op.payload).is_err(),
                "the sealed payload is not the frame"
            );
            let opened = reader.opened(&route, op).expect("a member opens it");
            assert_eq!(
                DocumentFrame::decode(&opened.payload).ok(),
                Some(DocumentFrame::Whole(snapshot.clone()))
            );
        }
    }
}
